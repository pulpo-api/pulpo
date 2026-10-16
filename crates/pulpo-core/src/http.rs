//! HTTP engine: sends a request and measures the response.

use crate::HttpMethod;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
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
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub disabled_params: Vec<Param>,
    #[serde(default)]
    pub path_params: Vec<Param>,
    /// `None` = inherit from the folder (resolved before sending).
    #[serde(default)]
    pub auth: Option<Auth>,
}

impl HttpRequest {
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        HttpRequest {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout_ms: None,
            disabled_params: Vec::new(),
            path_params: Vec::new(),
            auth: None,
        }
    }
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
pub async fn send(request: &HttpRequest) -> Result<HttpResponse, SendError> {
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

    let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()?;

    let mut builder = client.request(to_reqwest_method(request.method), url);
    for header in headers
        .iter()
        .filter(|h| h.enabled && !h.name.trim().is_empty())
    {
        let name = reqwest::header::HeaderName::from_bytes(header.name.trim().as_bytes())
            .map_err(|_| SendError::InvalidHeader(header.name.clone()))?;
        let value = reqwest::header::HeaderValue::from_str(&header.value)
            .map_err(|_| SendError::InvalidHeader(format!("{}: {}", header.name, header.value)))?;
        builder = builder.header(name, value);
    }
    if let Some(body) = &request.body {
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
        request.timeout_ms = Some(500);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""method":"GET""#));
        assert!(json.contains(r#""timeoutMs":500"#));
    }
}
