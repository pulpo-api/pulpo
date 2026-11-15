//! Socket.IO (v4, over WebSocket): connect to a namespace, emit events, receive them.
//!
//! The protocol rides on Engine.IO: `0` opens (the server's handshake), `2`/`3` are
//! ping/pong, and `4` carries Socket.IO packets: `0` connect, `2` event
//! (`42/admin,["name",{...}]`), `4` connect error, `1` disconnect. The request's saved
//! messages are events (`name` → event, `body` → its data).

use crate::http::{HttpRequest, HttpResponse, SendError};
use crate::websocket;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const QUIET: Duration = Duration::from_millis(1500);
/// The most `exchange` listens after joining, even if the server keeps talking (tickers).
const LISTEN: Duration = Duration::from_secs(5);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where and how to connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SocketIoTarget {
    /// `/` or `/admin`.
    #[serde(default = "root_namespace")]
    pub namespace: String,
    /// The server's path, `/socket.io` unless it was changed.
    #[serde(default = "default_path")]
    pub path: String,
    /// JSON sent with the connection (the `auth` of the client), may hold `{{variables}}`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
}

impl Default for SocketIoTarget {
    fn default() -> Self {
        SocketIoTarget {
            namespace: root_namespace(),
            path: default_path(),
            auth: String::new(),
        }
    }
}

fn root_namespace() -> String {
    "/".into()
}

fn default_path() -> String {
    "/socket.io".into()
}

/// The WebSocket URL of a Socket.IO server: `http(s)://host:port` (or `ws(s)://`) →
/// `ws(s)://host:port/socket.io/?EIO=4&transport=websocket`, keeping the query.
pub fn socket_url(url: &str, path: &str) -> String {
    let u = url.trim();
    let (scheme, rest) = match u.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("http".to_string(), u),
    };
    let secure = scheme == "https" || scheme == "wss";
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, format!("&{q}")),
        None => (rest, String::new()),
    };
    let host = rest.split('/').next().unwrap_or(rest);
    let path = format!("/{}/", path.trim().trim_matches('/'));
    format!(
        "{}://{host}{path}?EIO=4&transport=websocket{query}",
        if secure { "wss" } else { "ws" }
    )
}

fn namespace_prefix(namespace: &str) -> String {
    let ns = namespace.trim();
    if ns.is_empty() || ns == "/" {
        String::new()
    } else {
        format!(
            "{},",
            if ns.starts_with('/') {
                ns.to_string()
            } else {
                format!("/{ns}")
            }
        )
    }
}

/// The packet that joins a namespace, with the auth payload.
pub fn connect_packet(target: &SocketIoTarget) -> String {
    format!(
        "40{}{}",
        namespace_prefix(&target.namespace),
        target.auth.trim()
    )
}

/// An event: `42["name", data]` (data parsed as JSON when it is, else a string; no data
/// when the body is empty).
pub fn emit_packet(namespace: &str, event: &str, body: &str) -> String {
    let mut items = vec![serde_json::Value::String(event.to_string())];
    let body = body.trim();
    if !body.is_empty() {
        items.push(
            serde_json::from_str(body)
                .unwrap_or_else(|_| serde_json::Value::String(body.to_string())),
        );
    }
    format!(
        "42{}{}",
        namespace_prefix(namespace),
        serde_json::Value::Array(items)
    )
}

/// What a frame from the server means.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    /// Engine.IO handshake.
    Open,
    Ping,
    Connected,
    ConnectError(String),
    Event {
        event: String,
        args: Vec<serde_json::Value>,
    },
    Disconnected,
    Other,
}

pub fn parse_packet(text: &str) -> Packet {
    let mut chars = text.chars();
    match chars.next() {
        Some('0') => return Packet::Open,
        Some('2') => return Packet::Ping,
        Some('4') => {}
        _ => return Packet::Other,
    }
    let kind = chars.next();
    let mut rest: &str = &text[2.min(text.len())..];
    // Namespace (`/admin,`) and ack id (digits) come before the payload.
    if rest.starts_with('/') {
        rest = rest.split_once(',').map(|(_, r)| r).unwrap_or("");
    }
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    match kind {
        Some('0') => Packet::Connected,
        Some('1') => Packet::Disconnected,
        Some('4') => Packet::ConnectError(
            serde_json::from_str::<serde_json::Value>(rest)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| rest.to_string()),
        ),
        Some('2') | Some('3') => match serde_json::from_str::<serde_json::Value>(rest) {
            Ok(serde_json::Value::Array(mut items)) if !items.is_empty() => {
                let event = match items.remove(0) {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                Packet::Event { event, args: items }
            }
            _ => Packet::Other,
        },
        _ => Packet::Other,
    }
}

/// For runs and flows: connects, joins the namespace, emits the saved messages as events
/// and collects the events the server sends until it goes quiet. The response is `101`
/// with `[{"event", "args"}]` for the tests.
pub async fn exchange(
    request: &HttpRequest,
    target: &SocketIoTarget,
) -> Result<HttpResponse, SendError> {
    let started = Instant::now();
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let mut socket_request = request.clone();
    socket_request.url = socket_url(&request.url, &target.path);
    let (socket, headers) = websocket::connect_raw(&socket_request).await?;
    let (mut write, mut read) = socket.split();
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut connected = false;
    let mut listening: Option<Instant> = None;
    let mut size = 0usize;
    loop {
        let mut left = limit.saturating_sub(started.elapsed());
        if let Some(since) = listening {
            left = left.min(LISTEN.saturating_sub(since.elapsed()));
        }
        if left.is_zero() {
            break;
        }
        let wait = if connected { QUIET.min(left) } else { left };
        let text = match tokio::time::timeout(wait, read.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => t.to_string(),
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => break,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => return Err(SendError::Other(e.to_string())),
        };
        size += text.len();
        let send = |t: String| Message::Text(t.into());
        match parse_packet(&text) {
            Packet::Open => {
                write
                    .send(send(connect_packet(target)))
                    .await
                    .map_err(|e| SendError::Other(e.to_string()))?;
            }
            Packet::Ping => {
                let _ = write.send(send("3".into())).await;
            }
            Packet::Connected if !connected => {
                connected = true;
                listening = Some(Instant::now());
                for m in &request.messages {
                    write
                        .send(send(emit_packet(&target.namespace, &m.name, &m.body)))
                        .await
                        .map_err(|e| SendError::Other(e.to_string()))?;
                }
            }
            Packet::ConnectError(message) => {
                return Err(SendError::Other(format!(
                    "the server refused the connection: {message}"
                )));
            }
            Packet::Event { event, args } => {
                events.push(serde_json::json!({ "event": event, "args": args }));
            }
            Packet::Disconnected => break,
            _ => {}
        }
    }
    let _ = write.send(Message::Close(None)).await;
    if !connected {
        return Err(SendError::Other(
            "no Socket.IO connection (is it a Socket.IO v4 server at this path?)".into(),
        ));
    }
    Ok(HttpResponse {
        status: 101,
        status_text: "Switching Protocols".into(),
        http_version: "Socket.IO".into(),
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
    use tokio::net::TcpListener;

    #[test]
    fn writes_and_reads_packets() {
        assert_eq!(
            socket_url("https://chat.example.com:8443/rooms?token=a", "/socket.io"),
            "wss://chat.example.com:8443/socket.io/?EIO=4&transport=websocket&token=a"
        );
        assert_eq!(
            socket_url("http://localhost:3000", "ws/"),
            "ws://localhost:3000/ws/?EIO=4&transport=websocket"
        );
        let mut t = SocketIoTarget::default();
        assert_eq!(connect_packet(&t), "40");
        t.namespace = "admin".into();
        t.auth = r#"{"token":"x"}"#.into();
        assert_eq!(connect_packet(&t), r#"40/admin,{"token":"x"}"#);
        assert_eq!(
            emit_packet("/", "hello", r#"{"a":1}"#),
            r#"42["hello",{"a":1}]"#
        );
        assert_eq!(emit_packet("/admin", "ping", ""), r#"42/admin,["ping"]"#);
        assert_eq!(emit_packet("/", "say", "hola"), r#"42["say","hola"]"#);
        assert_eq!(
            parse_packet(r#"0{"sid":"x","pingInterval":25000}"#),
            Packet::Open
        );
        assert_eq!(parse_packet("2"), Packet::Ping);
        assert_eq!(parse_packet(r#"40/admin,{"sid":"y"}"#), Packet::Connected);
        assert_eq!(
            parse_packet(r#"42/admin,7["tick",{"n":1},"x"]"#),
            Packet::Event {
                event: "tick".into(),
                args: vec![serde_json::json!({ "n": 1 }), serde_json::json!("x")]
            }
        );
        assert_eq!(
            parse_packet(r#"44{"message":"Not authorized"}"#),
            Packet::ConnectError("Not authorized".into())
        );
    }

    /// A tiny Socket.IO v4 server: answers `hello` with `welcome` and every `echo` back.
    async fn server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    let _ = ws
                        .send(Message::Text(
                            r#"0{"sid":"s1","pingInterval":25000,"pingTimeout":20000}"#.into(),
                        ))
                        .await;
                    let _ = ws.send(Message::Text("2".into())).await;
                    while let Some(Ok(Message::Text(text))) = ws.next().await {
                        let text = text.to_string();
                        if text.starts_with("40") {
                            if !text.contains("\"token\":\"t-1\"") {
                                let _ = ws
                                    .send(Message::Text(r#"44{"message":"Not authorized"}"#.into()))
                                    .await;
                                continue;
                            }
                            let _ = ws.send(Message::Text(r#"40{"sid":"c1"}"#.into())).await;
                        } else if let Packet::Event { event, args } = parse_packet(&text) {
                            let reply = if event == "hello" {
                                r#"42["welcome",{"rooms":2}]"#.to_string()
                            } else {
                                format!("42{}", serde_json::json!(["echo", args.first()]))
                            };
                            let _ = ws.send(Message::Text(reply.into())).await;
                        }
                    }
                });
            }
        });
        format!("http://{address}")
    }

    fn request(url: &str) -> HttpRequest {
        let mut r = HttpRequest::new(HttpMethod::Get, url);
        r.messages = vec![
            websocket::WsMessage {
                name: "hello".into(),
                body: String::new(),
            },
            websocket::WsMessage {
                name: "echo".into(),
                body: r#"{"text":"hola"}"#.into(),
            },
        ];
        r
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn joins_emits_and_collects_events() {
        let url = server().await;
        let target = SocketIoTarget {
            auth: r#"{"token":"t-1"}"#.into(),
            ..Default::default()
        };
        let response = exchange(&request(&url), &target).await.unwrap();
        let got: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(
            got,
            serde_json::json!([
                { "event": "welcome", "args": [{ "rooms": 2 }] },
                { "event": "echo", "args": [{ "text": "hola" }] }
            ])
        );
        let refused = exchange(&request(&url), &SocketIoTarget::default())
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("Not authorized"), "{refused}");
    }
}
