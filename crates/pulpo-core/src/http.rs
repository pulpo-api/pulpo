//! HTTP engine: sends a request and measures the response.

use crate::cookies::SharedJar;
use crate::HttpMethod;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_REDIRECTS: usize = 10;
const USER_AGENT: &str = concat!("Pulpo/", env!("CARGO_PKG_VERSION"));

/// A header as edited in the UI. Disabled headers are kept but not sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Header {
            name: name.into(),
            value: value.into(),
            enabled: true,
        }
    }
}

/// How a request is sent. Every field is optional: unset fields come from the nearest
/// folder that sets them, then the workspace, then the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// Default 30 000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Default true. False accepts invalid or self-signed server certificates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_ssl: Option<bool>,
    /// Default true (up to 10 redirects).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_redirects: Option<bool>,
    /// `system` (HTTP_PROXY and friends, the default), `none`, or a proxy URL
    /// (`http://`, `https://` or `socks5://`, credentials as `{{variables}}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

impl Settings {
    /// These settings over `parent`'s: fields set here win.
    pub fn over(&self, parent: &Settings) -> Settings {
        Settings {
            timeout_ms: self.timeout_ms.or(parent.timeout_ms),
            verify_ssl: self.verify_ssl.or(parent.verify_ssl),
            follow_redirects: self.follow_redirects.or(parent.follow_redirects),
            proxy: self.proxy.clone().or_else(|| parent.proxy.clone()),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Settings::default()
    }
}

/// Which kind of form a request sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormKind {
    /// `application/x-www-form-urlencoded`
    Urlencoded,
    /// `multipart/form-data`, with text and file fields.
    Multipart,
}

/// A form field. When `file` is true, `value` is a path relative to the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormField {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub file: bool,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

impl FormField {
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        FormField {
            name: name.into(),
            value: value.into(),
            file: false,
            enabled: true,
        }
    }

    pub fn file(name: impl Into<String>, path: impl Into<String>) -> Self {
        FormField {
            file: true,
            ..FormField::text(name, path)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    pub kind: FormKind,
    #[serde(default)]
    pub fields: Vec<FormField>,
}

/// A name and a value: a disabled query param or a path param.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

impl Param {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Param {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Where an API key goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyLocation {
    #[default]
    Header,
    Query,
}

/// How a request authenticates. In files and requests, `None` means "inherit from the folder".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Auth {
    /// No authentication; also stops inheritance from parent folders.
    None,
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        #[serde(default)]
        password: String,
    },
    ApiKey {
        key: String,
        value: String,
        #[serde(default, rename = "in")]
        location: ApiKeyLocation,
    },
}

impl Auth {
    pub fn label(&self) -> &'static str {
        match self {
            Auth::None => "No auth",
            Auth::Bearer { .. } => "Bearer token",
            Auth::Basic { .. } => "Basic",
            Auth::ApiKey { .. } => "API key",
        }
    }
}

/// `application/x-www-form-urlencoded` encoding: like the query, but spaces are `+`.
fn encode_form(value: &str) -> String {
    encode_query(value).replace("%20", "+")
}

fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Adds the auth to a (resolved) request. A header written by hand wins over the auth.
pub fn apply_auth(url: &mut String, headers: &mut Vec<Header>, auth: &Auth) {
    let has = |headers: &Vec<Header>, name: &str| {
        headers
            .iter()
            .any(|h| h.enabled && h.name.eq_ignore_ascii_case(name))
    };
    match auth {
        Auth::None => {}
        Auth::Bearer { token } => {
            if !has(headers, "authorization") {
                headers.push(Header::new("Authorization", format!("Bearer {token}")));
            }
        }
        Auth::Basic { username, password } => {
            if !has(headers, "authorization") {
                let encoded = crate::import::base64(format!("{username}:{password}").as_bytes());
                headers.push(Header::new("Authorization", format!("Basic {encoded}")));
            }
        }
        Auth::ApiKey {
            key,
            value,
            location,
        } => match location {
            ApiKeyLocation::Header => {
                if !has(headers, key) {
                    headers.push(Header::new(key.clone(), value.clone()));
                }
            }
            ApiKeyLocation::Query => {
                let (base, hash) = match url.find('#') {
                    Some(i) => (url[..i].to_string(), url[i..].to_string()),
                    None => (url.clone(), String::new()),
                };
                let sep = if base.contains('?') { '&' } else { '?' };
                *url = format!(
                    "{base}{sep}{}={}{hash}",
                    encode_query(key),
                    encode_query(value)
                );
            }
        },
    }
}

/// An HTTP request ready to be sent. Variables must already be resolved.
///
/// Enabled query params live in `url`; disabled ones are kept in `disabled_params` so the
/// URL stays clean. `:name` path segments are replaced by `path_params` when sending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    /// Raw body (JSON, XML, GraphQL, text). Ignored when `form` or `body_file` is set.
    #[serde(default)]
    pub body: Option<String>,
    /// A form body; wins over `body_file` and `body`.
    #[serde(default)]
    pub form: Option<Form>,
    /// A file sent as the whole body, relative to the workspace. Wins over `body`.
    #[serde(default)]
    pub body_file: Option<String>,
    /// Own settings; unset fields are inherited (resolved before sending).
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub disabled_params: Vec<Param>,
    #[serde(default)]
    pub path_params: Vec<Param>,
    /// `None` = inherit from the folder (resolved before sending).
    #[serde(default)]
    pub auth: Option<Auth>,
    /// Send and store cookies with the environment's jar.
    #[serde(default = "enabled_by_default")]
    pub cookies: bool,
}

impl HttpRequest {
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        HttpRequest {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            form: None,
            body_file: None,
            settings: Settings::default(),
            disabled_params: Vec::new(),
            path_params: Vec::new(),
            auth: None,
            cookies: true,
        }
    }
}

/// What a send may use besides the request: the workspace (for uploads) and a cookie jar.
#[derive(Debug, Default, Clone)]
pub struct SendContext<'a> {
    pub root: Option<&'a Path>,
    pub cookies: Option<Arc<SharedJar>>,
    /// Client certificate (mTLS) as PEM: certificate chain and private key.
    pub client_pem: Option<Vec<u8>>,
}

/// Finds a file a request sends. Paths are relative to the workspace and may not leave
/// it, so a shared collection cannot read and upload other files of your machine.
pub fn workspace_file(root: Option<&Path>, relative: &str) -> Result<PathBuf, SendError> {
    let Some(root) = root else {
        return Err(SendError::File(format!(
            "{relative}: files can only be sent from a workspace"
        )));
    };
    let path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(SendError::File(format!(
            "{relative}: use a path inside the workspace, like files/photo.png"
        )));
    }
    let full = root.join(path);
    let real = full
        .canonicalize()
        .map_err(|e| SendError::File(format!("{relative}: {e}")))?;
    let real_root = root
        .canonicalize()
        .map_err(|e| SendError::File(format!("{}: {e}", root.display())))?;
    if !real.starts_with(&real_root) {
        return Err(SendError::File(format!(
            "{relative}: links out of the workspace are not followed"
        )));
    }
    if !real.is_file() {
        return Err(SendError::File(format!("{relative}: not a file")));
    }
    Ok(real)
}

/// A content type from the file extension, for uploads.
pub fn mime_for(path: &str) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "json" => "application/json",
        "xml" => "application/xml",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

fn read_file(root: Option<&Path>, relative: &str) -> Result<Vec<u8>, SendError> {
    let path = workspace_file(root, relative)?;
    std::fs::read(&path).map_err(|e| SendError::File(format!("{relative}: {e}")))
}

/// Where the path starts: after `scheme://host[:port]`, or at the start for relative URLs.
fn path_start(url: &str) -> usize {
    match url.find("://") {
        Some(i) => url[i + 3..]
            .find('/')
            .map(|p| i + 3 + p)
            .unwrap_or(url.len()),
        None => 0,
    }
}

/// Names of the `:name` segments in the URL path (not the port), in order.
pub fn path_param_names(url: &str) -> Vec<String> {
    let start = path_start(url);
    let path = &url[start..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.split('/')
        .filter_map(|segment| segment.strip_prefix(':'))
        .map(|name| {
            name.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

fn encode_segment(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Replaces `:name` path segments with their (URL-encoded) values. Unknown names stay.
pub fn apply_path_params(url: &str, params: &[Param]) -> String {
    if params.is_empty() {
        return url.to_string();
    }
    let start = path_start(url);
    let (head, rest) = url.split_at(start);
    let split = rest.find(['?', '#']).unwrap_or(rest.len());
    let (path, tail) = rest.split_at(split);
    let replaced: Vec<String> = path
        .split('/')
        .map(|segment| {
            let Some(name_and_more) = segment.strip_prefix(':') else {
                return segment.to_string();
            };
            let name: String = name_and_more
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            match params.iter().find(|p| p.name == name) {
                Some(p) if !name.is_empty() => format!(
                    "{}{}",
                    encode_segment(&p.value),
                    &name_and_more[name.len()..]
                ),
                _ => segment.to_string(),
            }
        })
        .collect();
    format!("{head}{}{tail}", replaced.join("/"))
}

/// What came back, with timing and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub headers: Vec<Header>,
    pub body: String,
    pub body_is_text: bool,
    pub size_bytes: usize,
    pub duration_ms: u64,
}

/// Why a request could not be completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum SendError {
    InvalidUrl(String),
    InvalidHeader(String),
    Timeout(String),
    Connect(String),
    Unresolved(String),
    /// A file to upload is missing or outside the workspace.
    File(String),
    Other(String),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::InvalidUrl(m) => write!(f, "invalid URL: {m}"),
            SendError::InvalidHeader(m) => write!(f, "invalid header: {m}"),
            SendError::Timeout(m) => write!(f, "timed out: {m}"),
            SendError::Connect(m) => write!(f, "could not connect: {m}"),
            SendError::Unresolved(m) => write!(f, "unresolved variables: {m}"),
            SendError::File(m) => write!(f, "file: {m}"),
            SendError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SendError {}

impl From<reqwest::Error> for SendError {
    fn from(err: reqwest::Error) -> Self {
        let message = error_chain(&err);
        if err.is_timeout() {
            SendError::Timeout(message)
        } else if err.is_connect() {
            SendError::Connect(message)
        } else if err.is_builder() {
            SendError::InvalidUrl(message)
        } else {
            SendError::Other(message)
        }
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn to_reqwest_method(method: HttpMethod) -> reqwest::Method {
    match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Delete => reqwest::Method::DELETE,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Options => reqwest::Method::OPTIONS,
    }
}

/// Sends the request and returns the response, whatever its status code.
/// Requests that upload files need [`send_in`].
pub async fn send(request: &HttpRequest) -> Result<HttpResponse, SendError> {
    send_in(request, None).await
}

/// Like [`send`], reading uploaded files from the workspace at `root`.
pub async fn send_in(
    request: &HttpRequest,
    root: Option<&Path>,
) -> Result<HttpResponse, SendError> {
    send_with(
        request,
        &SendContext {
            root,
            ..Default::default()
        },
    )
    .await
}

/// Sends with a workspace and a cookie jar. A `Cookie` header written by hand wins over
/// the jar; cookies set by responses (redirects included) go into it.
pub async fn send_with(
    request: &HttpRequest,
    context: &SendContext<'_>,
) -> Result<HttpResponse, SendError> {
    let root = context.root;
    let mut raw_url = apply_path_params(request.url.trim(), &request.path_params);
    let mut headers = request.headers.clone();
    if let Some(auth) = &request.auth {
        apply_auth(&mut raw_url, &mut headers, auth);
    }
    let url = reqwest::Url::parse(&raw_url)
        .map_err(|e| SendError::InvalidUrl(format!("{raw_url}: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SendError::InvalidUrl(format!(
            "unsupported scheme '{}', use http or https",
            url.scheme()
        )));
    }

    let settings = &request.settings;
    let timeout = Duration::from_millis(settings.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let mut client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .redirect(if settings.follow_redirects == Some(false) {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::limited(MAX_REDIRECTS)
        });
    if settings.verify_ssl == Some(false) {
        client = client.danger_accept_invalid_certs(true);
    }
    match settings.proxy.as_deref().map(str::trim) {
        None | Some("") | Some("system") => {}
        Some("none") => client = client.no_proxy(),
        Some(url) => {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| SendError::InvalidUrl(format!("proxy {url}: {e}")))?;
            client = client.proxy(proxy);
        }
    }
    if let Some(pem) = &context.client_pem {
        let identity = reqwest::Identity::from_pem(pem)
            .map_err(|e| SendError::File(format!("client certificate: {e}")))?;
        client = client.identity(identity);
    }
    if let (true, Some(jar)) = (request.cookies, &context.cookies) {
        client = client.cookie_provider(jar.clone());
    }
    let client = client.build()?;

    let has_header = |name: &str| {
        headers
            .iter()
            .any(|h| h.enabled && h.name.trim().eq_ignore_ascii_case(name))
    };
    let multipart = matches!(&request.form, Some(f) if f.kind == FormKind::Multipart);
    let mut builder = client.request(to_reqwest_method(request.method), url);
    for header in headers.iter().filter(|h| {
        // The multipart boundary lives in Content-Type, so Pulpo always writes that one.
        h.enabled
            && !h.name.trim().is_empty()
            && !(multipart && h.name.trim().eq_ignore_ascii_case("content-type"))
    }) {
        let name = reqwest::header::HeaderName::from_bytes(header.name.trim().as_bytes())
            .map_err(|_| SendError::InvalidHeader(header.name.clone()))?;
        let value = reqwest::header::HeaderValue::from_str(&header.value)
            .map_err(|_| SendError::InvalidHeader(format!("{}: {}", header.name, header.value)))?;
        builder = builder.header(name, value);
    }
    if let Some(form) = &request.form {
        let fields = form
            .fields
            .iter()
            .filter(|f| f.enabled && !f.name.is_empty());
        match form.kind {
            FormKind::Urlencoded => {
                let pairs: Vec<String> = fields
                    .map(|f| format!("{}={}", encode_form(&f.name), encode_form(&f.value)))
                    .collect();
                if !has_header("content-type") {
                    builder = builder.header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    );
                }
                builder = builder.body(pairs.join("&"));
            }
            FormKind::Multipart => {
                let mut parts = reqwest::multipart::Form::new();
                for field in fields {
                    if field.file {
                        let bytes = read_file(root, &field.value)?;
                        let file_name = Path::new(&field.value)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let part = reqwest::multipart::Part::bytes(bytes)
                            .file_name(file_name)
                            .mime_str(mime_for(&field.value))
                            .map_err(|e| SendError::Other(e.to_string()))?;
                        parts = parts.part(field.name.clone(), part);
                    } else {
                        parts = parts.text(field.name.clone(), field.value.clone());
                    }
                }
                builder = builder.multipart(parts);
            }
        }
    } else if let Some(file) = request
        .body_file
        .as_deref()
        .filter(|f| !f.trim().is_empty())
    {
        let bytes = read_file(root, file)?;
        if !has_header("content-type") {
            builder = builder.header(reqwest::header::CONTENT_TYPE, mime_for(file));
        }
        builder = builder.body(bytes);
    } else if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }

    let started = Instant::now();
    let response = builder.send().await?;
    let status = response.status();
    let http_version = format!("{:?}", response.version());
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| Header::new(name.as_str(), String::from_utf8_lossy(value.as_bytes())))
        .collect();
    let bytes = response.bytes().await?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let (body, body_is_text) = match std::str::from_utf8(&bytes) {
        Ok(text) => (text.to_string(), true),
        Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), false),
    };

    Ok(HttpResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        http_version,
        headers,
        body,
        body_is_text,
        size_bytes: bytes.len(),
        duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_each_kind_of_auth() {
        let mut url = "https://api.test/x?a=1#top".to_string();
        let mut headers = vec![];
        apply_auth(&mut url, &mut headers, &Auth::Bearer { token: "t".into() });
        assert_eq!(headers, vec![Header::new("Authorization", "Bearer t")]);

        let mut headers = vec![];
        apply_auth(
            &mut url,
            &mut headers,
            &Auth::Basic {
                username: "ana".into(),
                password: "pw".into(),
            },
        );
        assert_eq!(headers[0].value, "Basic YW5hOnB3");

        let mut headers = vec![Header::new("authorization", "Bearer by-hand")];
        apply_auth(&mut url, &mut headers, &Auth::Bearer { token: "t".into() });
        assert_eq!(headers.len(), 1, "a header written by hand wins");

        let mut headers = vec![];
        apply_auth(
            &mut url,
            &mut headers,
            &Auth::ApiKey {
                key: "X-Key".into(),
                value: "k".into(),
                location: ApiKeyLocation::Header,
            },
        );
        assert_eq!(headers, vec![Header::new("X-Key", "k")]);

        apply_auth(
            &mut url,
            &mut headers,
            &Auth::ApiKey {
                key: "api key".into(),
                value: "a&b".into(),
                location: ApiKeyLocation::Query,
            },
        );
        assert_eq!(url, "https://api.test/x?a=1&api%20key=a%26b#top");
    }

    #[test]
    fn finds_path_params_but_not_ports() {
        assert_eq!(
            path_param_names("http://localhost:8080/orders/:orderId/items/:item_id?x=:no"),
            ["orderId", "item_id"]
        );
        assert_eq!(path_param_names("{{base}}/users/:id"), ["id"]);
        assert!(path_param_names("https://api.test/plain").is_empty());
    }

    #[test]
    fn replaces_path_params_and_encodes_values() {
        let params = [Param::new("orderId", "a b/c"), Param::new("unused", "x")];
        assert_eq!(
            apply_path_params(
                "https://api.test:8443/orders/:orderId/lines?x=:orderId",
                &params
            ),
            "https://api.test:8443/orders/a%20b%2Fc/lines?x=:orderId"
        );
        assert_eq!(
            apply_path_params("https://api.test/:missing", &params),
            "https://api.test/:missing"
        );
        assert_eq!(
            apply_path_params("https://api.test/:orderId.json", &params),
            "https://api.test/a%20b%2Fc.json"
        );
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Answers one request with a JSON echo of the method, a header and the body.
    async fn echo_server(status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length || read == 0 {
                        break;
                    }
                }
                if read == 0 {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&buffer).to_string();
            let (head, body) = text.split_once("\r\n\r\n").unwrap();
            let method = head.split_whitespace().next().unwrap();
            let tenant = head
                .lines()
                .find_map(|l| l.strip_prefix("x-tenant-id: "))
                .unwrap_or("");
            let payload = format!(r#"{{"method":"{method}","tenant":"{tenant}","body":{body:?}}}"#);
            let reply = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        });
        format!("http://{address}")
    }

    /// Answers one request with the raw request (head and body) as its body.
    async fn raw_echo() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer).to_string();
                let done = match text.find("\r\n\r\n") {
                    Some(end) => {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        buffer.len() >= end + 4 + length
                    }
                    None => false,
                };
                if done || read == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                buffer.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            socket.write_all(&buffer).await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn sends_urlencoded_forms() {
        let url = raw_echo().await;
        let mut request = HttpRequest::new(HttpMethod::Post, url);
        request.body = Some("ignored".into());
        request.form = Some(Form {
            kind: FormKind::Urlencoded,
            fields: vec![
                FormField::text("q", "a b&c"),
                FormField {
                    enabled: false,
                    ..FormField::text("off", "1")
                },
                FormField::text("n", "ñ"),
            ],
        });
        let echoed = send(&request).await.unwrap().body;
        assert!(echoed.contains("content-type: application/x-www-form-urlencoded"));
        assert!(echoed.ends_with("\r\n\r\nq=a+b%26c&n=%C3%B1"), "{echoed}");
    }

    #[tokio::test]
    async fn sends_multipart_forms_and_files_from_the_workspace() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir(ws.path().join("files")).unwrap();
        std::fs::write(ws.path().join("files/hola.txt"), "hola pulpo").unwrap();

        let url = raw_echo().await;
        let mut request = HttpRequest::new(HttpMethod::Post, url);
        request
            .headers
            .push(Header::new("Content-Type", "multipart/form-data"));
        request.form = Some(Form {
            kind: FormKind::Multipart,
            fields: vec![
                FormField::text("title", "Hello"),
                FormField::file("doc", "files/hola.txt"),
            ],
        });
        let echoed = send_in(&request, Some(ws.path())).await.unwrap().body;
        assert!(
            echoed.contains("content-type: multipart/form-data; boundary="),
            "{echoed}"
        );
        assert_eq!(
            echoed.matches("content-type: multipart").count(),
            1,
            "the hand-written one is dropped"
        );
        assert!(echoed.contains("name=\"title\"\r\n\r\nHello"));
        assert!(echoed.contains(
            "name=\"doc\"; filename=\"hola.txt\"\r\nContent-Type: text/plain\r\n\r\nhola pulpo"
        ));

        let error = send(&request).await.unwrap_err();
        assert!(
            matches!(error, SendError::File(_)),
            "no workspace, no files"
        );
    }

    /// Answers one request with a fixed raw response.
    async fn reply_once(reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await.unwrap();
            socket.write_all(reply.as_bytes()).await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn can_stop_following_redirects() {
        let url = reply_once(
            "HTTP/1.1 302 Found
location: http://127.0.0.1:9/never
content-length: 0
connection: close

",
        )
        .await;
        let mut request = HttpRequest::new(HttpMethod::Get, url);
        request.settings.follow_redirects = Some(false);
        let response = send(&request).await.unwrap();
        assert_eq!(response.status, 302);
        assert!(response.headers.iter().any(|h| h.name == "location"));
    }

    #[tokio::test]
    async fn sends_through_a_proxy() {
        let proxy = raw_echo().await;
        let mut request = HttpRequest::new(HttpMethod::Get, "http://api.example.invalid/orders");
        request.settings.proxy = Some(proxy);
        let echoed = send(&request).await.unwrap().body;
        assert!(
            echoed.starts_with("GET http://api.example.invalid/orders HTTP/1.1"),
            "the proxy gets the absolute URL: {echoed}"
        );

        request.settings.proxy = Some("not a url".into());
        assert!(matches!(
            send(&request).await.unwrap_err(),
            SendError::InvalidUrl(_)
        ));
    }

    #[test]
    fn settings_inherit_field_by_field() {
        let folder = Settings {
            timeout_ms: Some(1000),
            verify_ssl: Some(false),
            proxy: Some("none".into()),
            ..Default::default()
        };
        let own = Settings {
            timeout_ms: Some(50),
            follow_redirects: Some(false),
            ..Default::default()
        };
        assert_eq!(
            own.over(&folder),
            Settings {
                timeout_ms: Some(50),
                verify_ssl: Some(false),
                follow_redirects: Some(false),
                proxy: Some("none".into()),
            }
        );
        assert!(Settings::default().is_empty());
    }

    #[tokio::test]
    async fn sends_a_file_as_the_body() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("data.json"), "{\"a\":1}").unwrap();
        let url = raw_echo().await;
        let mut request = HttpRequest::new(HttpMethod::Put, url);
        request.body_file = Some("data.json".into());
        let echoed = send_in(&request, Some(ws.path())).await.unwrap().body;
        assert!(echoed.contains("content-type: application/json"));
        assert!(echoed.ends_with("{\"a\":1}"));
    }

    #[test]
    fn workspace_files_cannot_leave_the_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let ws = parent.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(parent.path().join("secret.txt"), "x").unwrap();
        std::fs::write(ws.join("ok.txt"), "x").unwrap();

        assert!(workspace_file(Some(&ws), "ok.txt").is_ok());
        assert!(workspace_file(Some(&ws), "./ok.txt").is_ok());
        for bad in ["../secret.txt", "", "missing.txt"] {
            assert!(workspace_file(Some(&ws), bad).is_err(), "{bad}");
        }
        let absolute = parent.path().join("secret.txt");
        assert!(workspace_file(Some(&ws), &absolute.to_string_lossy()).is_err());
        assert!(workspace_file(None, "ok.txt").is_err());
    }

    #[tokio::test]
    async fn sends_method_headers_and_body() {
        let url = echo_server("HTTP/1.1 201 Created").await;
        let mut request = HttpRequest::new(HttpMethod::Post, format!("{url}/orders"));
        request.headers.push(Header::new("X-Tenant-ID", "test"));
        request.headers.push(Header {
            name: "X-Ignored".into(),
            value: "nope".into(),
            enabled: false,
        });
        request.body = Some("hola".into());

        let response = send(&request).await.unwrap();

        assert_eq!(response.status, 201);
        assert_eq!(response.status_text, "Created");
        assert!(response.body_is_text);
        assert_eq!(
            response.body,
            r#"{"method":"POST","tenant":"test","body":"hola"}"#
        );
        assert_eq!(response.size_bytes, response.body.len());
        assert!(response
            .headers
            .iter()
            .any(|h| h.name == "content-type" && h.value == "application/json"));
    }

    #[tokio::test]
    async fn returns_error_statuses_as_responses() {
        let url = echo_server("HTTP/1.1 404 Not Found").await;
        let response = send(&HttpRequest::new(HttpMethod::Get, url)).await.unwrap();
        assert_eq!(response.status, 404);
    }

    #[tokio::test]
    async fn rejects_invalid_urls() {
        let error = send(&HttpRequest::new(HttpMethod::Get, "not a url"))
            .await
            .unwrap_err();
        assert!(matches!(error, SendError::InvalidUrl(_)));
    }

    #[tokio::test]
    async fn rejects_unsupported_schemes() {
        let error = send(&HttpRequest::new(HttpMethod::Get, "ftp://example.com"))
            .await
            .unwrap_err();
        assert!(matches!(error, SendError::InvalidUrl(_)));
    }

    #[tokio::test]
    async fn reports_connection_failures() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = send(&HttpRequest::new(
            HttpMethod::Get,
            format!("http://{address}"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, SendError::Connect(_)), "{error:?}");
    }

    #[test]
    fn serializes_requests_in_camel_case() {
        let mut request = HttpRequest::new(HttpMethod::Get, "http://localhost");
        request.settings.timeout_ms = Some(500);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""method":"GET""#));
        assert!(json.contains(r#""settings":{"timeoutMs":500}"#));
    }
}
