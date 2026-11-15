//! MQTT 3.1.1: connect to a broker, subscribe to topics, publish and receive messages.
//!
//! Transports: `mqtt://host:1883` (TCP), `mqtts://host:8883` (TLS) and `ws://` / `wss://`
//! (MQTT over WebSocket, subprotocol `mqtt`). Only what testing needs: QoS 0 and 1,
//! retained messages, username/password, keep-alive pings.

use crate::http::{Header, HttpRequest, HttpResponse, SendError};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const QUIET: Duration = Duration::from_millis(1500);
/// The most a run listens after publishing, even if the broker keeps sending.
const LISTEN: Duration = Duration::from_secs(5);
const KEEP_ALIVE: u16 = 30;

/// How to talk to the broker. Saved messages are publications (name = topic).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MqttTarget {
    /// Empty: Pulpo makes one up.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// Topic filters to subscribe to (`sensors/+/temp`, `orders/#`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscribe: Vec<String>,
    /// QoS for subscriptions and publications: 0 or 1.
    #[serde(default)]
    pub qos: u8,
    /// Publish with the retain flag.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retain: bool,
}

pub fn is_mqtt_url(url: &str) -> bool {
    let u = url.trim_start().to_ascii_lowercase();
    u.starts_with("mqtt://") || u.starts_with("mqtts://")
}

/// A message from the broker (or a line of Pulpo's: `info`, `error`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MqttMessage {
    /// `received`, `sent`, `info` or `error`.
    pub direction: String,
    pub topic: String,
    pub payload: String,
    #[serde(default)]
    pub qos: u8,
    #[serde(default)]
    pub retain: bool,
    pub at_ms: u64,
}

// ------------------------------------------------------------------- packets

fn put_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Fixed header byte + "remaining length" (1–4 bytes, 7 bits each) + body.
fn packet(first: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = vec![first];
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend(body);
    out
}

pub fn connect_packet(target: &MqttTarget, client_id: &str) -> Vec<u8> {
    let mut body = Vec::new();
    put_string(&mut body, "MQTT");
    body.push(4); // protocol level 3.1.1
    let mut flags = 0x02; // clean session
    if !target.username.is_empty() {
        flags |= 0x80;
        if !target.password.is_empty() {
            flags |= 0x40;
        }
    }
    body.push(flags);
    body.extend_from_slice(&KEEP_ALIVE.to_be_bytes());
    put_string(&mut body, client_id);
    if !target.username.is_empty() {
        put_string(&mut body, &target.username);
        if !target.password.is_empty() {
            put_string(&mut body, &target.password);
        }
    }
    packet(0x10, body)
}

pub fn subscribe_packet(id: u16, topics: &[String], qos: u8) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    for t in topics {
        put_string(&mut body, t);
        body.push(qos.min(1));
    }
    packet(0x82, body)
}

pub fn publish_packet(id: u16, topic: &str, payload: &[u8], qos: u8, retain: bool) -> Vec<u8> {
    let qos = qos.min(1);
    let mut body = Vec::new();
    put_string(&mut body, topic);
    if qos > 0 {
        body.extend_from_slice(&id.to_be_bytes());
    }
    body.extend_from_slice(payload);
    packet(0x30 | (qos << 1) | u8::from(retain), body)
}

/// A packet from the broker.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    ConnAck(u8),
    SubAck(Vec<u8>),
    Publish {
        topic: String,
        payload: Vec<u8>,
        qos: u8,
        retain: bool,
        id: Option<u16>,
    },
    PubAck,
    PingResp,
    Other(u8),
}

/// Reads whole packets out of a byte buffer (what is left stays for the next read).
#[derive(Default)]
pub struct Reader {
    buffer: Vec<u8>,
}

impl Reader {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Incoming> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.buffer.len() < 2 {
                break;
            }
            let (mut len, mut mult, mut i) = (0usize, 1usize, 1usize);
            loop {
                if i >= self.buffer.len() || i > 4 {
                    return out;
                }
                let b = self.buffer[i] as usize;
                len += (b & 0x7f) * mult;
                mult *= 128;
                i += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            if self.buffer.len() < i + len {
                break;
            }
            let first = self.buffer[0];
            let body: Vec<u8> = self.buffer[i..i + len].to_vec();
            self.buffer.drain(..i + len);
            out.push(decode(first, &body));
        }
        out
    }
}

fn decode(first: u8, body: &[u8]) -> Incoming {
    match first >> 4 {
        2 => Incoming::ConnAck(body.get(1).copied().unwrap_or(0xff)),
        9 => Incoming::SubAck(body.get(2..).unwrap_or_default().to_vec()),
        4 => Incoming::PubAck,
        13 => Incoming::PingResp,
        3 => {
            let qos = (first >> 1) & 0x03;
            let retain = first & 0x01 == 1;
            let tlen = u16::from_be_bytes([
                body.first().copied().unwrap_or(0),
                body.get(1).copied().unwrap_or(0),
            ]) as usize;
            let topic =
                String::from_utf8_lossy(body.get(2..2 + tlen).unwrap_or_default()).into_owned();
            let mut at = 2 + tlen;
            let id = if qos > 0 {
                let id = u16::from_be_bytes([
                    body.get(at).copied().unwrap_or(0),
                    body.get(at + 1).copied().unwrap_or(0),
                ]);
                at += 2;
                Some(id)
            } else {
                None
            };
            Incoming::Publish {
                topic,
                payload: body.get(at..).unwrap_or_default().to_vec(),
                qos,
                retain,
                id,
            }
        }
        n => Incoming::Other(n),
    }
}

fn puback(id: u16) -> Vec<u8> {
    packet(0x40, id.to_be_bytes().to_vec())
}

fn connack_reason(code: u8) -> &'static str {
    match code {
        1 => "unacceptable protocol version",
        2 => "client id rejected",
        3 => "server unavailable",
        4 => "bad user name or password",
        5 => "not authorized",
        _ => "refused",
    }
}

// ------------------------------------------------------------------- transport

/// Bytes in and out, whatever the transport.
enum Link {
    Stream(Box<dyn AsyncReadWrite>),
    Ws(Box<crate::websocket::Socket>),
}

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

impl Link {
    async fn write(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        match self {
            Link::Stream(s) => s.write_all(&bytes).await.map_err(|e| e.to_string()),
            Link::Ws(ws) => ws
                .send(Message::Binary(bytes.into()))
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Link::Stream(s) => {
                let mut buf = vec![0u8; 16 * 1024];
                let n = s.read(&mut buf).await.map_err(|e| e.to_string())?;
                Ok((n > 0).then(|| buf[..n].to_vec()))
            }
            Link::Ws(ws) => loop {
                match ws.next().await {
                    Some(Ok(Message::Binary(b))) => return Ok(Some(b.to_vec())),
                    Some(Ok(Message::Close(_))) | None => return Ok(None),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.to_string()),
                }
            },
        }
    }
}

fn host_port(url: &str, default_port: u16) -> Result<(String, u16), SendError> {
    let rest = url.trim().split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (
            h.trim_matches(['[', ']']).to_string(),
            p.parse()
                .map_err(|_| SendError::InvalidUrl(format!("port in {url}")))?,
        ),
        _ => (authority.to_string(), default_port),
    };
    if host.is_empty() {
        return Err(SendError::InvalidUrl(url.to_string()));
    }
    Ok((host, port))
}

async fn open_link(request: &HttpRequest) -> Result<Link, SendError> {
    let url = request.url.trim();
    let lower = url.to_ascii_lowercase();
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    if lower.starts_with("ws://") || lower.starts_with("wss://") {
        let mut ws = request.clone();
        if !ws
            .headers
            .iter()
            .any(|h| h.enabled && h.name.eq_ignore_ascii_case("sec-websocket-protocol"))
        {
            ws.headers
                .push(Header::new("Sec-WebSocket-Protocol", "mqtt"));
        }
        let (socket, _) = crate::websocket::connect_raw(&ws).await?;
        return Ok(Link::Ws(Box::new(socket)));
    }
    let tls = lower.starts_with("mqtts://");
    let (host, port) = host_port(url, if tls { 8883 } else { 1883 })?;
    let tcp = tokio::time::timeout(limit, tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| SendError::Timeout(format!("no connection after {} ms", limit.as_millis())))?
        .map_err(|e| SendError::Connect(e.to_string()))?;
    if !tls {
        return Ok(Link::Stream(Box::new(tcp)));
    }
    if request.settings.verify_ssl == Some(false) {
        return Err(SendError::Other(
            "MQTT does not skip certificate checks yet; turn \"Verify SSL\" back on".into(),
        ));
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| SendError::Other(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| SendError::InvalidUrl(format!("{host}: {e}")))?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| SendError::Connect(e.to_string()))?;
    Ok(Link::Stream(Box::new(stream)))
}

fn client_id(target: &MqttTarget) -> String {
    if !target.client_id.trim().is_empty() {
        return target.client_id.trim().to_string();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("pulpo-{nanos:08x}")
}

/// Connects (CONNECT/CONNACK) and subscribes; returns the link and any packets that came
/// with the acknowledgements.
async fn session(
    request: &HttpRequest,
    target: &MqttTarget,
) -> Result<(Link, Reader, Vec<Incoming>), SendError> {
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let mut link = open_link(request).await?;
    link.write(connect_packet(target, &client_id(target)))
        .await
        .map_err(SendError::Other)?;
    let mut reader = Reader::default();
    let mut early = Vec::new();
    let mut acked = false;
    let mut subscribed = target.subscribe.is_empty();
    let started = Instant::now();
    while !(acked && subscribed) {
        let left = limit.saturating_sub(started.elapsed());
        let bytes = tokio::time::timeout(left, link.read())
            .await
            .map_err(|_| SendError::Timeout("the broker did not answer".into()))?
            .map_err(SendError::Other)?
            .ok_or_else(|| SendError::Other("the broker closed the connection".into()))?;
        for p in reader.feed(&bytes) {
            match p {
                Incoming::ConnAck(0) => {
                    acked = true;
                    if !subscribed {
                        link.write(subscribe_packet(1, &target.subscribe, target.qos))
                            .await
                            .map_err(SendError::Other)?;
                    }
                }
                Incoming::ConnAck(code) => {
                    return Err(SendError::Other(format!(
                        "the broker refused the connection: {}",
                        connack_reason(code)
                    )))
                }
                Incoming::SubAck(codes) => {
                    if codes.contains(&0x80) {
                        return Err(SendError::Other("the broker refused a subscription".into()));
                    }
                    subscribed = true;
                }
                other => early.push(other),
            }
        }
    }
    Ok((link, reader, early))
}

fn payload_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => format!("binary payload, {} bytes", bytes.len()),
    }
}

// ------------------------------------------------------------------- live

/// Publishes on a live connection; dropping it (or [`Live::close`]) disconnects.
pub struct Live {
    outgoing: mpsc::UnboundedSender<Option<(String, String, u8, bool)>>,
}

impl Live {
    pub fn publish(&self, topic: String, payload: String, qos: u8, retain: bool) -> bool {
        self.outgoing
            .send(Some((topic, payload, qos, retain)))
            .is_ok()
    }

    pub fn close(&self) {
        let _ = self.outgoing.send(None);
    }
}

/// Connects, subscribes and keeps the session; every message goes to `on_message`.
pub async fn open(
    request: &HttpRequest,
    target: &MqttTarget,
    mut on_message: impl FnMut(MqttMessage) + Send + 'static,
) -> Result<Live, SendError> {
    let started = Instant::now();
    let at = move || started.elapsed().as_millis() as u64;
    let info = |text: String, at: u64| MqttMessage {
        direction: "info".into(),
        payload: text,
        at_ms: at,
        ..Default::default()
    };
    let (mut link, mut reader, early) = session(request, target).await?;
    on_message(info(format!("Connected to {}", request.url.trim()), at()));
    if !target.subscribe.is_empty() {
        on_message(info(
            format!("Subscribed to {}", target.subscribe.join(", ")),
            at(),
        ));
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<(String, String, u8, bool)>>();
    let mut pending = early;
    tokio::spawn(async move {
        let mut next_id: u16 = 2;
        let mut ping = tokio::time::interval(Duration::from_secs(u64::from(KEEP_ALIVE) / 2));
        ping.tick().await;
        loop {
            for p in std::mem::take(&mut pending) {
                if let Incoming::Publish {
                    topic,
                    payload,
                    qos,
                    retain,
                    id,
                } = p
                {
                    if let Some(id) = id {
                        let _ = link.write(puback(id)).await;
                    }
                    on_message(MqttMessage {
                        direction: "received".into(),
                        topic,
                        payload: payload_text(&payload),
                        qos,
                        retain,
                        at_ms: at(),
                    });
                }
            }
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Some((topic, payload, qos, retain))) => {
                        next_id = next_id.wrapping_add(1).max(2);
                        match link.write(publish_packet(next_id, &topic, payload.as_bytes(), qos, retain)).await {
                            Ok(()) => on_message(MqttMessage { direction: "sent".into(), topic, payload, qos, retain, at_ms: at() }),
                            Err(e) => {
                                on_message(MqttMessage { direction: "error".into(), payload: e, at_ms: at(), ..Default::default() });
                                break;
                            }
                        }
                    }
                    _ => {
                        let _ = link.write(vec![0xe0, 0x00]).await;
                        on_message(info("Disconnected".into(), at()));
                        break;
                    }
                },
                _ = ping.tick() => {
                    let _ = link.write(vec![0xc0, 0x00]).await;
                }
                bytes = link.read() => match bytes {
                    Ok(Some(bytes)) => pending = reader.feed(&bytes),
                    Ok(None) => {
                        on_message(info("Closed by the broker".into(), at()));
                        break;
                    }
                    Err(e) => {
                        on_message(MqttMessage { direction: "error".into(), payload: e, at_ms: at(), ..Default::default() });
                        break;
                    }
                },
            }
        }
    });
    Ok(Live { outgoing: tx })
}

// ------------------------------------------------------------------- runs

/// For runs and flows: connects, subscribes, publishes the saved messages (name = topic)
/// and collects what arrives until it goes quiet (5 s at most). The response is `200`
/// with `[{"topic", "payload", "qos", "retain"}]` (JSON payloads as values).
pub async fn exchange(
    request: &HttpRequest,
    target: &MqttTarget,
) -> Result<HttpResponse, SendError> {
    let started = Instant::now();
    let limit = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let (mut link, mut reader, early) = session(request, target).await?;
    let mut id: u16 = 1;
    for m in &request.messages {
        id = id.wrapping_add(1).max(2);
        link.write(publish_packet(
            id,
            &m.name,
            m.body.as_bytes(),
            target.qos,
            target.retain,
        ))
        .await
        .map_err(SendError::Other)?;
    }
    let mut got = Vec::new();
    let mut size = 0usize;
    let listening = Instant::now();
    let mut packets = early;
    loop {
        for p in std::mem::take(&mut packets) {
            if let Incoming::Publish {
                topic,
                payload,
                qos,
                retain,
                id,
            } = p
            {
                if let Some(id) = id {
                    let _ = link.write(puback(id)).await;
                }
                size += payload.len();
                let text = payload_text(&payload);
                let value = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
                got.push(serde_json::json!({ "topic": topic, "payload": value, "qos": qos, "retain": retain }));
            }
        }
        let left = limit
            .saturating_sub(started.elapsed())
            .min(LISTEN.saturating_sub(listening.elapsed()));
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(QUIET.min(left), link.read()).await {
            Ok(Ok(Some(bytes))) => packets = reader.feed(&bytes),
            _ => break,
        }
    }
    let _ = link.write(vec![0xe0, 0x00]).await;
    let body = serde_json::to_string_pretty(&got).unwrap_or_default();
    Ok(HttpResponse {
        status: 200,
        status_text: "OK".into(),
        http_version: "MQTT".into(),
        headers: Vec::new(),
        body,
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
    fn encodes_and_decodes_packets() {
        let t = MqttTarget {
            username: "ana".into(),
            password: "pw".into(),
            ..Default::default()
        };
        let c = connect_packet(&t, "c1");
        assert_eq!(c[0], 0x10);
        assert_eq!(&c[2..8], b"\x00\x04MQTT");
        assert_eq!(c[9], 0xc2, "user, password, clean session");
        let p = publish_packet(7, "a/b", b"hola", 1, true);
        assert_eq!(p[0], 0x33);
        let mut r = Reader::default();
        // Split across two reads, and a long one (remaining length in 2 bytes).
        let big = publish_packet(0, "t", &[b'x'; 200], 0, false);
        assert_eq!(big[1], 0x80 | ((3 + 200) % 128) as u8);
        let mut all = p.clone();
        all.extend(&big);
        assert!(r.feed(&all[..5]).is_empty());
        let got = r.feed(&all[5..]);
        assert_eq!(
            got[0],
            Incoming::Publish {
                topic: "a/b".into(),
                payload: b"hola".to_vec(),
                qos: 1,
                retain: true,
                id: Some(7)
            }
        );
        assert!(matches!(&got[1], Incoming::Publish { payload, .. } if payload.len() == 200));
        assert_eq!(
            host_port("mqtt://broker:1884", 1883).unwrap(),
            ("broker".into(), 1884)
        );
        assert_eq!(
            host_port("mqtts://broker", 8883).unwrap(),
            ("broker".into(), 8883)
        );
        assert!(is_mqtt_url("MQTTS://x"));
    }

    /// A tiny broker: accepts user ana/pw, acks subscriptions, sends a retained message
    /// for `greetings`, and delivers every publish to the subscriber (itself).
    async fn broker() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut reader = Reader::default();
                    let mut buf = vec![0u8; 4096];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        let mut out = Vec::new();
                        for packet_bytes in split(&mut reader, &buf[..n]) {
                            let (first, body) = packet_bytes;
                            match first >> 4 {
                                1 => {
                                    let ok = body.windows(3).any(|w| w == b"ana");
                                    out.extend(packet(0x20, vec![0, if ok { 0 } else { 4 }]));
                                }
                                8 => {
                                    out.extend(packet(0x90, vec![body[0], body[1], 0]));
                                    out.extend(publish_packet(
                                        0,
                                        "greetings",
                                        b"{\"hello\":true}",
                                        0,
                                        true,
                                    ));
                                }
                                3 => {
                                    let qos = (first >> 1) & 3;
                                    let tlen = u16::from_be_bytes([body[0], body[1]]) as usize;
                                    let topic =
                                        String::from_utf8_lossy(&body[2..2 + tlen]).to_string();
                                    let mut at = 2 + tlen;
                                    if qos > 0 {
                                        out.extend(packet(0x40, body[at..at + 2].to_vec()));
                                        at += 2;
                                    }
                                    out.extend(publish_packet(
                                        0,
                                        &format!("echo/{topic}"),
                                        &body[at..],
                                        0,
                                        false,
                                    ));
                                }
                                12 => out.extend(vec![0xd0, 0]),
                                14 => return,
                                _ => {}
                            }
                        }
                        let _ = socket.write_all(&out).await;
                    }
                });
            }
        });
        format!("mqtt://{address}")
    }

    /// Raw packets (first byte, body) for the test broker.
    fn split(reader: &mut Reader, bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        reader.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while reader.buffer.len() >= 2 {
            let (mut len, mut mult, mut i) = (0usize, 1usize, 1usize);
            loop {
                let b = reader.buffer[i] as usize;
                len += (b & 0x7f) * mult;
                mult *= 128;
                i += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            if reader.buffer.len() < i + len {
                break;
            }
            out.push((reader.buffer[0], reader.buffer[i..i + len].to_vec()));
            reader.buffer.drain(..i + len);
        }
        out
    }

    fn request(url: &str) -> HttpRequest {
        let mut r = HttpRequest::new(HttpMethod::Get, url);
        r.messages = vec![crate::websocket::WsMessage {
            name: "orders/new".into(),
            body: "{\"id\": 7}".into(),
        }];
        r
    }

    fn target() -> MqttTarget {
        MqttTarget {
            username: "ana".into(),
            password: "pw".into(),
            subscribe: vec!["greetings".into(), "echo/#".into()],
            qos: 1,
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn publishes_and_collects_messages() {
        let url = broker().await;
        let response = exchange(&request(&url), &target()).await.unwrap();
        let got: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(
            got[0],
            serde_json::json!({ "topic": "greetings", "payload": { "hello": true }, "qos": 0, "retain": true })
        );
        assert_eq!(got[1]["topic"], "echo/orders/new");
        assert_eq!(got[1]["payload"]["id"], 7);
        let mut wrong = target();
        wrong.username = "eva".into();
        let refused = exchange(&request(&url), &wrong).await.unwrap_err();
        assert!(
            refused.to_string().contains("bad user name or password"),
            "{refused}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_a_live_session() {
        let url = broker().await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let live = open(&request(&url), &target(), move |m| {
            sink.lock()
                .unwrap()
                .push(format!("{} {} {}", m.direction, m.topic, m.payload))
        })
        .await
        .unwrap();
        assert!(live.publish("lights".into(), "on".into(), 0, false));
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.starts_with("received echo/lights"))
            {
                break;
            }
        }
        live.close();
        let seen = seen.lock().unwrap().clone();
        assert!(
            seen[0].starts_with("info  Connected to mqtt://"),
            "{seen:?}"
        );
        assert!(
            seen.contains(&"received greetings {\"hello\":true}".to_string()),
            "{seen:?}"
        );
        assert!(seen.contains(&"sent lights on".to_string()), "{seen:?}");
        assert!(
            seen.contains(&"received echo/lights on".to_string()),
            "{seen:?}"
        );
    }
}
