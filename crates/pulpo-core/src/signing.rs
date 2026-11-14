//! Auth that is computed from the request itself (AWS Signature v4, Hawk) or needs a
//! challenge from the server first (Digest, NTLM).

use crate::import::base64;
use hmac::{Hmac, Mac};
use md5::Md5;
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DigestAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AwsV4 {
    pub access_key: String,
    pub secret_key: String,
    /// For temporary credentials (`x-amz-security-token`).
    pub session_token: String,
    pub region: String,
    pub service: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HawkAlgorithm {
    #[default]
    Sha256,
    Sha1,
}

impl HawkAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            HawkAlgorithm::Sha256 => "sha256",
            HawkAlgorithm::Sha1 => "sha1",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hawk {
    pub id: String,
    pub key: String,
    pub algorithm: HawkAlgorithm,
    pub ext: String,
    /// Also sign the body.
    pub payload: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ntlm {
    pub username: String,
    pub password: String,
    pub domain: String,
    pub workstation: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC takes any key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_md5(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Md5>::new_from_slice(key).expect("HMAC takes any key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn set_header(request: &mut reqwest::Request, name: &str, value: &str) -> Result<(), String> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?;
    let value = HeaderValue::from_str(value).map_err(|e| format!("{name}: {e}"))?;
    request.headers_mut().insert(name, value);
    Ok(())
}

/// `Host` as the client will send it.
fn host_of(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

/// `(year, month, day, hour, minute, second)` in UTC.
fn utc(epoch: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (epoch / 86_400) as i64;
    let secs = epoch % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, secs / 3600, secs % 3600 / 60, secs % 60)
}

// ---------------------------------------------------------------- AWS Signature v4

/// URI encoding as AWS wants it: everything but `A-Z a-z 0-9 - _ . ~` (and `/` if asked).
fn aws_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let digits = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(digits, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Headers that change on the way or that the client adds later are not signed.
const UNSIGNED: [&str; 5] = [
    "authorization",
    "user-agent",
    "connection",
    "expect",
    "content-length",
];

/// Signs the request with AWS Signature v4 at `now` (seconds since the epoch).
pub fn sign_aws(request: &mut reqwest::Request, config: &AwsV4, now: u64) -> Result<(), String> {
    let (y, mo, d, h, mi, s) = utc(now);
    let date = format!("{y:04}{mo:02}{d:02}");
    let amz_date = format!("{date}T{h:02}{mi:02}{s:02}Z");
    let payload_hash = match request.body().and_then(|b| b.as_bytes()) {
        Some(bytes) => hex(&Sha256::digest(bytes)),
        None if request.body().is_none() => hex(&Sha256::digest(b"")),
        None => "UNSIGNED-PAYLOAD".to_string(),
    };
    let s3 = config.service == "s3";
    set_header(request, "x-amz-date", &amz_date)?;
    if s3 {
        set_header(request, "x-amz-content-sha256", &payload_hash)?;
    }
    if !config.session_token.is_empty() {
        set_header(request, "x-amz-security-token", &config.session_token)?;
    }

    let url = request.url().clone();
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    // S3 encodes the path once, the other services twice.
    let path = aws_encode(&percent_decode(path), true);
    let path = if s3 { path } else { aws_encode(&path, true) };
    let mut query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (aws_encode(&k, false), aws_encode(&v, false)))
        .collect();
    query.sort();
    let query = query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let mut headers: Vec<(String, String)> = vec![("host".into(), host_of(&url))];
    for (name, value) in request.headers() {
        let name = name.as_str().to_ascii_lowercase();
        if UNSIGNED.contains(&name.as_str()) || name == "host" {
            continue;
        }
        let value = String::from_utf8_lossy(value.as_bytes());
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        match headers.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => {
                existing.push(',');
                existing.push_str(&value);
            }
            None => headers.push((name, value)),
        }
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        request.method().as_str()
    );
    let scope = format!("{date}/{}/{}/aws4_request", config.region, config.service);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut key = hmac_sha256(
        format!("AWS4{}", config.secret_key).as_bytes(),
        date.as_bytes(),
    );
    for part in [
        config.region.as_str(),
        config.service.as_str(),
        "aws4_request",
    ] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    let signature = hex(&hmac_sha256(&key, to_sign.as_bytes()));
    set_header(
        request,
        "authorization",
        &format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            config.access_key
        ),
    )
}

// ---------------------------------------------------------------- Hawk

fn hawk_hash(algorithm: HawkAlgorithm, data: &[u8]) -> String {
    match algorithm {
        HawkAlgorithm::Sha256 => base64(&Sha256::digest(data)),
        HawkAlgorithm::Sha1 => base64(&Sha1::digest(data)),
    }
}

/// The `Authorization: Hawk ...` value for a request.
pub fn hawk_header(
    config: &Hawk,
    method: &str,
    url: &reqwest::Url,
    content_type: &str,
    payload: Option<&[u8]>,
    ts: u64,
    nonce: &str,
) -> String {
    let resource = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or(80);
    let hash = match payload.filter(|_| config.payload) {
        Some(body) => {
            let content_type = content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let mut data = format!("hawk.1.payload\n{content_type}\n").into_bytes();
            data.extend_from_slice(body);
            data.push(b'\n');
            hawk_hash(config.algorithm, &data)
        }
        None => String::new(),
    };
    let ext = config.ext.replace('\\', "\\\\").replace('\n', "\\n");
    let normalized = format!(
        "hawk.1.header\n{ts}\n{nonce}\n{}\n{resource}\n{host}\n{port}\n{hash}\n{ext}\n",
        method.to_ascii_uppercase()
    );
    let mac = match config.algorithm {
        HawkAlgorithm::Sha256 => hmac_sha256(config.key.as_bytes(), normalized.as_bytes()),
        HawkAlgorithm::Sha1 => hmac_sha1(config.key.as_bytes(), normalized.as_bytes()),
    };
    let mut header = format!(r#"Hawk id="{}", ts="{ts}", nonce="{nonce}""#, config.id);
    if !hash.is_empty() {
        header.push_str(&format!(r#", hash="{hash}""#));
    }
    if !config.ext.is_empty() {
        header.push_str(&format!(r#", ext="{}""#, config.ext.replace('"', "\\\"")));
    }
    header.push_str(&format!(r#", mac="{}""#, base64(&mac)));
    header
}

/// Signs the request with Hawk.
pub fn sign_hawk(request: &mut reqwest::Request, config: &Hawk, ts: u64) -> Result<(), String> {
    let nonce: String = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let content_type = request
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let payload = match request.body() {
        None => Some(&b""[..]),
        Some(body) => body.as_bytes(),
    };
    let header = hawk_header(
        config,
        request.method().as_str(),
        request.url(),
        &content_type,
        payload,
        ts,
        &nonce,
    );
    set_header(request, "authorization", &header)
}

// ---------------------------------------------------------------- Digest

/// The parameters of a `WWW-Authenticate: <scheme> k=v, k="v"` challenge.
fn challenge_params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let name = rest[..eq]
            .trim()
            .trim_start_matches(',')
            .trim()
            .to_ascii_lowercase();
        rest = rest[eq + 1..].trim_start();
        let value;
        if let Some(stripped) = rest.strip_prefix('"') {
            let mut text = String::new();
            let mut chars = stripped.char_indices();
            let mut end = stripped.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, next)) = chars.next() {
                            text.push(next);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => text.push(c),
                }
            }
            value = text;
            rest = stripped[end.min(stripped.len())..].trim_start();
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        rest = rest.trim_start_matches(',').trim_start();
        out.push((name, value));
    }
    out
}

/// The challenges of a scheme in `WWW-Authenticate` headers (case-insensitive).
pub fn find_challenge<'a>(headers: &'a [String], scheme: &str) -> Option<&'a str> {
    headers.iter().find_map(|h| {
        let h = h.trim();
        let (name, rest) = h.split_once(char::is_whitespace).unwrap_or((h, ""));
        name.eq_ignore_ascii_case(scheme).then_some(rest.trim())
    })
}

fn digest_hash(algorithm: &str, data: &str) -> Result<String, String> {
    let base = algorithm
        .trim_end_matches("-sess")
        .trim_end_matches("-SESS");
    match base.to_ascii_uppercase().as_str() {
        "" | "MD5" => Ok(hex(&Md5::digest(data.as_bytes()))),
        "SHA-256" => Ok(hex(&Sha256::digest(data.as_bytes()))),
        other => Err(format!("Digest algorithm {other} is not supported")),
    }
}

/// The `Authorization: Digest ...` answer to a challenge.
#[allow(clippy::too_many_arguments)]
pub fn digest_header(
    config: &DigestAuth,
    challenge: &str,
    method: &str,
    uri: &str,
    body: Option<&[u8]>,
    cnonce: &str,
    nc: u32,
) -> Result<String, String> {
    let params = challenge_params(challenge);
    let get = |name: &str| {
        params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let realm = get("realm");
    let nonce = get("nonce");
    let algorithm = get("algorithm");
    let offered: Vec<&str> = get("qop").split(',').map(str::trim).collect();
    let qop = if offered.contains(&"auth") {
        "auth"
    } else if offered.contains(&"auth-int") && body.is_some() {
        "auth-int"
    } else if get("qop").is_empty() {
        ""
    } else {
        return Err(format!("Digest qop \"{}\" is not supported", get("qop")));
    };
    let nc = format!("{nc:08x}");
    let mut ha1 = digest_hash(
        algorithm,
        &format!("{}:{realm}:{}", config.username, config.password),
    )?;
    if algorithm.to_ascii_lowercase().ends_with("-sess") {
        ha1 = digest_hash(algorithm, &format!("{ha1}:{nonce}:{cnonce}"))?;
    }
    let ha2 = if qop == "auth-int" {
        let body_hash = match algorithm.to_ascii_uppercase().trim_end_matches("-SESS") {
            "SHA-256" => hex(&Sha256::digest(body.unwrap_or_default())),
            _ => hex(&Md5::digest(body.unwrap_or_default())),
        };
        digest_hash(algorithm, &format!("{method}:{uri}:{body_hash}"))?
    } else {
        digest_hash(algorithm, &format!("{method}:{uri}"))?
    };
    let response = if qop.is_empty() {
        digest_hash(algorithm, &format!("{ha1}:{nonce}:{ha2}"))?
    } else {
        digest_hash(
            algorithm,
            &format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"),
        )?
    };
    let mut header = format!(
        r#"Digest username="{}", realm="{realm}", nonce="{nonce}", uri="{uri}""#,
        config.username.replace('"', "\\\"")
    );
    if !algorithm.is_empty() {
        header.push_str(&format!(", algorithm={algorithm}"));
    }
    header.push_str(&format!(r#", response="{response}""#));
    if !qop.is_empty() {
        header.push_str(&format!(r#", qop={qop}, nc={nc}, cnonce="{cnonce}""#));
    }
    let opaque = get("opaque");
    if !opaque.is_empty() {
        header.push_str(&format!(r#", opaque="{opaque}""#));
    }
    Ok(header)
}

// ---------------------------------------------------------------- NTLM (v2)

const NTLM_UNICODE: u32 = 0x0000_0001;
const NTLM_OEM: u32 = 0x0000_0002;
const NTLM_REQUEST_TARGET: u32 = 0x0000_0004;
const NTLM_NTLM: u32 = 0x0000_0200;
const NTLM_ALWAYS_SIGN: u32 = 0x0000_8000;
const NTLM_EXTENDED: u32 = 0x0008_0000;
const NTLM_TARGET_INFO: u32 = 0x0080_0000;
const NTLM_FLAGS: u32 =
    NTLM_UNICODE | NTLM_OEM | NTLM_REQUEST_TARGET | NTLM_NTLM | NTLM_ALWAYS_SIGN | NTLM_EXTENDED;
/// `MsvAvTimestamp` in the target info.
const AV_TIMESTAMP: u16 = 7;

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// The first message: `Authorization: NTLM <this>`.
pub fn ntlm_negotiate() -> String {
    let mut message = b"NTLMSSP\0".to_vec();
    message.extend_from_slice(&1u32.to_le_bytes());
    message.extend_from_slice(&NTLM_FLAGS.to_le_bytes());
    message.extend_from_slice(&[0; 16]); // no domain or workstation
    base64(&message)
}

pub(crate) fn ntowf_v2(config: &Ntlm) -> Vec<u8> {
    use md4::{Digest as _, Md4};
    let hash = Md4::digest(utf16(&config.password));
    let identity = utf16(&format!(
        "{}{}",
        config.username.to_uppercase(),
        config.domain
    ));
    hmac_md5(&hash, &identity)
}

struct Challenge {
    flags: u32,
    server_challenge: [u8; 8],
    target_info: Vec<u8>,
}

fn parse_ntlm_challenge(data: &[u8]) -> Result<Challenge, String> {
    let bad = || "the server sent an invalid NTLM challenge".to_string();
    if data.len() < 32 || &data[..8] != b"NTLMSSP\0" || data[8] != 2 {
        return Err(bad());
    }
    let u16_at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
    let flags = u32::from_le_bytes(data[20..24].try_into().map_err(|_| bad())?);
    let server_challenge = data[24..32].try_into().map_err(|_| bad())?;
    let target_info = if data.len() >= 48 {
        let (len, offset) = (
            u16_at(40),
            u32::from_le_bytes(data[44..48].try_into().map_err(|_| bad())?) as usize,
        );
        data.get(offset..offset + len).ok_or_else(bad)?.to_vec()
    } else {
        Vec::new()
    };
    Ok(Challenge {
        flags,
        server_challenge,
        target_info,
    })
}

/// The server's `MsvAvTimestamp`, if the target info has one.
fn av_timestamp(target_info: &[u8]) -> Option<[u8; 8]> {
    let mut i = 0;
    while i + 4 <= target_info.len() {
        let id = u16::from_le_bytes([target_info[i], target_info[i + 1]]);
        let len = u16::from_le_bytes([target_info[i + 2], target_info[i + 3]]) as usize;
        let value = target_info.get(i + 4..i + 4 + len)?;
        if id == 0 {
            return None;
        }
        if id == AV_TIMESTAMP && len == 8 {
            return value.try_into().ok();
        }
        i += 4 + len;
    }
    None
}

/// `NTProofStr` and the NTLMv2 response (`NTProofStr` + the client blob).
pub(crate) fn ntlm_v2_response(
    ntowf: &[u8],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
    timestamp: [u8; 8],
    target_info: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let mut blob = vec![1, 1, 0, 0, 0, 0, 0, 0];
    blob.extend_from_slice(&timestamp);
    blob.extend_from_slice(client_challenge);
    blob.extend_from_slice(&[0; 4]);
    blob.extend_from_slice(target_info);
    blob.extend_from_slice(&[0; 4]);
    let mut data = server_challenge.to_vec();
    data.extend_from_slice(&blob);
    let proof = hmac_md5(ntowf, &data);
    let mut response = proof.clone();
    response.extend_from_slice(&blob);
    (proof, response)
}

/// Windows FILETIME: 100 ns since 1601-01-01.
fn filetime(epoch: u64) -> [u8; 8] {
    ((epoch + 11_644_473_600) * 10_000_000).to_le_bytes()
}

/// The answer to the server's challenge (`WWW-Authenticate: NTLM <challenge>`).
pub fn ntlm_authenticate(config: &Ntlm, challenge: &str, now: u64) -> Result<String, String> {
    let data = crate::http::decode_base64(challenge.trim())
        .ok_or("the server sent an invalid NTLM challenge")?;
    let challenge = parse_ntlm_challenge(&data)?;
    let mut client_challenge = [0u8; 8];
    client_challenge.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
    let server_time = av_timestamp(&challenge.target_info);
    let ntowf = ntowf_v2(config);
    let (_, nt_response) = ntlm_v2_response(
        &ntowf,
        &challenge.server_challenge,
        &client_challenge,
        server_time.unwrap_or_else(|| filetime(now)),
        &challenge.target_info,
    );
    // With a server timestamp the LMv2 response must be zeros.
    let lm_response = if server_time.is_some() {
        vec![0; 24]
    } else {
        let mut data = challenge.server_challenge.to_vec();
        data.extend_from_slice(&client_challenge);
        let mut lm = hmac_md5(&ntowf, &data);
        lm.extend_from_slice(&client_challenge);
        lm
    };
    let fields = [
        lm_response,
        nt_response,
        utf16(&config.domain),
        utf16(&config.username),
        utf16(&config.workstation),
        Vec::new(), // no session key
    ];
    let mut message = b"NTLMSSP\0".to_vec();
    message.extend_from_slice(&3u32.to_le_bytes());
    let mut offset = 8 + 4 + fields.len() * 8 + 4;
    for field in &fields {
        let len = field.len() as u16;
        message.extend_from_slice(&len.to_le_bytes());
        message.extend_from_slice(&len.to_le_bytes());
        message.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += field.len();
    }
    let flags = (challenge.flags & (NTLM_FLAGS | NTLM_TARGET_INFO)) | NTLM_UNICODE | NTLM_NTLM;
    message.extend_from_slice(&flags.to_le_bytes());
    for field in &fields {
        message.extend_from_slice(field);
    }
    Ok(base64(&message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn converts_epoch_seconds_to_utc() {
        assert_eq!(utc(1_440_938_160), (2015, 8, 30, 12, 36, 0));
        assert_eq!(utc(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(utc(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn aws_signature_matches_the_get_vanilla_example() {
        let client = reqwest::Client::new();
        let mut request = client
            .get("https://example.amazonaws.com/")
            .build()
            .unwrap();
        let config = AwsV4 {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
            service: "service".into(),
            ..Default::default()
        };
        sign_aws(&mut request, &config, 1_440_938_160).unwrap();
        assert_eq!(
            request.headers()["authorization"],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert_eq!(request.headers()["x-amz-date"], "20150830T123600Z");
    }

    #[test]
    fn aws_signs_s3_payloads_and_session_tokens() {
        let client = reqwest::Client::new();
        let mut request = client
            .put("https://bucket.s3.amazonaws.com/a b.txt?x=1")
            .body("hello")
            .build()
            .unwrap();
        let config = AwsV4 {
            access_key: "AK".into(),
            secret_key: "SK".into(),
            session_token: "TOKEN".into(),
            region: "eu-west-1".into(),
            service: "s3".into(),
        };
        sign_aws(&mut request, &config, 1_440_938_160).unwrap();
        let auth = request.headers()["authorization"].to_str().unwrap();
        assert!(
            auth.contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
            ),
            "{auth}"
        );
        assert_eq!(
            request.headers()["x-amz-content-sha256"],
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn hawk_matches_the_reference_examples() {
        let config = Hawk {
            id: "dh37fgj492je".into(),
            key: "werxhqb98rpaxn39848xrunpaw3489ruxnpa98w4rxn".into(),
            algorithm: HawkAlgorithm::Sha256,
            ext: "some-app-ext-data".into(),
            payload: false,
        };
        let url = reqwest::Url::parse("http://example.com:8000/resource/1?b=1&a=2").unwrap();
        let header = hawk_header(&config, "GET", &url, "", None, 1_353_832_234, "j4h3g2");
        assert_eq!(
            header,
            r#"Hawk id="dh37fgj492je", ts="1353832234", nonce="j4h3g2", ext="some-app-ext-data", mac="6R4rV5iE+NPoym+WwjeHzjAGXUtLNIxmo1vpMofpLAE=""#
        );

        let config = Hawk {
            payload: true,
            ..config
        };
        let header = hawk_header(
            &config,
            "POST",
            &url,
            "text/plain",
            Some(b"Thank you for flying Hawk"),
            1_353_832_234,
            "j4h3g2",
        );
        assert!(
            header.contains(r#"hash="Yi9LfIIFRtBEPt74PVmbTF/xVAwPn7ub15ePICfgnuY=""#),
            "{header}"
        );
        assert!(
            header.ends_with(r#"mac="aSe1DERmZuRl3pI36/9BdZmnErTw3sNzOOAUlfeKjVw=""#),
            "{header}"
        );
    }

    #[test]
    fn digest_matches_the_rfc_2617_example() {
        let config = DigestAuth {
            username: "Mufasa".into(),
            password: "Circle Of Life".into(),
        };
        let challenge = r#"realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let header = digest_header(
            &config,
            challenge,
            "GET",
            "/dir/index.html",
            None,
            "0a4f113b",
            1,
        )
        .unwrap();
        assert!(
            header.contains(r#"response="6629fae49393a05397450978507c4ef1""#),
            "{header}"
        );
        assert!(header.contains("qop=auth, nc=00000001"), "{header}");
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
    }

    #[test]
    fn reads_challenges_from_www_authenticate() {
        let headers = vec![
            "Basic realm=\"x\"".to_string(),
            "digest realm=\"a, b\", nonce=n1, algorithm=SHA-256".to_string(),
        ];
        let challenge = find_challenge(&headers, "Digest").unwrap();
        let params = challenge_params(challenge);
        assert_eq!(
            params,
            vec![
                ("realm".to_string(), "a, b".to_string()),
                ("nonce".to_string(), "n1".to_string()),
                ("algorithm".to_string(), "SHA-256".to_string()),
            ]
        );
        assert!(find_challenge(&headers, "NTLM").is_none());
    }

    #[test]
    fn ntlm_v2_matches_the_ms_nlmp_example() {
        let config = Ntlm {
            username: "User".into(),
            password: "Password".into(),
            domain: "Domain".into(),
            workstation: "COMPUTER".into(),
        };
        let ntowf = ntowf_v2(&config);
        assert_eq!(hex(&ntowf), "0c868a403bfd7a93a3001ef22ef02e3f");
        let target_info =
            unhex("02000c0044006f006d00610069006e0001000c0053006500720076006500720000000000");
        let (proof, _) = ntlm_v2_response(
            &ntowf,
            &unhex("0123456789abcdef").try_into().unwrap(),
            &[0xaa; 8],
            [0; 8],
            &target_info,
        );
        assert_eq!(hex(&proof), "68cd0ab851e51c96aabc927bebef6a1c");
    }

    #[test]
    fn round_trips_through_the_request_file() {
        use crate::collection::{init_workspace, load_request, save_request, RequestEntry};
        use crate::http::{Auth, HttpRequest};
        use crate::HttpMethod;
        let dir = tempfile::tempdir().unwrap();
        init_workspace(dir.path(), "Demo").unwrap();
        let auths = [
            Auth::Digest(DigestAuth {
                username: "ana".into(),
                password: "{{password}}".into(),
            }),
            Auth::AwsV4(AwsV4 {
                access_key: "{{ak}}".into(),
                secret_key: "{{sk}}".into(),
                session_token: String::new(),
                region: "eu-west-1".into(),
                service: "s3".into(),
            }),
            Auth::Hawk(Hawk {
                id: "id".into(),
                key: "{{hawkKey}}".into(),
                algorithm: HawkAlgorithm::Sha1,
                ext: "x".into(),
                payload: true,
            }),
            Auth::Ntlm(Ntlm {
                username: "ana".into(),
                password: "{{password}}".into(),
                domain: "CORP".into(),
                workstation: String::new(),
            }),
        ];
        for auth in auths {
            let mut request = HttpRequest::new(HttpMethod::Get, "https://api.example/me");
            request.auth = Some(auth);
            let entry = RequestEntry {
                id: "01-me.pulpo.yaml".into(),
                name: "me".into(),
                protocol: crate::Protocol::Http,
                request,
                scripts: Default::default(),
            };
            save_request(dir.path(), &entry).unwrap();
            assert_eq!(
                load_request(dir.path(), &entry.id).unwrap().request.auth,
                entry.request.auth
            );
        }
    }

    #[test]
    fn ntlm_messages_round_trip() {
        let negotiate = crate::http::decode_base64(&ntlm_negotiate()).unwrap();
        assert_eq!(&negotiate[..9], b"NTLMSSP\0\x01");
        // A challenge with a target info holding a timestamp.
        let mut info = vec![7, 0, 8, 0];
        info.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        info.extend_from_slice(&[0, 0, 0, 0]);
        let mut challenge = b"NTLMSSP\0".to_vec();
        challenge.extend_from_slice(&2u32.to_le_bytes());
        challenge.extend_from_slice(&[0; 8]); // target name
        challenge.extend_from_slice(&(NTLM_FLAGS | NTLM_TARGET_INFO).to_le_bytes());
        challenge.extend_from_slice(&[9; 8]); // server challenge
        challenge.extend_from_slice(&[0; 8]); // reserved
        challenge.extend_from_slice(&(info.len() as u16).to_le_bytes());
        challenge.extend_from_slice(&(info.len() as u16).to_le_bytes());
        challenge.extend_from_slice(&48u32.to_le_bytes());
        challenge.extend_from_slice(&info);
        let config = Ntlm {
            username: "ana".into(),
            password: "pw".into(),
            domain: "CORP".into(),
            workstation: String::new(),
        };
        let answer = ntlm_authenticate(&config, &base64(&challenge), 0).unwrap();
        let answer = crate::http::decode_base64(&answer).unwrap();
        assert_eq!(&answer[..9], b"NTLMSSP\0\x03");
        // LM response: 24 zeros because the server sent a timestamp.
        let lm_offset = u32::from_le_bytes(answer[16..20].try_into().unwrap()) as usize;
        assert_eq!(&answer[lm_offset..lm_offset + 24], &[0; 24]);
        // The user name, in UTF-16.
        let user_len = u16::from_le_bytes([answer[36], answer[37]]) as usize;
        let user_offset = u32::from_le_bytes(answer[40..44].try_into().unwrap()) as usize;
        assert_eq!(answer[user_offset..user_offset + user_len], utf16("ana"));
    }
}
