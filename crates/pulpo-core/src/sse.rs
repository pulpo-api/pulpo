//! Server-Sent Events: a request that asks for `Accept: text/event-stream`.
//!
//! The app listens live ([`open`]): every event arrives as an [`Event`] until the server
//! ends the stream or the app stops it. `pulpo run` and flows use [`collect`]: listen until
//! the stream goes quiet (or ends) and hand the events to the tests as a JSON list.

use crate::http::{sniff_content_type, Header, HttpRequest, HttpResponse, SendError};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// How long `collect` waits for another event before it stops listening.
const QUIET: Duration = Duration::from_millis(2000);
/// The most `exchange` listens after it opens, even if the server keeps talking (tickers).
const LISTEN: Duration = Duration::from_secs(5);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// One event of the stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// `message` when the server names none; `info` and `error` are Pulpo's own lines
    /// (connected, ended).
    pub event: String,
    pub data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Milliseconds since the stream opened.
    pub at_ms: u64,
}

/// Whether a request asks for an event stream (an enabled `Accept: text/event-stream`).
pub fn is_event_stream(request: &HttpRequest) -> bool {
    request.headers.iter().any(|h| {
        h.enabled
            && h.name.trim().eq_ignore_ascii_case("accept")
            && h.value.to_ascii_lowercase().contains("text/event-stream")
    })
}

/// Reads the `text/event-stream` format: `event:`, `data:` (several lines join with `\n`),
/// `id:`; a blank line ends an event, `:` lines are comments.
#[derive(Default)]
pub struct Parser {
    buffer: String,
    event: String,
    data: Vec<String>,
    id: Option<String>,
}

impl Parser {
    /// Feeds a piece of the stream; returns the events it completed.
    pub fn feed(&mut self, text: &str) -> Vec<(String, String, Option<String>)> {
        self.buffer.push_str(text);
        let mut out = Vec::new();
        while let Some(end) = self.buffer.find(['\n', '\r']) {
            let line = self.buffer[..end].to_string();
            let mut skip = end + 1;
            if self.buffer[end..].starts_with("\r\n") {
                skip = end + 2;
            } else if self.buffer[end..].starts_with('\r') && skip == self.buffer.len() {
                // A lone \r at the end may be half of \r\n: wait for more.
                break;
            }
            self.buffer.drain(..skip);
            if line.is_empty() {
                if !self.data.is_empty() || !self.event.is_empty() {
                    let event = if self.event.is_empty() {
                        "message".to_string()
                    } else {
                        std::mem::take(&mut self.event)
                    };
                    out.push((event, self.data.join("\n"), self.id.clone()));
                    self.data.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => self.data.push(value.to_string()),
                "id" => self.id = Some(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

/// Sends the (resolved) request and returns the open response, checked to be a stream.
async fn start(request: &HttpRequest) -> Result<(reqwest::Response, Vec<Header>), SendError> {
    let mut client = reqwest::Client::builder()
        .connect_timeout(
            request
                .settings
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_TIMEOUT),
        )
        .user_agent(crate::http::USER_AGENT);
    if request.settings.verify_ssl == Some(false) {
        client = client.danger_accept_invalid_certs(true);
    }
    match request.settings.proxy.as_deref().map(str::trim) {
        None | Some("") | Some("system") => {}
        Some("none") => client = client.no_proxy(),
        Some(url) => {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| SendError::InvalidUrl(format!("proxy {url}: {e}")))?;
            client = client.proxy(proxy);
        }
    }
    let client = client
        .build()
        .map_err(|e| SendError::Other(e.to_string()))?;
    let url = crate::http::apply_path_params(&request.url, &request.path_params);
    let method = reqwest::Method::from_bytes(request.method.to_string().as_bytes())
        .map_err(|e| SendError::Other(e.to_string()))?;
    let mut builder = client.request(method, url.trim());
    let mut has_type = false;
    for h in request
        .headers
        .iter()
        .filter(|h| h.enabled && !h.name.trim().is_empty())
    {
        has_type |= h.name.trim().eq_ignore_ascii_case("content-type");
        builder = builder.header(h.name.trim(), h.value.as_str());
    }
    if let Some(body) = request.wire_body().filter(|b| !b.is_empty()) {
        if !has_type {
            builder = builder.header("content-type", sniff_content_type(&body));
        }
        builder = builder.body(body);
    }
    let response = builder.send().await.map_err(|e| {
        if e.is_timeout() {
            SendError::Timeout(e.to_string())
        } else if e.is_connect() {
            SendError::Connect(e.to_string())
        } else {
            SendError::Other(e.to_string())
        }
    })?;
    let headers: Vec<Header> = response
        .headers()
        .iter()
        .map(|(n, v)| Header::new(n.as_str(), v.to_str().unwrap_or_default()))
        .collect();
    let status = response.status();
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !status.is_success() || !kind.contains("text/event-stream") {
        let code = status.as_u16();
        let reason = status.canonical_reason().unwrap_or("");
        let body = response.text().await.unwrap_or_default();
        let cut: String = body.chars().take(300).collect();
        return Err(SendError::Other(format!(
            "{code} {reason}: not an event stream ({}){}",
            if kind.is_empty() {
                "no Content-Type"
            } else {
                kind.as_str()
            },
            if cut.is_empty() {
                String::new()
            } else {
                format!(": {cut}")
            }
        )));
    }
    Ok((response, headers))
}

fn line(event: &str, data: impl Into<String>, started: Instant) -> Event {
    Event {
        event: event.into(),
        data: data.into(),
        id: None,
        at_ms: started.elapsed().as_millis() as u64,
    }
}

/// Stops a live stream.
pub struct Live {
    stop: Option<oneshot::Sender<()>>,
}

impl Live {
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Listens live; every event goes to `on_event`, and the last line says why it ended.
pub async fn open(
    request: &HttpRequest,
    mut on_event: impl FnMut(Event) + Send + 'static,
) -> Result<Live, SendError> {
    let started = Instant::now();
    let (mut response, _) = start(request).await?;
    on_event(line(
        "info",
        format!("Listening to {}", request.url.trim()),
        started,
    ));
    let (tx, mut rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut parser = Parser::default();
        loop {
            tokio::select! {
                _ = &mut rx => {
                    on_event(line("info", "Stopped", started));
                    break;
                }
                chunk = response.chunk() => match chunk {
                    Ok(Some(bytes)) => {
                        for (event, data, id) in parser.feed(&String::from_utf8_lossy(&bytes)) {
                            on_event(Event { id, ..line(&event, data, started) });
                        }
                    }
                    Ok(None) => {
                        on_event(line("info", "The server ended the stream", started));
                        break;
                    }
                    Err(e) => {
                        on_event(line("error", e.to_string(), started));
                        break;
                    }
                },
            }
        }
    });
    Ok(Live { stop: Some(tx) })
}

/// For runs and flows: listens until the stream ends or goes quiet (or the timeout). The
/// response is the stream's status with the events as a JSON list
/// (`[{"event", "data", "id"}]`, JSON data as values), for `pm.response.json()`.
pub async fn collect(request: &HttpRequest) -> Result<HttpResponse, SendError> {
    let started = Instant::now();
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let (mut response, headers) = start(request).await?;
    let status = response.status();
    let mut parser = Parser::default();
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut size = 0usize;
    let listening = Instant::now();
    loop {
        let left = limit
            .saturating_sub(started.elapsed())
            .min(LISTEN.saturating_sub(listening.elapsed()));
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(QUIET.min(left), response.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                size += bytes.len();
                for (event, data, id) in parser.feed(&String::from_utf8_lossy(&bytes)) {
                    let value = serde_json::from_str(&data)
                        .unwrap_or_else(|_| serde_json::Value::String(data.clone()));
                    let mut item = serde_json::json!({ "event": event, "data": value });
                    if let Some(id) = id {
                        item["id"] = serde_json::Value::String(id);
                    }
                    events.push(item);
                }
            }
            _ => break,
        }
    }
    Ok(HttpResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").into(),
        http_version: "SSE".into(),
        headers,
        body: serde_json::to_string_pretty(&events).unwrap_or_default(),
        body_is_text: true,
        body_base64: None,
        size_bytes: size,
        duration_ms: started.elapsed().as_millis() as u64,
        url: request.url.clone(),
        redirects: Vec::new(),
        timings: None,
        sent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpMethod;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_the_event_stream_format() {
        let mut p = Parser::default();
        let mut got = p.feed(": comment\nevent: tick\ndata: {\"n\": 1}\nid: 7\n\ndata: a");
        got.extend(p.feed("\ndata: b\r\n\r\n"));
        assert_eq!(
            got,
            vec![
                ("tick".into(), "{\"n\": 1}".into(), Some("7".into())),
                ("message".into(), "a\nb".into(), Some("7".into())),
            ]
        );
    }

    /// Sends three events (one every 50 ms) and ends the stream; `/plain` is not a stream.
    async fn server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 4096];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                    if text.starts_with("GET /plain") {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi")
                            .await;
                        return;
                    }
                    assert!(text.contains("authorization: Bearer t-1"), "{text}");
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                        .await;
                    for n in 1..=3 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let event = format!("event: tick\nid: {n}\ndata: {{\"n\": {n}}}\n\n");
                        let _ = socket.write_all(event.as_bytes()).await;
                    }
                });
            }
        });
        format!("http://{address}")
    }

    fn request(url: &str) -> HttpRequest {
        let mut r = HttpRequest::new(HttpMethod::Get, url);
        r.headers.push(Header::new("Accept", "text/event-stream"));
        r.headers.push(Header::new("Authorization", "Bearer t-1"));
        r
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn collects_events_for_tests() {
        let base = server().await;
        let response = collect(&request(&format!("{base}/events"))).await.unwrap();
        assert_eq!(response.status, 200);
        let got: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(
            got[0],
            serde_json::json!({ "event": "tick", "data": { "n": 1 }, "id": "1" })
        );
        assert_eq!(got.as_array().unwrap().len(), 3);
        let error = collect(&request(&format!("{base}/plain")))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not an event stream (text/plain)"),
            "{error}"
        );
        assert!(is_event_stream(&request("x")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listens_live_until_the_server_ends() {
        let base = server().await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let _live = open(&request(&format!("{base}/events")), move |e| {
            sink.lock().unwrap().push(format!("{} {}", e.event, e.data))
        })
        .await
        .unwrap();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if seen.lock().unwrap().iter().any(|s| s.contains("ended")) {
                break;
            }
        }
        let seen = seen.lock().unwrap().clone();
        assert!(seen[0].starts_with("info Listening to http://"), "{seen:?}");
        assert_eq!(
            seen[1..4],
            ["tick {\"n\": 1}", "tick {\"n\": 2}", "tick {\"n\": 3}"]
        );
        assert_eq!(seen[4], "info The server ended the stream");
    }
}
