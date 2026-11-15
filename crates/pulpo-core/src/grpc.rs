//! gRPC: a request whose URL is `grpc://host:port` (or `grpcs://` with TLS).
//!
//! The methods come from the server (reflection) or from `.proto` files of the workspace,
//! compiled by Pulpo itself (no `protoc`). Messages are written as JSON and turned into
//! protobuf with the method's descriptor; replies come back as JSON (a list when the
//! server streams). Headers go as metadata.

use crate::http::{Header, HttpRequest, HttpResponse, SendError};
use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::Status;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a gRPC request calls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcTarget {
    /// `package.Service/Method`.
    #[serde(default)]
    pub method: String,
    /// Workspace `.proto` files with the service; empty = ask the server (reflection).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proto_files: Vec<String>,
}

/// A method the app can pick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcMethod {
    /// `package.Service/Method`.
    pub path: String,
    pub service: String,
    pub name: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
    pub input: String,
    pub output: String,
    /// A JSON message with every field, to start from.
    pub example: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcSchema {
    /// `reflection` or `proto files`.
    pub source: String,
    pub methods: Vec<GrpcMethod>,
}

pub fn is_grpc_url(url: &str) -> bool {
    let u = url.trim_start().to_ascii_lowercase();
    u.starts_with("grpc://") || u.starts_with("grpcs://")
}

/// `grpc://h:p` → `http://h:p`, `grpcs://` → `https://`.
fn endpoint_url(url: &str) -> String {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    let rest = |n: usize| u[n..].trim_end_matches('/').to_string();
    if lower.starts_with("grpcs://") {
        format!("https://{}", rest(8))
    } else if lower.starts_with("grpc://") {
        format!("http://{}", rest(7))
    } else {
        u.trim_end_matches('/').to_string()
    }
}

async fn channel(request: &HttpRequest) -> Result<Channel, SendError> {
    let url = endpoint_url(&request.url);
    let timeout = request
        .settings
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let mut endpoint = Endpoint::from_shared(url.clone())
        .map_err(|e| SendError::InvalidUrl(format!("{url}: {e}")))?
        .connect_timeout(timeout)
        .timeout(timeout)
        .user_agent(crate::http::USER_AGENT)
        .map_err(|e| SendError::Other(e.to_string()))?;
    if url.starts_with("https://") {
        if request.settings.verify_ssl == Some(false) {
            return Err(SendError::Other(
                "gRPC does not skip certificate checks yet; turn \"Verify SSL\" back on".into(),
            ));
        }
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|e| SendError::Other(e.to_string()))?;
    }
    endpoint
        .connect()
        .await
        .map_err(|e| SendError::Connect(error_chain(&e)))
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(s) = source {
        out.push_str(&format!(": {s}"));
        source = s.source();
    }
    out
}

// ------------------------------------------------------------------- descriptors

/// Compiles workspace `.proto` files (imports are looked up next to each file and from
/// the workspace root).
pub fn pool_from_protos(root: &Path, files: &[String]) -> Result<DescriptorPool, String> {
    let mut includes: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    let mut names = Vec::new();
    for f in files {
        let path = crate::collection::find_in_workspace(root, f)
            .ok_or_else(|| format!("no .proto file {f} in the workspace"))?;
        if let Some(dir) = path.parent() {
            if !includes.contains(&dir.to_path_buf()) {
                includes.insert(0, dir.to_path_buf());
            }
        }
        names.push(path);
    }
    let mut compiler = protox::Compiler::new(includes).map_err(|e| e.to_string())?;
    compiler.include_imports(true);
    compiler.open_files(names).map_err(|e| e.to_string())?;
    Ok(compiler.descriptor_pool())
}

/// Asks the server for its services and their files (reflection v1, else v1alpha).
pub async fn pool_from_reflection(channel: Channel) -> Result<DescriptorPool, String> {
    match reflection_v1(channel.clone()).await {
        Ok(pool) => Ok(pool),
        Err(v1) => reflection_v1alpha(channel)
            .await
            .map_err(|alpha| format!("the server does not answer reflection ({v1}; {alpha}); add its .proto files instead")),
    }
}

macro_rules! reflection {
    ($name:ident, $pb:path) => {
        async fn $name(channel: Channel) -> Result<DescriptorPool, String> {
            use pb::server_reflection_client::ServerReflectionClient;
            use pb::server_reflection_request::MessageRequest;
            use pb::server_reflection_response::MessageResponse;
            use $pb as pb;
            let mut client = ServerReflectionClient::new(channel);
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<pb::ServerReflectionRequest>();
            let ask = |m: MessageRequest| pb::ServerReflectionRequest {
                host: String::new(),
                message_request: Some(m),
            };
            tx.send(ask(MessageRequest::ListServices(String::new())))
                .map_err(|e| e.to_string())?;
            let mut stream = client
                .server_reflection_info(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
                .await
                .map_err(|e| e.message().to_string())?
                .into_inner();
            let mut asked = 1usize;
            let mut files: std::collections::BTreeMap<String, prost_types::FileDescriptorProto> =
                Default::default();
            let mut wanted: Vec<String> = Vec::new();
            while asked > 0 {
                let Some(reply) = stream
                    .message()
                    .await
                    .map_err(|e| e.message().to_string())?
                else {
                    break;
                };
                asked -= 1;
                match reply.message_response {
                    Some(MessageResponse::ListServicesResponse(list)) => {
                        for s in list.service {
                            if s.name.starts_with("grpc.reflection.") {
                                continue;
                            }
                            tx.send(ask(MessageRequest::FileContainingSymbol(s.name)))
                                .map_err(|e| e.to_string())?;
                            asked += 1;
                        }
                    }
                    Some(MessageResponse::FileDescriptorResponse(found)) => {
                        for bytes in found.file_descriptor_proto {
                            let file = prost_types::FileDescriptorProto::decode(bytes.as_slice())
                                .map_err(|e| e.to_string())?;
                            for dep in &file.dependency {
                                if !files.contains_key(dep) && !wanted.contains(dep) {
                                    wanted.push(dep.clone());
                                    tx.send(ask(MessageRequest::FileByFilename(dep.clone())))
                                        .map_err(|e| e.to_string())?;
                                    asked += 1;
                                }
                            }
                            files.insert(file.name().to_string(), file);
                        }
                    }
                    Some(MessageResponse::ErrorResponse(e)) => {
                        return Err(e.error_message);
                    }
                    _ => {}
                }
            }
            drop(tx);
            let mut pool = DescriptorPool::new();
            // Add files after their dependencies.
            let mut left: Vec<_> = files.into_values().collect();
            while !left.is_empty() {
                let before = left.len();
                let mut rest = Vec::new();
                for file in left {
                    let ready = file
                        .dependency
                        .iter()
                        .all(|d| pool.get_file_by_name(d).is_some());
                    if ready {
                        pool.add_file_descriptor_proto(file)
                            .map_err(|e| e.to_string())?;
                    } else {
                        rest.push(file);
                    }
                }
                if rest.len() == before {
                    let missing: Vec<String> = rest.iter().map(|f| f.name().to_string()).collect();
                    return Err(format!("missing dependencies for {}", missing.join(", ")));
                }
                left = rest;
            }
            Ok(pool)
        }
    };
}
reflection!(reflection_v1, tonic_reflection::pb::v1);
reflection!(reflection_v1alpha, tonic_reflection::pb::v1alpha);

/// Every method of a pool, with an example message.
pub fn methods(pool: &DescriptorPool) -> Vec<GrpcMethod> {
    let mut out = Vec::new();
    for service in pool.services() {
        if service.full_name().starts_with("grpc.reflection.") {
            continue;
        }
        for m in service.methods() {
            out.push(GrpcMethod {
                path: format!("{}/{}", service.full_name(), m.name()),
                service: service.full_name().to_string(),
                name: m.name().to_string(),
                client_streaming: m.is_client_streaming(),
                server_streaming: m.is_server_streaming(),
                input: m.input().full_name().to_string(),
                output: m.output().full_name().to_string(),
                example: serde_json::to_string_pretty(&example(&m.input(), 0)).unwrap_or_default(),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// A JSON message with every field set to a sample value.
fn example(message: &MessageDescriptor, depth: usize) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for field in message.fields() {
        let value = example_field(&field, depth);
        let value = if field.is_list() {
            serde_json::Value::Array(vec![value])
        } else if field.is_map() {
            serde_json::json!({})
        } else {
            value
        };
        out.insert(field.json_name().to_string(), value);
    }
    serde_json::Value::Object(out)
}

fn example_field(field: &FieldDescriptor, depth: usize) -> serde_json::Value {
    use serde_json::json;
    match field.kind() {
        Kind::Double | Kind::Float => json!(0.0),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 | Kind::Uint32 | Kind::Fixed32 => json!(0),
        // 64-bit numbers travel as strings in protobuf JSON.
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 | Kind::Uint64 | Kind::Fixed64 => json!("0"),
        Kind::Bool => json!(false),
        Kind::String => json!(""),
        Kind::Bytes => json!(""),
        Kind::Enum(e) => json!(e
            .values()
            .next()
            .map(|v| v.name().to_string())
            .unwrap_or_default()),
        Kind::Message(m) => match m.full_name() {
            "google.protobuf.Timestamp" => json!("1970-01-01T00:00:00Z"),
            "google.protobuf.Duration" => json!("1s"),
            "google.protobuf.Struct" | "google.protobuf.Any" => json!({}),
            "google.protobuf.StringValue" => json!(""),
            _ if depth >= 3 => json!({}),
            _ => example(&m, depth + 1),
        },
    }
}

/// The methods of a request's service: from its `.proto` files, else reflection.
pub async fn schema(root: &Path, request: &HttpRequest) -> Result<GrpcSchema, String> {
    let target = request.grpc.clone().unwrap_or_default();
    let (pool, source) = load_pool(root, request, &target).await?;
    Ok(GrpcSchema {
        source,
        methods: methods(&pool),
    })
}

async fn load_pool(
    root: &Path,
    request: &HttpRequest,
    target: &GrpcTarget,
) -> Result<(DescriptorPool, String), String> {
    if target.proto_files.is_empty() {
        let channel = channel(request).await.map_err(|e| e.to_string())?;
        Ok((pool_from_reflection(channel).await?, "reflection".into()))
    } else {
        Ok((
            pool_from_protos(root, &target.proto_files)?,
            "proto files".into(),
        ))
    }
}

/// An import plan for a `.proto` file: a folder per service, a request per method with
/// an example message, sent to `{{baseUrl}}` (an environment with
/// `grpc://localhost:50051`).
pub fn proto_plan(path: &Path) -> Result<crate::import::ImportPlan, String> {
    use crate::import::{ImportFormat, ImportNode, ImportPlan};
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut compiler = protox::Compiler::new([dir]).map_err(|e| e.to_string())?;
    compiler.include_imports(true);
    compiler.open_files([&name]).map_err(|e| e.to_string())?;
    let all = methods(&compiler.descriptor_pool());
    if all.is_empty() {
        return Err(format!("{name} has no services"));
    }
    let mut folders: Vec<(String, Vec<ImportNode>)> = Vec::new();
    for m in all {
        let mut request = HttpRequest::new(crate::HttpMethod::Post, "{{baseUrl}}");
        request.body = Some(m.example.clone());
        request.grpc = Some(GrpcTarget {
            method: m.path.clone(),
            proto_files: vec![name.clone()],
        });
        let node = ImportNode::Request {
            name: m.name.clone(),
            request,
            scripts: Default::default(),
        };
        match folders.iter_mut().find(|(s, _)| *s == m.service) {
            Some((_, list)) => list.push(node),
            None => folders.push((m.service.clone(), vec![node])),
        }
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "gRPC".into());
    let nodes = if folders.len() == 1 {
        folders.pop().map(|(_, n)| n).unwrap_or_default()
    } else {
        folders
            .into_iter()
            .map(|(name, children)| ImportNode::Folder {
                name,
                children,
                scripts: Default::default(),
                auth: None,
            })
            .collect()
    };
    Ok(ImportPlan {
        format: ImportFormat::Proto,
        folder: Some(stem.clone()),
        folder_scripts: Default::default(),
        folder_auth: None,
        nodes,
        environments: vec![crate::environment::Environment {
            id: String::new(),
            name: format!("{stem} local"),
            variables: vec![crate::environment::Variable {
                name: "baseUrl".into(),
                value: "grpc://localhost:50051".into(),
                secret: false,
                has_value: true,
            }],
        }],
        warnings: Vec::new(),
    })
}

// ------------------------------------------------------------------- calling

/// Encodes and decodes messages known only at run time.
#[derive(Clone)]
pub struct DynamicCodec {
    decode: MessageDescriptor,
}

impl DynamicCodec {
    pub fn new(decode: MessageDescriptor) -> Self {
        DynamicCodec { decode }
    }
}

pub struct DynamicEncoder;
pub struct DynamicDecoder(MessageDescriptor);

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;
    fn encode(&mut self, item: DynamicMessage, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("could not encode the message: {e}")))
    }
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<DynamicMessage>, Status> {
        DynamicMessage::decode(self.0.clone(), src)
            .map(Some)
            .map_err(|e| Status::internal(format!("could not decode the reply: {e}")))
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;
    fn encoder(&mut self) -> DynamicEncoder {
        DynamicEncoder
    }
    fn decoder(&mut self) -> DynamicDecoder {
        DynamicDecoder(self.decode.clone())
    }
}

/// JSON (protobuf's JSON mapping) to a message of the given type.
pub fn from_json(message: &MessageDescriptor, text: &str) -> Result<DynamicMessage, String> {
    let text = if text.trim().is_empty() { "{}" } else { text };
    let mut de = serde_json::Deserializer::from_str(text);
    let out = DynamicMessage::deserialize(message.clone(), &mut de)
        .map_err(|e| format!("the message does not fit {}: {e}", message.full_name()))?;
    de.end().map_err(|e| e.to_string())?;
    Ok(out)
}

fn to_json(message: &DynamicMessage) -> serde_json::Value {
    let options = prost_reflect::SerializeOptions::new().skip_default_fields(false);
    message
        .serialize_with_options(serde_json::value::Serializer, &options)
        .unwrap_or(serde_json::Value::Null)
}

fn find_method(pool: &DescriptorPool, path: &str) -> Result<MethodDescriptor, String> {
    let (service, method) = path
        .trim()
        .trim_start_matches('/')
        .rsplit_once('/')
        .ok_or_else(|| format!("pick a method (package.Service/Method), not '{path}'"))?;
    let service = pool
        .get_service_by_name(service)
        .ok_or_else(|| format!("no service {service}"))?;
    let found = service.methods().find(|m| m.name() == method);
    found.ok_or_else(|| format!("{} has no method {method}", service.full_name()))
}

/// The HTTP status closest to a gRPC code, so the app and tests read errors as usual.
fn http_status(code: tonic::Code) -> u16 {
    use tonic::Code::*;
    match code {
        Ok => 200,
        InvalidArgument | FailedPrecondition | OutOfRange => 400,
        Unauthenticated => 401,
        PermissionDenied => 403,
        NotFound => 404,
        AlreadyExists | Aborted => 409,
        ResourceExhausted => 429,
        Cancelled => 499,
        Unimplemented => 501,
        Unavailable => 503,
        DeadlineExceeded => 504,
        _ => 500,
    }
}

fn metadata_headers(map: &tonic::metadata::MetadataMap) -> Vec<Header> {
    map.clone()
        .into_headers()
        .iter()
        .map(|(n, v)| Header::new(n.as_str(), v.to_str().unwrap_or("(binary)")))
        .collect()
}

/// Calls the request's method with its message (unary and server streaming: the body;
/// client and bidirectional streaming: the saved messages, else the body).
pub async fn invoke(root: &Path, request: &HttpRequest) -> Result<HttpResponse, SendError> {
    let started = Instant::now();
    let target = request.grpc.clone().unwrap_or_default();
    let (pool, _) = load_pool(root, request, &target)
        .await
        .map_err(SendError::Other)?;
    let method = find_method(&pool, &target.method).map_err(SendError::Other)?;
    let input = method.input();
    let texts: Vec<String> = if method.is_client_streaming() && !request.messages.is_empty() {
        request.messages.iter().map(|m| m.body.clone()).collect()
    } else {
        vec![request.body.clone().unwrap_or_default()]
    };
    let mut messages = Vec::new();
    for text in &texts {
        messages.push(from_json(&input, text).map_err(SendError::Other)?);
    }
    let channel = channel(request).await?;
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready()
        .await
        .map_err(|e| SendError::Connect(error_chain(&e)))?;
    let path: tonic::codegen::http::uri::PathAndQuery =
        format!("/{}/{}", method.parent_service().full_name(), method.name())
            .parse()
            .map_err(|e| SendError::Other(format!("{e}")))?;
    let mut call = tonic::Request::new(tokio_stream::iter(messages));
    for h in request
        .headers
        .iter()
        .filter(|h| h.enabled && !h.name.trim().is_empty())
    {
        let name = h.name.trim().to_ascii_lowercase();
        if ["content-type", "te", "user-agent", "accept"].contains(&name.as_str()) {
            continue;
        }
        let key = tonic::metadata::MetadataKey::from_bytes(name.as_bytes())
            .map_err(|e| SendError::InvalidHeader(format!("{}: {e}", h.name)))?;
        let value = h
            .value
            .parse()
            .map_err(|e| SendError::InvalidHeader(format!("{}: {e}", h.name)))?;
        call.metadata_mut().insert(key, value);
    }
    let codec = DynamicCodec::new(method.output());
    let result = grpc.streaming(call, path, codec).await;
    let (status, mut headers, body) = match result {
        Err(status) => (status, Vec::new(), None),
        Ok(response) => {
            let mut headers = metadata_headers(response.metadata());
            let mut stream = response.into_inner();
            let mut got = Vec::new();
            let outcome = loop {
                match stream.message().await {
                    Ok(Some(m)) => got.push(to_json(&m)),
                    Ok(None) => break Status::ok(""),
                    Err(status) => break status,
                }
            };
            if let Ok(Some(trailers)) = stream.trailers().await {
                headers.extend(metadata_headers(&trailers));
            }
            // A streaming server answers a list; the others, one message.
            let body = if method.is_server_streaming() {
                serde_json::Value::Array(got)
            } else {
                got.into_iter().next().unwrap_or(serde_json::Value::Null)
            };
            (outcome, headers, Some(body))
        }
    };
    headers.push(Header::new(
        "grpc-status",
        (status.code() as i32).to_string(),
    ));
    if !status.message().is_empty() {
        headers.push(Header::new("grpc-message", status.message()));
    }
    let ok = status.code() == tonic::Code::Ok;
    let body = match (ok, body) {
        (true, Some(b)) => b,
        _ => serde_json::json!({
            "code": format!("{:?}", status.code()),
            "message": status.message(),
        }),
    };
    let text = serde_json::to_string_pretty(&body).unwrap_or_default();
    Ok(HttpResponse {
        status: http_status(status.code()),
        status_text: if ok {
            "OK".into()
        } else {
            format!("{:?}: {}", status.code(), status.message())
        },
        http_version: "gRPC".into(),
        headers,
        size_bytes: text.len(),
        body: text,
        body_is_text: true,
        body_base64: None,
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
    use tonic::codegen::{http, BoxFuture, Context, Poll, Service};
    use tonic::server::{
        ClientStreamingService, Grpc, NamedService, ServerStreamingService, UnaryService,
    };

    const PROTO: &str = r#"syntax = "proto3";
package test;
import "google/protobuf/timestamp.proto";
enum Mood { HAPPY = 0; SAD = 1; }
message SayRequest { string name = 1; int64 times = 2; Mood mood = 3; google.protobuf.Timestamp at = 4; }
message SayReply { string text = 1; repeated string tags = 2; }
service Echo {
  rpc Say(SayRequest) returns (SayReply);
  rpc Count(SayRequest) returns (stream SayReply);
  rpc Join(stream SayRequest) returns (SayReply);
}
"#;

    fn workspace() -> (tempfile::TempDir, DescriptorPool) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("protos")).unwrap();
        std::fs::write(dir.path().join("protos/echo.proto"), PROTO).unwrap();
        let pool = pool_from_protos(dir.path(), &["protos/echo.proto".to_string()]).unwrap();
        (dir, pool)
    }

    fn reply(pool: &DescriptorPool, text: &str, tags: &[&str]) -> DynamicMessage {
        let json = serde_json::json!({ "text": text, "tags": tags }).to_string();
        from_json(&pool.get_message_by_name("test.SayReply").unwrap(), &json).unwrap()
    }

    fn field(m: &DynamicMessage, name: &str) -> String {
        match m.get_field_by_name(name).as_deref() {
            Some(prost_reflect::Value::String(s)) => s.clone(),
            Some(prost_reflect::Value::I64(n)) => n.to_string(),
            _ => String::new(),
        }
    }

    struct Say(DescriptorPool);
    impl UnaryService<DynamicMessage> for Say {
        type Response = DynamicMessage;
        type Future = BoxFuture<tonic::Response<DynamicMessage>, Status>;
        fn call(&mut self, request: tonic::Request<DynamicMessage>) -> Self::Future {
            let pool = self.0.clone();
            Box::pin(async move {
                let token = request
                    .metadata()
                    .get("x-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string();
                let name = field(request.get_ref(), "name");
                if name == "nobody" {
                    return Err(Status::not_found("no such person"));
                }
                Ok(tonic::Response::new(reply(
                    &pool,
                    &format!("hola {name}"),
                    &[&token],
                )))
            })
        }
    }

    struct Count(DescriptorPool);
    impl ServerStreamingService<DynamicMessage> for Count {
        type Response = DynamicMessage;
        type ResponseStream =
            tokio_stream::Iter<std::vec::IntoIter<Result<DynamicMessage, Status>>>;
        type Future = BoxFuture<tonic::Response<Self::ResponseStream>, Status>;
        fn call(&mut self, request: tonic::Request<DynamicMessage>) -> Self::Future {
            let pool = self.0.clone();
            Box::pin(async move {
                let times: usize = field(request.get_ref(), "times").parse().unwrap_or(0);
                let items: Vec<_> = (1..=times)
                    .map(|n| Ok(reply(&pool, &n.to_string(), &[])))
                    .collect();
                Ok(tonic::Response::new(tokio_stream::iter(items)))
            })
        }
    }

    struct Join(DescriptorPool);
    impl ClientStreamingService<DynamicMessage> for Join {
        type Response = DynamicMessage;
        type Future = BoxFuture<tonic::Response<DynamicMessage>, Status>;
        fn call(
            &mut self,
            request: tonic::Request<tonic::Streaming<DynamicMessage>>,
        ) -> Self::Future {
            let pool = self.0.clone();
            Box::pin(async move {
                let mut stream = request.into_inner();
                let mut names = Vec::new();
                while let Some(m) = stream.message().await? {
                    names.push(field(&m, "name"));
                }
                Ok(tonic::Response::new(reply(&pool, &names.join("+"), &[])))
            })
        }
    }

    #[derive(Clone)]
    struct Echo(DescriptorPool);
    impl NamedService for Echo {
        const NAME: &'static str = "test.Echo";
    }
    impl<B> Service<http::Request<B>> for Echo
    where
        B: tonic::codegen::Body + Send + 'static,
        B::Error: Into<tonic::codegen::StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::Body>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, request: http::Request<B>) -> Self::Future {
            let pool = self.0.clone();
            let input = pool.get_message_by_name("test.SayRequest").unwrap();
            let path = request.uri().path().to_string();
            Box::pin(async move {
                let mut grpc = Grpc::new(DynamicCodec::new(input));
                Ok(match path.as_str() {
                    "/test.Echo/Say" => grpc.unary(Say(pool), request).await,
                    "/test.Echo/Count" => grpc.server_streaming(Count(pool), request).await,
                    _ => grpc.client_streaming(Join(pool), request).await,
                })
            })
        }
    }

    /// A gRPC server with reflection and the Echo service; returns `grpc://127.0.0.1:port`.
    async fn server(pool: DescriptorPool) -> String {
        let set: &'static [u8] = Box::leak(pool.encode_to_vec().into_boxed_slice());
        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(set)
            .build_v1()
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(reflection)
                .add_service(Echo(pool))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        format!("grpc://{address}")
    }

    fn request(url: &str, method: &str, body: &str) -> HttpRequest {
        let mut r = HttpRequest::new(HttpMethod::Post, url);
        r.grpc = Some(GrpcTarget {
            method: method.into(),
            proto_files: Vec::new(),
        });
        r.body = Some(body.into());
        r.headers.push(Header::new("x-token", "t-1"));
        r
    }

    #[test]
    fn compiles_protos_and_writes_examples() {
        let (_dir, pool) = workspace();
        let all = methods(&pool);
        let say = all.iter().find(|m| m.path == "test.Echo/Say").unwrap();
        assert!(!say.client_streaming && !say.server_streaming);
        let example: serde_json::Value = serde_json::from_str(&say.example).unwrap();
        assert_eq!(
            example,
            serde_json::json!({ "name": "", "times": "0", "mood": "HAPPY", "at": "1970-01-01T00:00:00Z" })
        );
        assert!(all
            .iter()
            .any(|m| m.path == "test.Echo/Count" && m.server_streaming));
        assert!(all
            .iter()
            .any(|m| m.path == "test.Echo/Join" && m.client_streaming));
        let wrong = from_json(
            &pool.get_message_by_name("test.SayRequest").unwrap(),
            r#"{"nmae": 1}"#,
        );
        assert!(wrong.unwrap_err().contains("does not fit test.SayRequest"));
        assert!(is_grpc_url("grpcs://x:443"));
        assert_eq!(endpoint_url("grpc://h:50051/"), "http://h:50051");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_the_schema_by_reflection_and_calls_every_kind() {
        let (dir, pool) = workspace();
        let url = server(pool).await;
        let root = dir.path();

        let found = schema(root, &request(&url, "", "")).await.unwrap();
        assert_eq!(found.source, "reflection");
        let paths: Vec<&str> = found.methods.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            paths,
            ["test.Echo/Count", "test.Echo/Join", "test.Echo/Say"]
        );

        let said = invoke(root, &request(&url, "test.Echo/Say", r#"{"name": "Ana"}"#))
            .await
            .unwrap();
        assert_eq!(said.status, 200, "{}", said.status_text);
        let body: serde_json::Value = serde_json::from_str(&said.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "text": "hola Ana", "tags": ["t-1"] })
        );
        assert!(said
            .headers
            .iter()
            .any(|h| h.name == "grpc-status" && h.value == "0"));

        let counted = invoke(root, &request(&url, "test.Echo/Count", r#"{"times": "3"}"#))
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&counted.body).unwrap();
        assert_eq!(body.as_array().unwrap().len(), 3);
        assert_eq!(body[2]["text"], "3");

        let mut join = request(&url, "test.Echo/Join", "");
        join.messages = ["a", "b", "c"]
            .iter()
            .map(|n| crate::websocket::WsMessage {
                name: n.to_string(),
                body: format!(r#"{{"name": "{n}"}}"#),
            })
            .collect();
        let joined = invoke(root, &join).await.unwrap();
        assert!(joined.body.contains("a+b+c"), "{}", joined.body);

        let missing = invoke(
            root,
            &request(&url, "test.Echo/Say", r#"{"name": "nobody"}"#),
        )
        .await
        .unwrap();
        assert_eq!(missing.status, 404);
        assert_eq!(missing.status_text, "NotFound: no such person");

        let mut from_files = request(&url, "test.Echo/Say", r#"{"name": "Eva"}"#);
        from_files.grpc.as_mut().unwrap().proto_files = vec!["protos/echo.proto".into()];
        let said = invoke(root, &from_files).await.unwrap();
        assert!(said.body.contains("hola Eva"));

        let error = invoke(root, &request("grpc://127.0.0.1:9", "test.Echo/Say", "{}"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().starts_with("could not connect"),
            "{error}"
        );
    }

    #[test]
    fn saves_grpc_requests() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::collection::init_workspace(root, "Rpc").unwrap();
        let mut r = request(
            "grpc://localhost:50051",
            "test.Echo/Say",
            "{\n  \"name\": \"Ana\"\n}",
        );
        r.grpc.as_mut().unwrap().proto_files = vec!["protos/echo.proto".into()];
        let entry = crate::collection::RequestEntry {
            id: "01-say.pulpo.yaml".into(),
            name: "Say".into(),
            protocol: crate::Protocol::Http,
            request: r,
            scripts: Default::default(),
        };
        crate::collection::save_request(root, &entry).unwrap();
        let yaml = std::fs::read_to_string(root.join("01-say.pulpo.yaml")).unwrap();
        assert!(yaml.contains("protocol: \"grpc\""), "{yaml}");
        assert!(
            yaml.contains(
                "grpc:\n  method: \"test.Echo/Say\"\n  protoFiles:\n    - \"protos/echo.proto\"\n"
            ),
            "{yaml}"
        );
        assert!(root.join("01-say.body.json").exists());
        let back = crate::collection::load_request(root, "01-say.pulpo.yaml").unwrap();
        assert_eq!(back.request.grpc, entry.request.grpc);
        assert_eq!(back.protocol, crate::Protocol::Grpc);
    }
}
