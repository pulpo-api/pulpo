//! OAuth 2.0: getting, caching and refreshing access tokens.
//!
//! Grants: authorization code (with PKCE, through the system browser and a loopback
//! redirect), client credentials and password. Tokens are cached per environment: the
//! access and refresh tokens in the OS keychain, split into parts because Windows caps a
//! credential at 2.5 KB and tokens from Azure AD and others are longer; what the app shows
//! about them (expiry, scope, number of parts) in `.pulpo/tokens.json`.

use crate::environment::{self, SecretStore};
use crate::http::{self, Form, FormField, FormKind, Header, HttpRequest, SendContext, Settings};
use crate::local;
use crate::HttpMethod;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TOKENS_FILE: &str = "tokens.json";
/// Characters per keychain entry: 2.5 KB holds 1280 UTF-16 characters, with room to spare.
const PART_CHARS: usize = 1000;
/// A token this close to its expiry counts as expired.
const EXPIRY_MARGIN_SECS: u64 = 30;
/// How long the browser login may take.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Grant {
    #[default]
    AuthorizationCode,
    ClientCredentials,
    Password,
}

impl Grant {
    pub fn as_str(self) -> &'static str {
        match self {
            Grant::AuthorizationCode => "authorizationCode",
            Grant::ClientCredentials => "clientCredentials",
            Grant::Password => "password",
        }
    }
}

/// Where the client id and secret go in token requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ClientAuth {
    /// `Authorization: Basic base64(id:secret)` (RFC 6749 recommends it).
    #[default]
    Header,
    /// `client_id` and `client_secret` in the form body.
    Body,
}

fn yes() -> bool {
    true
}

pub(crate) fn bearer() -> String {
    "Bearer".into()
}

/// The OAuth 2.0 settings of an `auth:` block. Values may hold `{{variables}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuth2 {
    #[serde(default)]
    pub grant: Grant,
    /// Authorization endpoint (authorization code only).
    #[serde(default)]
    pub auth_url: String,
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub scope: String,
    /// Resource owner credentials (password grant only).
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// Empty = `http://127.0.0.1:<free port>/callback`. Must be a loopback address.
    #[serde(default)]
    pub redirect_uri: String,
    /// PKCE (S256) for the authorization code grant.
    #[serde(default = "yes")]
    pub pkce: bool,
    #[serde(default)]
    pub client_auth: ClientAuth,
    /// Word before the token in the `Authorization` header.
    #[serde(default = "bearer")]
    pub header_prefix: String,
}

impl Default for OAuth2 {
    fn default() -> Self {
        OAuth2 {
            grant: Grant::default(),
            auth_url: String::new(),
            token_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scope: String::new(),
            username: String::new(),
            password: String::new(),
            redirect_uri: String::new(),
            pkce: true,
            client_auth: ClientAuth::default(),
            header_prefix: bearer(),
        }
    }
}

/// A cached access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Seconds since the epoch; `None` when the server did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub obtained_at: u64,
    #[serde(default)]
    pub grant: Grant,
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
}

impl Token {
    pub fn is_valid(&self, now: u64) -> bool {
        self.expires_at
            .is_none_or(|at| now + EXPIRY_MARGIN_SECS < at)
    }
}

/// What the app shows about a cached token; never the token itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub key: String,
    pub grant: Grant,
    pub token_url: String,
    pub client_id: String,
    pub scope: String,
    pub obtained_at: u64,
    pub expires_at: Option<u64>,
    pub valid: bool,
    pub has_refresh: bool,
    /// The first characters, to tell tokens apart.
    pub preview: String,
}

impl TokenInfo {
    pub fn of(key: &str, token: &Token, now: u64) -> TokenInfo {
        TokenInfo {
            key: key.to_string(),
            grant: token.grant,
            token_url: token.token_url.clone(),
            client_id: token.client_id.clone(),
            scope: token.scope.clone(),
            obtained_at: token.obtained_at,
            expires_at: token.expires_at,
            valid: token.is_valid(now),
            has_refresh: token.refresh_token.is_some(),
            preview: token.access_token.chars().take(10).collect(),
        }
    }
}

pub type Tokens = BTreeMap<String, Token>;

/// A token as `.pulpo/tokens.json` keeps it: everything but the secrets, which are in
/// `parts` keychain entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    #[serde(flatten)]
    meta: Token,
    parts: usize,
}

/// The secrets of a token, as kept (split) in the keychain.
#[derive(Serialize, Deserialize)]
struct Secrets {
    access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TokensFile {
    version: u32,
    environments: BTreeMap<String, BTreeMap<String, Stored>>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Identifies a token: same grant, endpoints, client, scope and user = same token.
pub fn cache_key(config: &OAuth2) -> String {
    let mut hash = Sha256::new();
    for part in [
        config.grant.as_str(),
        &config.token_url,
        &config.auth_url,
        &config.client_id,
        &config.scope,
        &config.username,
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    hash.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn file_path(root: &Path) -> PathBuf {
    root.join(local::LOCAL_DIR).join(TOKENS_FILE)
}

fn read(root: &Path) -> TokensFile {
    fs::read_to_string(file_path(root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(TokensFile {
            version: 1,
            environments: BTreeMap::new(),
        })
}

fn env_key(environment_id: Option<&str>) -> String {
    environment_id.unwrap_or(local::NO_ENVIRONMENT).to_string()
}

fn part_key(root: &Path, environment_id: Option<&str>, key: &str, part: usize) -> String {
    environment::secret_key(
        root,
        &env_key(environment_id),
        &format!("oauth:{key}:{part}"),
    )
}

fn read_secrets(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    key: &str,
    parts: usize,
) -> Option<Secrets> {
    let mut text = String::new();
    for part in 0..parts {
        text.push_str(
            &store
                .get(&part_key(root, environment_id, key, part))
                .ok()??,
        );
    }
    serde_json::from_str(&text).ok()
}

fn delete_parts(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    key: &str,
    parts: std::ops::Range<usize>,
) -> Result<(), String> {
    for part in parts {
        store.delete(&part_key(root, environment_id, key, part))?;
    }
    Ok(())
}

fn write_file(root: &Path, file: &TokensFile) -> Result<(), String> {
    let dir = local::ensure_local_dir(root).map_err(|e| e.to_string())?;
    let path = dir.join(TOKENS_FILE);
    let text =
        serde_json::to_string_pretty(file).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

/// Cached tokens of an environment (`None` = no environment selected). A token whose
/// secrets are not in the keychain (or the keychain does not answer) is left out.
pub fn load_tokens(root: &Path, environment_id: Option<&str>, store: &dyn SecretStore) -> Tokens {
    let stored = read(root)
        .environments
        .remove(&env_key(environment_id))
        .unwrap_or_default();
    stored
        .into_iter()
        .filter_map(|(key, stored)| {
            let secrets = read_secrets(root, environment_id, store, &key, stored.parts)?;
            let mut token = stored.meta;
            token.access_token = secrets.access;
            token.refresh_token = secrets.refresh;
            Some((key, token))
        })
        .collect()
}

/// Stores (`Some`) or forgets (`None`) one token: the secrets in the keychain, the rest in
/// `.pulpo/tokens.json`.
pub fn save_token(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    key: &str,
    token: Option<&Token>,
) -> Result<(), String> {
    let mut file = read(root);
    file.version = 1;
    let tokens = file
        .environments
        .entry(env_key(environment_id))
        .or_default();
    let old_parts = tokens.get(key).map_or(0, |stored| stored.parts);
    match token {
        Some(token) => {
            let secrets = serde_json::to_string(&Secrets {
                access: token.access_token.clone(),
                refresh: token.refresh_token.clone(),
            })
            .map_err(|e| e.to_string())?;
            let chars: Vec<char> = secrets.chars().collect();
            let parts: Vec<String> = chars.chunks(PART_CHARS).map(String::from_iter).collect();
            for (part, text) in parts.iter().enumerate() {
                store
                    .set(&part_key(root, environment_id, key, part), text)
                    .map_err(|e| format!("could not save the token in the keychain: {e}"))?;
            }
            delete_parts(root, environment_id, store, key, parts.len()..old_parts)?;
            let mut meta = token.clone();
            meta.access_token.clear();
            meta.refresh_token = None;
            tokens.insert(
                key.to_string(),
                Stored {
                    meta,
                    parts: parts.len(),
                },
            );
        }
        None => {
            delete_parts(root, environment_id, store, key, 0..old_parts)?;
            tokens.remove(key);
        }
    }
    file.environments.retain(|_, tokens| !tokens.is_empty());
    write_file(root, &file)
}

/// Forgets every token of an environment.
pub fn clear_tokens(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
) -> Result<usize, String> {
    let keys: Vec<String> = read(root)
        .environments
        .remove(&env_key(environment_id))
        .unwrap_or_default()
        .into_keys()
        .collect();
    for key in &keys {
        save_token(root, environment_id, store, key, None)?;
    }
    Ok(keys.len())
}

/// `Authorization: <prefix> <token>`, unless a header written by hand is already there.
pub fn apply_token(headers: &mut Vec<Header>, prefix: &str, token: &str) {
    let has = headers
        .iter()
        .any(|h| h.enabled && h.name.eq_ignore_ascii_case("authorization"));
    if !has {
        let value = if prefix.trim().is_empty() {
            token.to_string()
        } else {
            format!("{} {token}", prefix.trim())
        };
        headers.push(Header::new("Authorization", value));
    }
}

fn base64url(bytes: &[u8]) -> String {
    crate::import::base64(bytes)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

fn random_string() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// The S256 PKCE challenge of a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Sends a token request (form POST) and reads the token from the answer.
async fn token_request(
    config: &OAuth2,
    mut fields: Vec<(&str, String)>,
    settings: &Settings,
    context: &SendContext<'_>,
) -> Result<Token, String> {
    if config.token_url.trim().is_empty() {
        return Err("OAuth 2.0: the token URL is empty".into());
    }
    let mut request = HttpRequest::new(HttpMethod::Post, config.token_url.trim());
    request
        .headers
        .push(Header::new("Accept", "application/json"));
    request.settings = settings.clone();
    request.cookies = false;
    let public = config.client_secret.is_empty();
    if config.client_auth == ClientAuth::Header && !public {
        let encoded = crate::import::base64(
            format!("{}:{}", config.client_id, config.client_secret).as_bytes(),
        );
        request
            .headers
            .push(Header::new("Authorization", format!("Basic {encoded}")));
    } else {
        if !config.client_id.is_empty() {
            fields.push(("client_id", config.client_id.clone()));
        }
        if !public {
            fields.push(("client_secret", config.client_secret.clone()));
        }
    }
    request.form = Some(Form {
        kind: FormKind::Urlencoded,
        fields: fields
            .into_iter()
            .map(|(name, value)| FormField::text(name, value))
            .collect(),
    });
    let context = SendContext {
        root: context.root,
        cookies: None,
        client_pem: context.client_pem.clone(),
    };
    let response = http::send_with(&request, &context)
        .await
        .map_err(|e| format!("OAuth 2.0 token request: {e}"))?;
    let json: Option<serde_json::Value> = serde_json::from_str(&response.body).ok();
    let field = |name: &str| {
        json.as_ref()
            .and_then(|j| j.get(name))
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
    };
    if !(200..300).contains(&response.status) || field("error").is_some() {
        let reason = match (field("error"), field("error_description")) {
            (Some(error), Some(description)) => format!("{error}: {description}"),
            (Some(error), None) => error,
            _ => response.body.chars().take(300).collect(),
        };
        return Err(format!(
            "OAuth 2.0 token request: {} {} ({reason})",
            response.status, response.status_text
        ));
    }
    let Some(access_token) = field("access_token") else {
        return Err("OAuth 2.0 token request: the answer has no access_token".into());
    };
    let obtained_at = now();
    Ok(Token {
        access_token,
        token_type: field("token_type").unwrap_or_else(|| "Bearer".into()),
        refresh_token: field("refresh_token"),
        expires_at: field("expires_in")
            .and_then(|s| s.parse::<u64>().ok())
            .map(|secs| obtained_at + secs),
        scope: field("scope").unwrap_or_else(|| config.scope.clone()),
        obtained_at,
        grant: config.grant,
        token_url: config.token_url.clone(),
        client_id: config.client_id.clone(),
    })
}

fn with_scope<'a>(mut fields: Vec<(&'a str, String)>, config: &OAuth2) -> Vec<(&'a str, String)> {
    if !config.scope.trim().is_empty() {
        fields.push(("scope", config.scope.trim().to_string()));
    }
    fields
}

/// Client credentials and password grants: a token without a browser.
pub async fn fetch(
    config: &OAuth2,
    settings: &Settings,
    context: &SendContext<'_>,
) -> Result<Token, String> {
    let fields = match config.grant {
        Grant::ClientCredentials => vec![("grant_type", "client_credentials".into())],
        Grant::Password => vec![
            ("grant_type", "password".into()),
            ("username", config.username.clone()),
            ("password", config.password.clone()),
        ],
        Grant::AuthorizationCode => {
            return Err(
                "OAuth 2.0: the authorization code grant needs a login in the browser".into(),
            )
        }
    };
    token_request(config, with_scope(fields, config), settings, context).await
}

/// A new token from a refresh token. Keeps the refresh token when the server sends none.
pub async fn refresh(
    config: &OAuth2,
    refresh_token: &str,
    settings: &Settings,
    context: &SendContext<'_>,
) -> Result<Token, String> {
    let fields = vec![
        ("grant_type", "refresh_token".into()),
        ("refresh_token", refresh_token.to_string()),
    ];
    let mut token = token_request(config, with_scope(fields, config), settings, context).await?;
    if token.refresh_token.is_none() {
        token.refresh_token = Some(refresh_token.to_string());
    }
    Ok(token)
}

/// Where the browser comes back: host, port and path of a loopback redirect URI.
struct Loopback {
    bind: String,
    path: String,
    /// The redirect URI sent to the server (`{port}` is filled once bound).
    uri: String,
}

fn loopback(redirect_uri: &str) -> Result<Loopback, String> {
    let redirect_uri = redirect_uri.trim();
    if redirect_uri.is_empty() {
        return Ok(Loopback {
            bind: "127.0.0.1:0".into(),
            path: "/callback".into(),
            uri: "http://127.0.0.1:{port}/callback".into(),
        });
    }
    let url = reqwest::Url::parse(redirect_uri)
        .map_err(|e| format!("OAuth 2.0: redirect URI {redirect_uri}: {e}"))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1");
    if url.scheme() != "http" || !loopback {
        return Err(format!(
            "OAuth 2.0: the redirect URI must be http://127.0.0.1, http://localhost or \
             http://[::1] so Pulpo can catch the answer (got {redirect_uri}). Leave it empty \
             to use a free port, or register one such as http://127.0.0.1:8765/callback"
        ));
    }
    let port = url.port().unwrap_or(80);
    let ip = if host.contains(':') {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    Ok(Loopback {
        bind: format!("{ip}:{port}"),
        path: url.path().to_string(),
        uri: redirect_uri.to_string(),
    })
}

/// The URL the browser opens to log in.
pub fn authorization_url(
    config: &OAuth2,
    redirect_uri: &str,
    state: &str,
    challenge: Option<&str>,
) -> String {
    let auth_url = config.auth_url.trim();
    let mut params = vec![
        ("response_type", "code".to_string()),
        ("client_id", config.client_id.clone()),
        ("redirect_uri", redirect_uri.to_string()),
        ("state", state.to_string()),
    ];
    if !config.scope.trim().is_empty() {
        params.push(("scope", config.scope.trim().to_string()));
    }
    if let Some(challenge) = challenge {
        params.push(("code_challenge", challenge.to_string()));
        params.push(("code_challenge_method", "S256".into()));
    }
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", http::encode_query(v)))
        .collect();
    let sep = if auth_url.contains('?') { '&' } else { '?' };
    format!("{auth_url}{sep}{}", query.join("&"))
}

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>Pulpo</title>\
<body style=\"font-family:system-ui;padding:3em;text-align:center\">\
<h2>Logged in</h2><p>Pulpo has the token. You can close this tab.</p></body>";

fn page_error(message: &str) -> String {
    let safe = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>Pulpo</title>\
         <body style=\"font-family:system-ui;padding:3em;text-align:center\">\
         <h2>Login failed</h2><p>{safe}</p></body>"
    )
}

/// Authorization code grant: opens the login page with `open`, waits for the browser to
/// come back to the loopback redirect URI and exchanges the code for a token.
pub async fn authorize(
    config: &OAuth2,
    settings: &Settings,
    context: &SendContext<'_>,
    open: &(dyn Fn(&str) -> Result<(), String> + Sync),
    timeout: Duration,
) -> Result<Token, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if config.auth_url.trim().is_empty() {
        return Err("OAuth 2.0: the authorization URL is empty".into());
    }
    let target = loopback(&config.redirect_uri)?;
    let listener = tokio::net::TcpListener::bind(&target.bind)
        .await
        .map_err(|e| format!("OAuth 2.0: cannot listen on {}: {e}", target.bind))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = target.uri.replace("{port}", &port.to_string());
    let state = random_string();
    let verifier = random_string();
    let challenge = config.pkce.then(|| pkce_challenge(&verifier));
    open(&authorization_url(
        config,
        &redirect_uri,
        &state,
        challenge.as_deref(),
    ))?;

    let wait = async {
        loop {
            let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
            // The request line may come in several pieces: read until it is complete.
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            while !received.windows(2).any(|w| w == b"\r\n") && received.len() < 16 * 1024 {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => received.extend_from_slice(&buffer[..read]),
                }
            }
            let head = String::from_utf8_lossy(&received).into_owned();
            let target_path = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let (path, query) = target_path
                .split_once('?')
                .unwrap_or((target_path.as_str(), ""));
            let reply = |status: &str, body: &str| {
                format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            if path != target.path {
                let _ = socket
                    .write_all(reply("404 Not Found", "").as_bytes())
                    .await;
                continue;
            }
            let params = parse_query(query);
            let outcome = if let Some(error) = params.get("error") {
                let description = params
                    .get("error_description")
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                Err(format!("OAuth 2.0 login: {error}{description}"))
            } else if params.get("state") != Some(&state) {
                Err("OAuth 2.0 login: the state does not match, the answer is not ours".into())
            } else if let Some(code) = params.get("code") {
                Ok(code.clone())
            } else {
                Err("OAuth 2.0 login: the answer has no code".into())
            };
            let page = match &outcome {
                Ok(_) => PAGE_OK.to_string(),
                Err(e) => page_error(e),
            };
            let _ = socket.write_all(reply("200 OK", &page).as_bytes()).await;
            let _ = socket.shutdown().await;
            return outcome;
        }
    };
    let code = tokio::time::timeout(timeout, wait)
        .await
        .map_err(|_| "OAuth 2.0 login: the browser did not come back in time".to_string())??;

    let mut fields = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];
    if config.pkce {
        fields.push(("code_verifier", verifier));
    }
    token_request(config, fields, settings, context).await
}

/// Endpoints read from an OpenID Connect discovery document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub scopes_supported: Vec<String>,
}

/// Reads `<issuer>/.well-known/openid-configuration` (or the URL as given when it
/// already points to a `.well-known` document).
pub async fn discover(issuer: &str, settings: &Settings) -> Result<Discovery, String> {
    let issuer = issuer.trim();
    let url = if issuer.contains("/.well-known/") {
        issuer.to_string()
    } else {
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        )
    };
    let mut request = HttpRequest::new(HttpMethod::Get, &url);
    request
        .headers
        .push(Header::new("Accept", "application/json"));
    request.settings = settings.clone();
    request.cookies = false;
    let response = http::send(&request)
        .await
        .map_err(|e| format!("OpenID discovery: {e}"))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "OpenID discovery: {url} answered {} {}",
            response.status, response.status_text
        ));
    }
    let json: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|e| format!("OpenID discovery: {url} is not JSON: {e}"))?;
    let text = |name: &str| {
        json.get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let discovery = Discovery {
        authorization_endpoint: text("authorization_endpoint"),
        token_endpoint: text("token_endpoint"),
        scopes_supported: json
            .get("scopes_supported")
            .and_then(|s| s.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };
    if discovery.token_endpoint.is_empty() {
        return Err(format!("OpenID discovery: {url} has no token_endpoint"));
    }
    Ok(discovery)
}

/// Opens a URL in the system browser.
pub fn open_browser(url: &str) -> Result<(), String> {
    let result = if cfg!(target_os = "windows") {
        // rundll32 takes the URL as is; `cmd /c start` would split it at every `&`.
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    result
        .map(|_| ())
        .map_err(|e| format!("could not open the browser: {e}"))
}

/// What happened when a request needed a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obtained {
    /// A valid token from the cache.
    Cached,
    Refreshed,
    Fetched,
}

/// The token a request uses: a valid cached one, else refreshed, else fetched (client
/// credentials and password). The authorization code grant needs a login first
/// ([`authorize`], "Get new access token" in the app or `pulpo auth login`).
pub async fn token_for(
    config: &OAuth2,
    tokens: &Tokens,
    settings: &Settings,
    context: &SendContext<'_>,
) -> Result<(Token, Obtained), String> {
    let key = cache_key(config);
    let cached = tokens.get(&key);
    if let Some(token) = cached {
        if token.is_valid(now()) {
            return Ok((token.clone(), Obtained::Cached));
        }
    }
    let mut refresh_error = None;
    if let Some(refresh_token) = cached.and_then(|t| t.refresh_token.as_deref()) {
        match refresh(config, refresh_token, settings, context).await {
            Ok(token) => return Ok((token, Obtained::Refreshed)),
            Err(e) => refresh_error = Some(e),
        }
    }
    match config.grant {
        Grant::AuthorizationCode => {
            let why = match (cached, refresh_error) {
                (None, _) => "there is no token yet".to_string(),
                (Some(_), Some(e)) => format!("the token expired and could not be refreshed ({e})"),
                (Some(_), None) => "the token expired".to_string(),
            };
            Err(format!(
                "OAuth 2.0: {why}. Log in with \"Get new access token\" in the Auth tab \
                 or `pulpo auth login <request>`"
            ))
        }
        _ => fetch(config, settings, context)
            .await
            .map(|token| (token, Obtained::Fetched)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn same_settings_share_a_token() {
        let a = OAuth2 {
            grant: Grant::ClientCredentials,
            token_url: "https://id.example/token".into(),
            client_id: "app".into(),
            ..Default::default()
        };
        let mut b = a.clone();
        b.client_secret = "other secret".into();
        assert_eq!(
            cache_key(&a),
            cache_key(&b),
            "the secret is not part of the key"
        );
        b.scope = "read".into();
        assert_ne!(cache_key(&a), cache_key(&b));
    }

    #[test]
    fn decodes_the_redirect_query() {
        let params = parse_query("code=a%2Fb+c&state=xyz&empty");
        assert_eq!(params["code"], "a/b c");
        assert_eq!(params["state"], "xyz");
        assert_eq!(params["empty"], "");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn only_accepts_loopback_redirects() {
        assert!(loopback("").is_ok());
        let l = loopback("http://localhost:8765/cb").unwrap();
        assert_eq!(
            (l.bind.as_str(), l.path.as_str()),
            ("127.0.0.1:8765", "/cb")
        );
        assert!(loopback("https://oauth.pstmn.io/v1/callback").is_err());
    }

    #[test]
    fn builds_the_login_url() {
        let config = OAuth2 {
            auth_url: "https://id.example/authorize?prompt=login".into(),
            client_id: "my app".into(),
            scope: "openid email".into(),
            ..Default::default()
        };
        let url = authorization_url(&config, "http://127.0.0.1:1/callback", "s1", Some("ch"));
        assert_eq!(
            url,
            "https://id.example/authorize?prompt=login&response_type=code&client_id=my%20app\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcallback&state=s1&scope=openid%20email\
             &code_challenge=ch&code_challenge_method=S256"
        );
    }

    #[test]
    fn keeps_token_secrets_in_the_keychain_split_into_parts() {
        let dir = tempfile::tempdir().unwrap();
        let store = environment::MemoryStore::default();
        let long = "x".repeat(2 * PART_CHARS + 10);
        let token = Token {
            access_token: long.clone(),
            token_type: "Bearer".into(),
            refresh_token: Some("refresh-me".into()),
            expires_at: Some(now() + 3600),
            scope: String::new(),
            obtained_at: now(),
            grant: Grant::ClientCredentials,
            token_url: String::new(),
            client_id: String::new(),
        };
        save_token(dir.path(), Some("dev"), &store, "k1", Some(&token)).unwrap();
        assert_eq!(load_tokens(dir.path(), Some("dev"), &store)["k1"], token);
        assert!(load_tokens(dir.path(), None, &store).is_empty());
        assert!(dir.path().join(".pulpo/.gitignore").is_file());
        let file = fs::read_to_string(dir.path().join(".pulpo/tokens.json")).unwrap();
        assert!(
            !file.contains("xxxx") && !file.contains("refresh-me"),
            "{file}"
        );
        assert!(file.contains("\"parts\": 3"), "{file}");
        assert!(store
            .get(&part_key(dir.path(), Some("dev"), "k1", 2))
            .unwrap()
            .is_some());

        // A shorter token frees the parts it no longer needs.
        let short = Token {
            access_token: "abc".into(),
            ..token.clone()
        };
        save_token(dir.path(), Some("dev"), &store, "k1", Some(&short)).unwrap();
        assert_eq!(load_tokens(dir.path(), Some("dev"), &store)["k1"], short);
        assert!(store
            .get(&part_key(dir.path(), Some("dev"), "k1", 1))
            .unwrap()
            .is_none());

        // Without its secrets a token is not cached.
        store
            .delete(&part_key(dir.path(), Some("dev"), "k1", 0))
            .unwrap();
        assert!(load_tokens(dir.path(), Some("dev"), &store).is_empty());

        assert_eq!(clear_tokens(dir.path(), Some("dev"), &store).unwrap(), 1);
        assert!(!fs::read_to_string(dir.path().join(".pulpo/tokens.json"))
            .unwrap()
            .contains("k1"));
    }

    #[test]
    fn a_token_about_to_expire_is_not_valid() {
        let mut token = Token {
            access_token: "t".into(),
            token_type: String::new(),
            refresh_token: None,
            expires_at: None,
            scope: String::new(),
            obtained_at: 0,
            grant: Grant::Password,
            token_url: String::new(),
            client_id: String::new(),
        };
        assert!(token.is_valid(1000), "no expiry = valid");
        token.expires_at = Some(1010);
        assert!(!token.is_valid(1000));
        token.expires_at = Some(2000);
        assert!(token.is_valid(1000));
    }

    #[test]
    fn round_trips_through_the_request_file() {
        use crate::collection::{init_workspace, load_request, save_request, RequestEntry};
        use crate::http::{Auth, HttpRequest};
        let dir = tempfile::tempdir().unwrap();
        init_workspace(dir.path(), "Demo").unwrap();
        let mut request = HttpRequest::new(HttpMethod::Get, "https://api.example/me");
        request.auth = Some(Auth::OAuth2(OAuth2 {
            grant: Grant::Password,
            token_url: "{{idp}}/token".into(),
            client_id: "{{clientId}}".into(),
            client_secret: "{{clientSecret}}".into(),
            username: "ana".into(),
            password: "{{password}}".into(),
            scope: "openid api".into(),
            pkce: false,
            client_auth: ClientAuth::Body,
            header_prefix: "Token".into(),
            ..Default::default()
        }));
        let entry = RequestEntry {
            id: "01-me.pulpo.yaml".into(),
            name: "me".into(),
            protocol: crate::Protocol::Http,
            request,
            scripts: Default::default(),
        };
        save_request(dir.path(), &entry).unwrap();
        let text = fs::read_to_string(dir.path().join("01-me.pulpo.yaml")).unwrap();
        assert!(
            text.contains("  type: \"oauth2\"\n  grant: \"password\"\n"),
            "{text}"
        );
        assert!(
            !text.contains("authUrl"),
            "only what the grant uses: {text}"
        );
        assert_eq!(
            load_request(dir.path(), &entry.id).unwrap().request.auth,
            entry.request.auth
        );
    }
}
