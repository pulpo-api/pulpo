//! WebSocket: a request whose URL is `ws://` or `wss://`.
//!
//! The handshake carries the request's headers and auth (its own or a folder's). The app
//! keeps a live connection ([`open`]): what is sent and received arrives as [`Frame`]s.
//! `pulpo run` and flows use [`exchange`]: connect, send the saved messages in order and
//! collect what comes back until the line goes quiet; tests see it as a JSON list.

use crate::http::{Header, HttpRequest, HttpResponse, SendError};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;

/// How long `exchange` waits for more messages after the last one.
const QUIET: Duration = Duration::from_millis(1500);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A saved message of a WebSocket request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsMessage {
    pub name: String,
    pub body: String,
}

/// One line of the conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    /// `sent`, `received`, `info` (connected, closed) or `error`.
    pub direction: String,
    pub text: String,
    /// The message was binary (`text` is its size).
    #[serde(default)]
    pub binary: bool,
    /// Milliseconds since the connection opened.
    pub at_ms: u64,
}

pub fn is_websocket_url(url: &str) -> bool {
    let u = url.trim_start().to_ascii_lowercase();
    u.starts_with("ws://") || u.starts_with("wss://")
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the connection with the (resolved) request's headers; returns the socket, the
/// handshake's response headers and a line saying how it went.
async fn connect(request: &HttpRequest) -> Result<(Socket, Vec<Header>, String), SendError> {
    if request.settings.verify_ssl == Some(false) {
        return Err(SendError::Other(
            "WebSocket does not skip certificate checks yet; turn \"Verify SSL\" back on".into(),
        ));
    }
    let mut handshake = request
        .url
        .trim()
        .into_client_request()
        .map_err(|e| SendError::InvalidUrl(e.to_string()))?;
    for h in request
        .headers
        .iter()
        .filter(|h| h.enabled && !h.name.trim().is_empty())
    {
        let name = HeaderName::from_bytes(h.name.trim().as_bytes())
            .map_err(|e| SendError::InvalidHeader(format!("{}: {e}", h.name)))?;
        let value = HeaderValue::from_str(&h.value)
            .map_err(|e| SendError::InvalidHeader(format!("{}: {e}", h.name)))?;
        handshake.headers_mut().append(name, value);
    }
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let (socket, response) =
        tokio::time::timeout(limit, tokio_tungstenite::connect_async(handshake))
            .await
            .map_err(|_| {
                SendError::Timeout(format!("no handshake after {} ms", limit.as_millis()))
            })?
            .map_err(|e| SendError::Connect(e.to_string()))?;
    let headers: Vec<Header> = response
        .headers()
        .iter()
        .map(|(n, v)| Header::new(n.as_str(), v.to_str().unwrap_or_default()))
        .collect();
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|p| format!(" · protocol {p}"))
        .unwrap_or_default();
    let note = format!("Connected to {}{protocol}", request.url.trim());
    Ok((socket, headers, note))
}

fn frame(direction: &str, text: impl Into<String>, started: Instant) -> Frame {
    Frame {
        direction: direction.into(),
        text: text.into(),
        binary: false,
        at_ms: started.elapsed().as_millis() as u64,
    }
}

fn received(message: Message, started: Instant) -> Option<Frame> {
    match message {
        Message::Text(text) => Some(frame("received", text.to_string(), started)),
        Message::Binary(bytes) => Some(Frame {
            binary: true,
            ..frame(
                "received",
                format!("binary message, {} bytes", bytes.len()),
                started,
            )
        }),
        Message::Close(close) => Some(frame(
            "info",
            match close {
                Some(c) if !c.reason.is_empty() => {
                    format!("Closed by the server: {} {}", u16::from(c.code), c.reason)
                }
                Some(c) => format!("Closed by the server: {}", u16::from(c.code)),
                None => "Closed by the server".to_string(),
            },
            started,
        )),
        // Pings are answered by the library; pongs and raw frames are noise.
        _ => None,
    }
}

/// A live connection: send text with [`Live::send`], close with [`Live::close`].
pub struct Live {
    outgoing: mpsc::UnboundedSender<Option<String>>,
}

impl Live {
    pub fn send(&self, text: String) -> bool {
        self.outgoing.send(Some(text)).is_ok()
    }

    pub fn close(&self) {
        let _ = self.outgoing.send(None);
    }
}

/// Connects and keeps the line open; every frame (also the ones it sends) goes to
/// `on_frame`, and the last one says it closed.
pub async fn open(
    request: &HttpRequest,
    mut on_frame: impl FnMut(Frame) + Send + 'static,
) -> Result<Live, SendError> {
    let started = Instant::now();
    let (socket, _, note) = connect(request).await?;
    on_frame(frame("info", note, started));
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<String>>();
    tokio::spawn(async move {
        let (mut write, mut read) = socket.split();
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Some(text)) => {
                        match write.send(Message::Text(text.clone().into())).await {
                            Ok(()) => on_frame(frame("sent", text, started)),
                            Err(e) => {
                                on_frame(frame("error", e.to_string(), started));
                                break;
                            }
                        }
                    }
                    // Closed by the app (or the app went away).
                    _ => {
                        let _ = write.send(Message::Close(None)).await;
                        on_frame(frame("info", "Closed", started));
                        break;
                    }
                },
                incoming = read.next() => match incoming {
                    Some(Ok(message)) => {
                        let closing = matches!(message, Message::Close(_));
                        if let Some(f) = received(message, started) {
                            on_frame(f);
                        }
                        if closing {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        on_frame(frame("error", e.to_string(), started));
                        break;
                    }
                    None => {
                        on_frame(frame("info", "Closed", started));
                        break;
                    }
                },
            }
        }
    });
    Ok(Live { outgoing: tx })
}

/// For runs and flows: connects, sends the saved messages in order and collects what
/// comes back until nothing arrives for a moment (or the timeout). The response is
/// `101` with the received messages as a JSON list (JSON messages as values, the rest
/// as strings), so test scripts read them with `pm.response.json()`.
pub async fn exchange(request: &HttpRequest) -> Result<HttpResponse, SendError> {
    let started = Instant::now();
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let (socket, headers, _) = connect(request).await?;
    let (mut write, mut read) = socket.split();
    for message in &request.messages {
        write
            .send(Message::Text(message.body.clone().into()))
            .await
            .map_err(|e| SendError::Other(format!("could not send '{}': {e}", message.name)))?;
    }
    let mut got: Vec<serde_json::Value> = Vec::new();
    let mut size = 0usize;
    loop {
        let left = limit.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(QUIET.min(left), read.next()).await {
            Err(_) => break,
            Ok(None) | Ok(Some(Err(_))) => break,
            Ok(Some(Ok(Message::Text(text)))) => {
                size += text.len();
                got.push(
                    serde_json::from_str(text.as_str())
                        .unwrap_or_else(|_| serde_json::Value::String(text.to_string())),
                );
            }
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                size += bytes.len();
                got.push(serde_json::Value::String(format!(
                    "binary message, {} bytes",
                    bytes.len()
                )));
            }
            Ok(Some(Ok(Message::Close(_)))) => break,
            Ok(Some(Ok(_))) => {}
        }
    }
    let _ = write.send(Message::Close(None)).await;
    let body = serde_json::to_string_pretty(&got).unwrap_or_default();
    Ok(HttpResponse {
        status: 101,
        status_text: "Switching Protocols".into(),
        http_version: "WebSocket".into(),
        headers,
        body_is_text: true,
        body_base64: None,
        size_bytes: size,
        duration_ms: started.elapsed().as_millis() as u64,
        url: request.url.clone(),
        redirects: Vec::new(),
        timings: None,
        sent: None,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpMethod;
    use tokio::net::TcpListener;

    /// Echoes text messages back in upper case, says hello first, and closes on "bye".
    // The handshake callback's error type is the library's, large by design.
    #[allow(clippy::result_large_err)]
    async fn server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let callback =
                        |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                         response| {
                            let token = request
                                .headers()
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            assert_eq!(token, "Bearer t-1");
                            Ok(response)
                        };
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await
                    else {
                        return;
                    };
                    let _ = ws.send(Message::Text(r#"{"hello":true}"#.into())).await;
                    while let Some(Ok(message)) = ws.next().await {
                        if let Message::Text(text) = message {
                            if text.as_str() == "bye" {
                                let _ = ws.close(None).await;
                                return;
                            }
                            let _ = ws.send(Message::Text(text.to_uppercase().into())).await;
                        }
                    }
                });
            }
        });
        format!("ws://{address}/live")
    }

    fn request(url: &str) -> HttpRequest {
        let mut request = HttpRequest::new(HttpMethod::Get, url);
        request
            .headers
            .push(Header::new("Authorization", "Bearer t-1"));
        request
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exchanges_saved_messages_and_collects_the_answers() {
        let mut r = request(&server().await);
        r.messages = vec![
            WsMessage {
                name: "Ping".into(),
                body: "ping".into(),
            },
            WsMessage {
                name: "Pong".into(),
                body: "pong".into(),
            },
        ];
        let response = exchange(&r).await.unwrap();
        assert_eq!(response.status, 101);
        let got: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(got, serde_json::json!([{ "hello": true }, "PING", "PONG"]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_a_live_connection() {
        let url = server().await;
        let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Frame>::new()));
        let sink = frames.clone();
        let live = open(&request(&url), move |f| sink.lock().unwrap().push(f))
            .await
            .unwrap();
        assert!(live.send("hola".into()));
        assert!(live.send("bye".into()));
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if frames
                .lock()
                .unwrap()
                .iter()
                .any(|f| f.text.starts_with("Closed"))
            {
                break;
            }
        }
        let seen: Vec<String> = frames
            .lock()
            .unwrap()
            .iter()
            .map(|f| format!("{} {}", f.direction, f.text))
            .collect();
        assert!(seen[0].starts_with("info Connected to ws://"), "{seen:?}");
        assert!(
            seen.contains(&r#"received {"hello":true}"#.to_string()),
            "{seen:?}"
        );
        assert!(seen.contains(&"sent hola".to_string()), "{seen:?}");
        assert!(seen.contains(&"received HOLA".to_string()), "{seen:?}");
        assert!(seen.last().unwrap().starts_with("info Closed"), "{seen:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn says_why_it_could_not_connect() {
        let error = exchange(&request("ws://127.0.0.1:9/nobody"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().starts_with("could not connect"),
            "{error}"
        );
        assert!(is_websocket_url(" WSS://x"));
        assert!(!is_websocket_url("https://x"));
    }

    fn entry(url: &str) -> crate::collection::RequestEntry {
        let mut request = HttpRequest::new(HttpMethod::Get, url);
        request.auth = Some(crate::http::Auth::Bearer {
            token: "{{token}}".into(),
        });
        request.messages = vec![
            WsMessage {
                name: "Subscribe".into(),
                body: "{\n  \"type\": \"subscribe\",\n  \"channel\": \"{{channel}}\"\n}".into(),
            },
            WsMessage {
                name: "Bye".into(),
                body: "bye".into(),
            },
        ];
        crate::collection::RequestEntry {
            id: "01-live.pulpo.yaml".into(),
            name: "Live".into(),
            protocol: crate::Protocol::Http,
            request,
            scripts: crate::collection::Scripts::default(),
        }
    }

    #[test]
    fn saves_messages_as_readable_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::collection::init_workspace(root, "Live").unwrap();
        let live = entry("wss://example.com/live");
        crate::collection::save_request(root, &live).unwrap();
        let yaml = std::fs::read_to_string(root.join("01-live.pulpo.yaml")).unwrap();
        assert!(yaml.contains("protocol: \"websocket\""), "{yaml}");
        assert!(!yaml.contains("method:"), "{yaml}");
        assert!(
            yaml.contains("    body: |-\n      {\n        \"type\": \"subscribe\","),
            "{yaml}"
        );
        assert!(yaml.contains("    body: \"bye\"\n"), "{yaml}");
        let back = crate::collection::load_request(root, "01-live.pulpo.yaml").unwrap();
        assert_eq!(back.request.messages, live.request.messages);
        assert_eq!(back.protocol, crate::Protocol::Websocket);
        assert!(crate::collection::format_workspace(root, true)
            .unwrap()
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_with_variables_auth_and_tests_on_the_answers() {
        let url = server().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::collection::init_workspace(root, "Live").unwrap();
        let mut live = entry(&url);
        live.protocol = crate::Protocol::Websocket;
        live.scripts.test = "const got = pm.response.json();\n\
            pm.test('greets', () => pm.expect(got[0].hello).to.eql(true));\n\
            pm.test('subscribed', () => pm.expect(got[1].CHANNEL).to.eql('BTC'));"
            .into();
        let mut options = crate::run::RunOptions::default();
        options.data.insert("token".into(), "t-1".into());
        options.data.insert("channel".into(), "btc".into());
        let mut session = crate::run::Session::open(
            root,
            None,
            &crate::environment::MemoryStore::default(),
            false,
        )
        .unwrap();
        let report = crate::run::run_in(root, &live, &mut session, &options).await;
        assert!(report.error.is_none(), "{:?}", report.error);
        assert_eq!(report.response.as_ref().unwrap().status, 101);
        assert!(report.passed(), "{:#?}", report.tests);
        assert!(report.response.unwrap().body.contains("BTC"));
    }

    /// Answers the handshake choosing `graphql-transport-ws`, as GraphQL servers do.
    #[allow(clippy::result_large_err)]
    async fn graphql_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let callback = |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                                mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    let offered = request
                        .headers()
                        .get("sec-websocket-protocol")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    assert!(offered.contains("graphql-transport-ws"), "{offered}");
                    response.headers_mut().insert(
                        "sec-websocket-protocol",
                        HeaderValue::from_static("graphql-transport-ws"),
                    );
                    Ok(response)
                };
                if let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await {
                    while let Some(Ok(Message::Text(text))) = ws.next().await {
                        if text.contains("connection_init") {
                            let _ = ws
                                .send(Message::Text(r#"{"type":"connection_ack"}"#.into()))
                                .await;
                        }
                    }
                }
            }
        });
        format!("ws://{address}/graphql")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn negotiates_a_subprotocol() {
        let mut r = HttpRequest::new(HttpMethod::Get, graphql_server().await);
        r.headers.push(Header::new(
            "Sec-WebSocket-Protocol",
            "graphql-transport-ws, graphql-ws",
        ));
        let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Frame>::new()));
        let sink = frames.clone();
        let live = open(&r, move |f| sink.lock().unwrap().push(f))
            .await
            .unwrap();
        assert!(live.send(r#"{"type":"connection_init","payload":{}}"#.into()));
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if frames
                .lock()
                .unwrap()
                .iter()
                .any(|f| f.text.contains("connection_ack"))
            {
                break;
            }
        }
        let seen: Vec<String> = frames
            .lock()
            .unwrap()
            .iter()
            .map(|f| f.text.clone())
            .collect();
        assert!(
            seen[0].ends_with("· protocol graphql-transport-ws"),
            "{seen:?}"
        );
        assert!(
            seen.iter().any(|t| t.contains("connection_ack")),
            "{seen:?}"
        );
        live.close();
    }
}
