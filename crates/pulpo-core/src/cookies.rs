//! Cookie jars: one per environment, kept in `.pulpo/cookies.json` (local, never in git).
//!
//! A small RFC 6265 subset: `Set-Cookie` with Domain, Path, Expires, Max-Age, Secure,
//! HttpOnly and SameSite. A server can only set cookies for its own domain (or a parent
//! that is not a top-level domain), and `Secure` cookies only travel over https (or to
//! localhost). The jar plugs into reqwest, so cookies set during redirects are kept too.

use crate::collection::CollectionError;
use crate::local::{ensure_local_dir, LOCAL_DIR, NO_ENVIRONMENT};
use reqwest::header::HeaderValue;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const COOKIES_FILE: &str = "cookies.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lowercase, without a leading dot.
    pub domain: String,
    /// True when the server sent no Domain: only that exact host gets it back.
    #[serde(default)]
    pub host_only: bool,
    #[serde(default = "root_path")]
    pub path: String,
    /// Unix seconds; `None` = session cookie (kept until the jar is cleared).
    #[serde(default)]
    pub expires: Option<i64>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub same_site: Option<String>,
}

fn root_path() -> String {
    "/".into()
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn is_ip(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok()
}

fn is_local(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// `host` is `domain` or a subdomain of it.
fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.ends_with(domain)
            && host[..host.len() - domain.len()].ends_with('.')
            && !is_ip(host))
}

/// The path a cookie gets when the server sends none: the request path up to its last `/`.
fn default_path(url: &Url) -> String {
    let path = url.path();
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/') || request_path[cookie_path.len()..].starts_with('/')))
}

/// Parses a cookie date. Besides the HTTP formats, accepts the common `21-Oct-2015` form.
fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    let attempts = [text.to_string(), text.replace('-', " ")];
    attempts.iter().find_map(|t| {
        httpdate::parse_http_date(t)
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .or_else(|| {
                // `Wed, 21 Oct 2015 07:28:00 GMT` written with dashes needs its comma back.
                let (day, rest) = t.split_once(' ')?;
                let fixed = format!("{}, {}", day.trim_end_matches(','), rest.trim());
                httpdate::parse_http_date(&fixed)
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
            })
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jar {
    pub cookies: Vec<Cookie>,
}

impl Jar {
    /// Applies one `Set-Cookie` header received from `url`. Returns false when it is
    /// rejected (malformed, or for a domain the server does not own).
    pub fn store(&mut self, set_cookie: &str, url: &Url, now: i64) -> bool {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        let mut parts = set_cookie.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
            return false;
        };
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let mut cookie = Cookie {
            name: name.to_string(),
            value: value.trim().trim_matches('"').to_string(),
            domain: host.clone(),
            host_only: true,
            path: default_path(url),
            expires: None,
            secure: false,
            http_only: false,
            same_site: None,
        };
        let mut max_age: Option<i64> = None;
        for attribute in parts {
            let (key, val) = attribute.split_once('=').unwrap_or((attribute, ""));
            let val = val.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "domain" => {
                    let domain = val.trim_start_matches('.').to_ascii_lowercase();
                    if domain.is_empty() {
                        continue;
                    }
                    // A server only sets cookies for itself or a parent that is not a TLD.
                    if !domain_matches(&host, &domain) || (domain != host && !domain.contains('.'))
                    {
                        return false;
                    }
                    cookie.host_only = domain == host;
                    cookie.domain = domain;
                }
                "path" if val.starts_with('/') => cookie.path = val.to_string(),
                "expires" => {
                    if let Some(time) = parse_date(val) {
                        cookie.expires = Some(time);
                    }
                }
                "max-age" => {
                    if let Ok(seconds) = val.parse::<i64>() {
                        max_age = Some(seconds);
                    }
                }
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                "samesite" if !val.is_empty() => cookie.same_site = Some(val.to_string()),
                _ => {}
            }
        }
        if let Some(seconds) = max_age {
            cookie.expires = Some(if seconds <= 0 { 0 } else { now + seconds });
        }
        self.cookies.retain(|c| {
            !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        });
        // An expiry in the past is how servers delete cookies.
        if cookie.expires.is_none_or(|t| t > now) {
            self.cookies.push(cookie);
        }
        true
    }

    /// Cookies to send to `url`, most specific path first.
    pub fn matching(&self, url: &Url, now: i64) -> Vec<&Cookie> {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return Vec::new();
        };
        let secure_ok = url.scheme() == "https" || is_local(&host);
        let mut found: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.expires.is_none_or(|t| t > now))
            .filter(|c| {
                if c.host_only {
                    c.domain == host
                } else {
                    domain_matches(&host, &c.domain)
                }
            })
            .filter(|c| path_matches(url.path(), &c.path))
            .filter(|c| !c.secure || secure_ok)
            .collect();
        found.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        found
    }

    /// The `Cookie` header for `url`, if any cookie matches.
    pub fn header_for(&self, url: &Url, now: i64) -> Option<String> {
        let cookies = self.matching(url, now);
        (!cookies.is_empty()).then(|| {
            cookies
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    pub fn purge_expired(&mut self, now: i64) {
        self.cookies.retain(|c| c.expires.is_none_or(|t| t > now));
    }
}

/// A jar shared by the requests of one run (and reqwest, through [`reqwest::cookie::CookieStore`]).
#[derive(Debug, Default)]
pub struct SharedJar {
    jar: Mutex<Jar>,
    changed: Mutex<bool>,
}

impl SharedJar {
    pub fn new(jar: Jar) -> Self {
        SharedJar {
            jar: Mutex::new(jar),
            changed: Mutex::new(false),
        }
    }

    pub fn snapshot(&self) -> Jar {
        self.jar.lock().map(|j| j.clone()).unwrap_or_default()
    }

    pub fn changed(&self) -> bool {
        self.changed.lock().map(|c| *c).unwrap_or(false)
    }

    /// `name=value` pairs that would be sent to `url`.
    pub fn pairs_for(&self, url: &str) -> Vec<(String, String)> {
        let Ok(url) = Url::parse(url) else {
            return Vec::new();
        };
        self.jar
            .lock()
            .map(|jar| {
                jar.matching(&url, now())
                    .into_iter()
                    .map(|c| (c.name.clone(), c.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl reqwest::cookie::CookieStore for SharedJar {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let Ok(mut jar) = self.jar.lock() else { return };
        let now = now();
        let mut any = false;
        for header in cookie_headers {
            if let Ok(text) = header.to_str() {
                any |= jar.store(text, url, now);
            }
        }
        if any {
            if let Ok(mut changed) = self.changed.lock() {
                *changed = true;
            }
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let jar = self.jar.lock().ok()?;
        jar.header_for(url, now())
            .and_then(|header| HeaderValue::from_str(&header).ok())
    }
}

// ---------------------------------------------------------------- files

#[derive(Debug, Default, Serialize, Deserialize)]
struct CookiesFile {
    version: u32,
    environments: BTreeMap<String, Vec<Cookie>>,
}

fn file_path(root: &Path) -> PathBuf {
    root.join(LOCAL_DIR).join(COOKIES_FILE)
}

fn read(root: &Path) -> CookiesFile {
    fs::read_to_string(file_path(root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(CookiesFile {
            version: 1,
            environments: BTreeMap::new(),
        })
}

/// The jar of an environment (`None` = no environment selected), without expired cookies.
pub fn load_jar(root: &Path, environment_id: Option<&str>) -> Jar {
    let mut jar = Jar {
        cookies: read(root)
            .environments
            .remove(environment_id.unwrap_or(NO_ENVIRONMENT))
            .unwrap_or_default(),
    };
    jar.purge_expired(now());
    jar
}

/// Replaces the jar of an environment (an empty jar removes it).
pub fn save_jar(
    root: &Path,
    environment_id: Option<&str>,
    jar: &Jar,
) -> Result<(), CollectionError> {
    let mut file = read(root);
    let key = environment_id.unwrap_or(NO_ENVIRONMENT).to_string();
    if jar.cookies.is_empty() {
        if file.environments.remove(&key).is_none() {
            return Ok(());
        }
    } else {
        file.environments.insert(key, jar.cookies.clone());
    }
    ensure_local_dir(root)?;
    let path = file_path(root);
    let text =
        serde_json::to_string_pretty(&file).map_err(|e| CollectionError::Io(e.to_string()))?;
    fs::write(&path, text + "\n")
        .map_err(|e| CollectionError::Io(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn stores_and_sends_cookies_by_domain_and_path() {
        let mut jar = Jar::default();
        let from = url("https://api.shop.test/v1/login");
        assert!(jar.store("session=abc; Path=/; HttpOnly; Secure", &from, NOW));
        assert!(jar.store("pref=dark; Domain=.shop.test; Path=/", &from, NOW));
        assert!(jar.store("scoped=1", &from, NOW), "default path is /v1");

        assert_eq!(
            jar.header_for(&url("https://api.shop.test/v1/orders"), NOW)
                .unwrap(),
            "scoped=1; session=abc; pref=dark"
        );
        assert_eq!(
            jar.header_for(&url("https://www.shop.test/"), NOW).unwrap(),
            "pref=dark",
            "Domain cookies reach subdomains, host-only ones do not"
        );
        assert_eq!(
            jar.header_for(&url("http://api.shop.test/v1"), NOW)
                .unwrap(),
            "scoped=1; pref=dark",
            "Secure cookies need https"
        );
        assert_eq!(
            jar.header_for(&url("https://api.shop.test/v10"), NOW)
                .unwrap(),
            "session=abc; pref=dark"
        );
        assert_eq!(jar.header_for(&url("https://other.test/"), NOW), None);
    }

    #[test]
    fn rejects_cookies_for_other_domains() {
        let mut jar = Jar::default();
        let from = url("https://api.shop.test/");
        assert!(!jar.store("a=1; Domain=evil.test", &from, NOW));
        assert!(
            !jar.store("a=1; Domain=test", &from, NOW),
            "no TLD-wide cookies"
        );
        assert!(!jar.store("=1", &from, NOW));
        assert!(!jar.store("novalue", &from, NOW));
        assert!(jar.cookies.is_empty());
        assert!(jar.store("a=1; Domain=localhost", &url("http://localhost:8080/"), NOW));
    }

    #[test]
    fn expires_replaces_and_deletes() {
        let mut jar = Jar::default();
        let from = url("https://shop.test/");
        jar.store("a=1; Max-Age=60", &from, NOW);
        assert_eq!(jar.cookies[0].expires, Some(NOW + 60));
        jar.store("a=2", &from, NOW);
        assert_eq!(jar.cookies.len(), 1);
        assert_eq!(jar.cookies[0].value, "2");
        jar.store("a=; Max-Age=0", &from, NOW);
        assert!(jar.cookies.is_empty(), "Max-Age=0 deletes");

        jar.store("b=1; Expires=Wed, 21 Oct 2015 07:28:00 GMT", &from, NOW);
        assert!(jar.cookies.is_empty(), "a past date deletes");
        jar.store("c=1; Expires=Thu, 01-Jan-2032 00:00:00 GMT", &from, NOW);
        assert_eq!(jar.cookies[0].expires, Some(1_956_528_000));
        assert_eq!(
            jar.header_for(&url("https://shop.test/"), 1_956_528_001),
            None
        );
    }

    #[test]
    fn keeps_a_jar_per_environment_in_pulpo_dir() {
        let ws = tempfile::tempdir().unwrap();
        let mut jar = Jar::default();
        jar.store("a=1", &url("https://shop.test/"), now());
        save_jar(ws.path(), Some("environments/local.yaml"), &jar).unwrap();

        assert_eq!(load_jar(ws.path(), Some("environments/local.yaml")), jar);
        assert!(load_jar(ws.path(), None).cookies.is_empty());
        assert!(ws.path().join(".pulpo/.gitignore").exists());

        save_jar(ws.path(), Some("environments/local.yaml"), &Jar::default()).unwrap();
        assert!(load_jar(ws.path(), Some("environments/local.yaml"))
            .cookies
            .is_empty());
    }
}
