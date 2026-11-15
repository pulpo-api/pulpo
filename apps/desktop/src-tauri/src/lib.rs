use pulpo_core::certificates::{self, ClientCertificate};
use pulpo_core::codegen::{self, CodeOptions, Language};
use pulpo_core::collection::{self, CollectionError, FolderMeta, RequestEntry, Scripts, Workspace};
use pulpo_core::cookies::{self, Cookie, Jar};
use pulpo_core::data::{self, Row};
use pulpo_core::environment::{self, Environment};
use pulpo_core::flow;
use pulpo_core::http::{self, Auth, HttpRequest, HttpResponse, SendError, Settings};
use pulpo_core::import::{self, ImportSummary};
use pulpo_core::keychain::Keychain;
use pulpo_core::load::{self, LoadOptions, LoadProgress, LoadSummary};
use pulpo_core::oauth::{self, Discovery, OAuth2, TokenInfo};
use pulpo_core::organize;
use pulpo_core::report;
use pulpo_core::run::{self, RunOptions, RunReport};
use pulpo_core::variables::{self, Variables};
use pulpo_core::Protocol;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Set to stop the running load test.
#[derive(Default)]
struct LoadStop(Arc<AtomicBool>);

#[derive(Serialize)]
struct CoreInfo {
    version: &'static str,
    protocols: Vec<&'static str>,
}

#[tauri::command]
fn core_info() -> CoreInfo {
    CoreInfo {
        version: pulpo_core::VERSION,
        protocols: Protocol::ALL.iter().map(|p| p.as_str()).collect(),
    }
}

/// Resolves `{{variables}}` with the chosen environment (if any) and sends the request.
#[tauri::command]
async fn send_http(
    request: HttpRequest,
    root: Option<String>,
    environment_id: Option<String>,
) -> Result<HttpResponse, SendError> {
    let vars = match (root.as_deref(), environment_id) {
        (Some(root), Some(id)) => environment::variables_for(Path::new(&root), &id, &Keychain)
            .map_err(|e| SendError::Other(e.to_string()))?,
        _ => Variables::new(),
    };
    let resolved = variables::resolve_request(&request, &vars)
        .map_err(|missing| SendError::Unresolved(missing.join(", ")))?;
    http::send_in(&resolved, root.as_deref().map(Path::new)).await
}

/// Runs a request with its folder and own scripts. `entry` may contain unsaved edits.
/// The runner passes `options` (iteration, data row, retries, snapshots).
#[tauri::command]
async fn run_request(
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
    options: Option<RunOptions>,
) -> RunReport {
    run::run_entry_with(
        Path::new(&root),
        &entry,
        environment_id.as_deref(),
        &Keychain,
        &options.unwrap_or_default(),
    )
    .await
}

/// Rows of a CSV or JSON data file; a relative path is inside the workspace.
#[tauri::command]
async fn load_data(root: String, path: String) -> Result<Vec<Row>, String> {
    let path = PathBuf::from(&path);
    let path = if path.is_absolute() {
        path
    } else {
        Path::new(&root).join(path)
    };
    data::load_data(&path).map_err(|e| e.to_string())
}

/// A run's report as HTML (default), JUnit XML or Markdown.
#[tauri::command]
fn run_report(name: String, reports: Vec<RunReport>, format: String) -> String {
    match format.as_str() {
        "junit" => report::junit_xml(&name, &reports),
        "markdown" => report::markdown_summary(&name, &reports),
        _ => report::html_report(&name, &reports, &report::now()),
    }
}

/// Open WebSocket connections by id, with the variables to resolve typed messages.
#[derive(Default)]
struct Sockets(
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            (
                pulpo_core::websocket::Live,
                pulpo_core::variables::Variables,
            ),
        >,
    >,
);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SocketFrame {
    id: String,
    frame: pulpo_core::websocket::Frame,
}

/// Opens a WebSocket for a request; frames arrive as `ws-frame` events with this id.
#[tauri::command]
async fn ws_connect(
    app: tauri::AppHandle,
    sockets: tauri::State<'_, Sockets>,
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
) -> Result<String, String> {
    let root = PathBuf::from(root);
    let (request, vars) = run::prepare(&root, &entry, environment_id.as_deref(), &Keychain).await?;
    let id = format!("{}-{}", entry.id, uuid_like());
    let frame_id = id.clone();
    let live = pulpo_core::websocket::open(&request, move |frame| {
        let _ = app.emit(
            "ws-frame",
            SocketFrame {
                id: frame_id.clone(),
                frame,
            },
        );
    })
    .await
    .map_err(|e| e.to_string())?;
    sockets
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), (live, vars));
    Ok(id)
}

fn uuid_like() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default()
}

/// Sends a text message on an open WebSocket; `{{variables}}` are filled in.
#[tauri::command]
fn ws_send(sockets: tauri::State<'_, Sockets>, id: String, text: String) -> Result<(), String> {
    let map = sockets.0.lock().map_err(|e| e.to_string())?;
    let (live, vars) = map.get(&id).ok_or("the connection is closed")?;
    let mut missing = std::collections::BTreeSet::new();
    let resolved = pulpo_core::variables::resolve_text(&text, vars, &mut missing);
    if !missing.is_empty() {
        return Err(format!(
            "unresolved variables: {}",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if live.send(resolved) {
        Ok(())
    } else {
        Err("the connection is closed".into())
    }
}

/// Closes a WebSocket (a `Closed` frame follows).
#[tauri::command]
fn ws_close(sockets: tauri::State<'_, Sockets>, id: String) -> Result<(), String> {
    if let Some((live, _)) = sockets.0.lock().map_err(|e| e.to_string())?.remove(&id) {
        live.close();
    }
    Ok(())
}

/// The SOAP operations of a WSDL (a workspace file or a URL), with sample envelopes.
#[tauri::command]
async fn soap_operations(
    root: String,
    wsdl: String,
) -> Result<Vec<pulpo_core::soap::SoapOperation>, String> {
    let text = pulpo_core::soap::load(Path::new(&root), &wsdl).await?;
    pulpo_core::soap::operations(&text)
}

/// The methods of a gRPC request's server (reflection) or `.proto` files.
#[tauri::command]
async fn grpc_schema(
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
) -> Result<pulpo_core::grpc::GrpcSchema, String> {
    let root = PathBuf::from(root);
    let (request, _) = run::prepare(&root, &entry, environment_id.as_deref(), &Keychain).await?;
    pulpo_core::grpc::schema(&root, &request).await
}

/// The running mock server, if any: (address, folder, server).
#[derive(Default)]
struct Mocks(std::sync::Mutex<Option<(String, String, pulpo_core::mock::Mock)>>);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MockState {
    url: String,
    folder: String,
}

/// The routes a folder would serve (`""` = the whole workspace).
#[tauri::command]
fn mock_routes(root: String, folder: String) -> Result<Vec<pulpo_core::mock::Route>, String> {
    pulpo_core::mock::routes(Path::new(&root), &folder)
}

/// Starts the mock server (stopping one already running); calls arrive as `mock-hit`.
#[tauri::command]
async fn mock_start(
    app: tauri::AppHandle,
    mocks: tauri::State<'_, Mocks>,
    root: String,
    folder: String,
    port: u16,
) -> Result<MockState, String> {
    if let Some((_, _, mut old)) = mocks.0.lock().map_err(|e| e.to_string())?.take() {
        old.stop();
    }
    let mock = pulpo_core::mock::start(Path::new(&root), &folder, port, move |hit| {
        let _ = app.emit("mock-hit", hit);
    })
    .await?;
    let url = format!("http://{}", mock.address);
    *mocks.0.lock().map_err(|e| e.to_string())? = Some((url.clone(), folder.clone(), mock));
    Ok(MockState { url, folder })
}

/// Stops the mock server.
#[tauri::command]
fn mock_stop(mocks: tauri::State<'_, Mocks>) -> Result<(), String> {
    if let Some((_, _, mut mock)) = mocks.0.lock().map_err(|e| e.to_string())?.take() {
        mock.stop();
    }
    Ok(())
}

/// The running mock server, if any.
#[tauri::command]
fn mock_status(mocks: tauri::State<'_, Mocks>) -> Result<Option<MockState>, String> {
    Ok(mocks
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .map(|(url, folder, _)| MockState {
            url: url.clone(),
            folder: folder.clone(),
        }))
}

/// Live MQTT sessions by id.
#[derive(Default)]
struct Brokers(
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            (pulpo_core::mqtt::Live, pulpo_core::variables::Variables),
        >,
    >,
);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BrokerMessage {
    id: String,
    message: pulpo_core::mqtt::MqttMessage,
}

/// Connects to an MQTT broker and subscribes; messages arrive as `mqtt-message`.
#[tauri::command]
async fn mqtt_connect(
    app: tauri::AppHandle,
    brokers: tauri::State<'_, Brokers>,
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
) -> Result<String, String> {
    let root = PathBuf::from(root);
    let (request, vars) = run::prepare(&root, &entry, environment_id.as_deref(), &Keychain).await?;
    let target = request.mqtt.clone().unwrap_or_default();
    let id = format!("{}-{}", entry.id, uuid_like());
    let message_id = id.clone();
    let live = pulpo_core::mqtt::open(&request, &target, move |message| {
        let _ = app.emit(
            "mqtt-message",
            BrokerMessage {
                id: message_id.clone(),
                message,
            },
        );
    })
    .await
    .map_err(|e| e.to_string())?;
    brokers
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), (live, vars));
    Ok(id)
}

/// Publishes on a live MQTT session; `{{variables}}` in the topic and payload are filled in.
#[tauri::command]
fn mqtt_publish(
    brokers: tauri::State<'_, Brokers>,
    id: String,
    topic: String,
    payload: String,
    qos: u8,
    retain: bool,
) -> Result<(), String> {
    let map = brokers.0.lock().map_err(|e| e.to_string())?;
    let (live, vars) = map.get(&id).ok_or("the connection is closed")?;
    let mut missing = std::collections::BTreeSet::new();
    let topic = pulpo_core::variables::resolve_text(&topic, vars, &mut missing);
    let payload = pulpo_core::variables::resolve_text(&payload, vars, &mut missing);
    if !missing.is_empty() {
        return Err(format!(
            "unresolved variables: {}",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if live.publish(topic, payload, qos, retain) {
        Ok(())
    } else {
        Err("the connection is closed".into())
    }
}

/// Disconnects a live MQTT session.
#[tauri::command]
fn mqtt_close(brokers: tauri::State<'_, Brokers>, id: String) -> Result<(), String> {
    if let Some((live, _)) = brokers.0.lock().map_err(|e| e.to_string())?.remove(&id) {
        live.close();
    }
    Ok(())
}

/// Live event streams (SSE) by id.
#[derive(Default)]
struct Streams(std::sync::Mutex<std::collections::HashMap<String, pulpo_core::sse::Live>>);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamEvent {
    id: String,
    event: pulpo_core::sse::Event,
}

/// Listens to a request's event stream; events arrive as `sse-event` with this id.
#[tauri::command]
async fn sse_listen(
    app: tauri::AppHandle,
    streams: tauri::State<'_, Streams>,
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
) -> Result<String, String> {
    let root = PathBuf::from(root);
    let (request, _) = run::prepare(&root, &entry, environment_id.as_deref(), &Keychain).await?;
    let id = format!("{}-{}", entry.id, uuid_like());
    let event_id = id.clone();
    let live = pulpo_core::sse::open(&request, move |event| {
        let _ = app.emit(
            "sse-event",
            StreamEvent {
                id: event_id.clone(),
                event,
            },
        );
    })
    .await
    .map_err(|e| e.to_string())?;
    streams
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), live);
    Ok(id)
}

/// Stops listening (a `Stopped` line follows).
#[tauri::command]
fn sse_stop(streams: tauri::State<'_, Streams>, id: String) -> Result<(), String> {
    if let Some(mut live) = streams.0.lock().map_err(|e| e.to_string())?.remove(&id) {
        live.stop();
    }
    Ok(())
}

/// Runs a load test, emitting `load-progress` about once a second.
#[tauri::command]
async fn load_test(
    app: tauri::AppHandle,
    stop: tauri::State<'_, LoadStop>,
    root: String,
    entries: Vec<RequestEntry>,
    environment_id: Option<String>,
    options: LoadOptions,
) -> Result<LoadSummary, String> {
    stop.0.store(false, Ordering::Relaxed);
    let flag = stop.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = PathBuf::from(root);
        let session = run::Session::open(&root, environment_id.as_deref(), &Keychain, false)?;
        load::run_load(
            &root,
            &entries,
            &session,
            &options,
            flag,
            &|p: LoadProgress| {
                let _ = app.emit("load-progress", p);
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn stop_load_test(stop: tauri::State<'_, LoadStop>) {
    stop.0.store(true, Ordering::Relaxed);
}

#[tauri::command]
async fn folder_scripts(root: String, folder_id: String) -> Result<Scripts, CollectionError> {
    collection::folder_scripts(Path::new(&root), &folder_id)
}

#[tauri::command]
async fn list_examples(
    root: String,
    request_id: String,
) -> Result<Vec<pulpo_core::examples::Example>, CollectionError> {
    pulpo_core::examples::list_examples(Path::new(&root), &request_id)
}

#[tauri::command]
async fn save_example(
    root: String,
    request_id: String,
    example: pulpo_core::examples::Example,
) -> Result<pulpo_core::examples::Example, CollectionError> {
    pulpo_core::examples::save_example(Path::new(&root), &request_id, &example)
}

#[tauri::command]
async fn delete_example(
    root: String,
    request_id: String,
    id: String,
) -> Result<(), CollectionError> {
    pulpo_core::examples::delete_example(Path::new(&root), &request_id, &id)
}

/// Runs a JSONPath or jq query over a response body, for the response filter.
#[tauri::command]
async fn query_json(
    body: String,
    language: pulpo_core::query::QueryLanguage,
    expression: String,
) -> Result<Vec<serde_json::Value>, String> {
    pulpo_core::query::query(&body, language, &expression)
}

/// Writes a response body to a file the user picked: text as is, binary from base64.
#[tauri::command]
async fn save_response(
    path: String,
    text: Option<String>,
    base64: Option<String>,
) -> Result<(), String> {
    let bytes = match (base64, text) {
        (Some(b64), _) => http::decode_base64(&b64).ok_or("the body is not valid base64")?,
        (None, Some(text)) => text.into_bytes(),
        (None, None) => Vec::new(),
    };
    std::fs::write(&path, bytes).map_err(|e| format!("{path}: {e}"))
}

/// What a folder (`""` = the workspace) sets for the requests inside.
#[tauri::command]
async fn folder_meta(root: String, folder_id: String) -> Result<FolderMeta, CollectionError> {
    collection::folder_meta(Path::new(&root), &folder_id)
}

/// Saves headers, auth and settings of a folder (the name is kept).
#[tauri::command]
async fn save_folder_meta(
    root: String,
    folder_id: String,
    meta: FolderMeta,
) -> Result<(), CollectionError> {
    collection::save_folder_meta(Path::new(&root), &folder_id, &meta)
}

/// Renames a request or folder ("" = the workspace); returns the new id.
#[tauri::command]
async fn rename_node(
    root: String,
    id: String,
    name: String,
) -> Result<organize::Organized, CollectionError> {
    organize::rename(Path::new(&root), &id, &name)
}

/// Moves a request or folder into `target` at `index`; returns the new id.
#[tauri::command]
async fn move_node(
    root: String,
    id: String,
    target: String,
    index: usize,
) -> Result<organize::Organized, CollectionError> {
    organize::move_node(Path::new(&root), &id, &target, index)
}

/// Copies a request or folder right after it; returns the id of the copy.
#[tauri::command]
async fn duplicate_node(root: String, id: String) -> Result<organize::Organized, CollectionError> {
    organize::duplicate(Path::new(&root), &id)
}

#[tauri::command]
async fn delete_folder(root: String, id: String) -> Result<(), CollectionError> {
    organize::delete_folder(Path::new(&root), &id)
}

/// Client certificates (mTLS) of this machine, from `.pulpo/certificates.json`.
#[tauri::command]
async fn list_certificates(root: String) -> Vec<ClientCertificate> {
    certificates::load_certificates(Path::new(&root))
}

#[tauri::command]
async fn save_certificates(
    root: String,
    certificates: Vec<ClientCertificate>,
) -> Result<(), CollectionError> {
    certificates::save_certificates(Path::new(&root), &certificates)
}

/// Replaces the settings of a folder (`""` = the workspace).
#[tauri::command]
async fn save_folder_settings(
    root: String,
    folder_id: String,
    settings: Settings,
) -> Result<(), CollectionError> {
    collection::save_folder_settings(Path::new(&root), &folder_id, &settings)
}

/// The cookie jar of an environment (None = no environment), from `.pulpo/cookies.json`.
#[tauri::command]
async fn list_cookies(root: String, environment_id: Option<String>) -> Vec<Cookie> {
    cookies::load_jar(Path::new(&root), environment_id.as_deref()).cookies
}

/// Replaces the cookie jar of an environment (edits, deletes, "clear all").
#[tauri::command]
async fn save_cookies(
    root: String,
    environment_id: Option<String>,
    cookies: Vec<Cookie>,
) -> Result<(), CollectionError> {
    cookies::save_jar(
        Path::new(&root),
        environment_id.as_deref(),
        &Jar { cookies },
    )
}

#[tauri::command]
async fn save_folder_auth(
    root: String,
    folder_id: String,
    auth: Option<Auth>,
) -> Result<(), CollectionError> {
    collection::save_folder_auth(Path::new(&root), &folder_id, auth.as_ref())
}

#[tauri::command]
async fn save_folder_scripts(
    root: String,
    folder_id: String,
    scripts: Scripts,
) -> Result<(), CollectionError> {
    collection::save_folder_scripts(Path::new(&root), &folder_id, &scripts)
}

/// The request as code in another language. `entry` may contain unsaved edits; folders
/// give it their auth, headers and settings. Secrets only with `include_secrets`.
#[tauri::command]
async fn generate_code(
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
    language: Language,
    options: CodeOptions,
) -> Result<String, CollectionError> {
    codegen::for_entry(
        Path::new(&root),
        &entry,
        environment_id.as_deref(),
        &Keychain,
        language,
        options,
    )
}

/// Values set by scripts for an environment (`.pulpo/local.json`); they win over it.
#[tauri::command]
async fn local_values(
    root: String,
    environment_id: Option<String>,
) -> std::collections::HashMap<String, String> {
    pulpo_core::local::load_local(Path::new(&root), environment_id.as_deref())
}

#[tauri::command]
async fn clear_local_values(
    root: String,
    environment_id: Option<String>,
) -> Result<(), CollectionError> {
    pulpo_core::local::clear_local(Path::new(&root), environment_id.as_deref())
}

#[tauri::command]
async fn list_environments(root: String) -> Result<Vec<Environment>, CollectionError> {
    environment::list_environments(Path::new(&root), &Keychain)
}

#[tauri::command]
async fn save_environment(root: String, env: Environment) -> Result<(), CollectionError> {
    environment::save_environment(Path::new(&root), &env, &Keychain)
}

#[tauri::command]
async fn create_environment(root: String, name: String) -> Result<Environment, CollectionError> {
    environment::create_environment(Path::new(&root), &name, &Keychain)
}

#[tauri::command]
async fn delete_environment(root: String, id: String) -> Result<(), CollectionError> {
    environment::delete_environment(Path::new(&root), &id, &Keychain)
}

#[tauri::command]
async fn open_workspace(root: String) -> Result<Workspace, CollectionError> {
    collection::load_workspace(Path::new(&root))
}

#[tauri::command]
async fn init_workspace(root: String, name: String) -> Result<Workspace, CollectionError> {
    collection::init_workspace(Path::new(&root), &name)?;
    collection::load_workspace(Path::new(&root))
}

#[tauri::command]
async fn save_request(root: String, entry: RequestEntry) -> Result<(), CollectionError> {
    collection::save_request(Path::new(&root), &entry)
}

#[tauri::command]
async fn create_request(
    root: String,
    folder_id: String,
    name: String,
) -> Result<RequestEntry, CollectionError> {
    collection::create_request(Path::new(&root), &folder_id, &name)
}

#[tauri::command]
async fn create_folder(
    root: String,
    parent_id: String,
    name: String,
) -> Result<String, CollectionError> {
    collection::create_folder(Path::new(&root), &parent_id, &name)
}

#[tauri::command]
async fn delete_request(root: String, id: String) -> Result<(), CollectionError> {
    collection::delete_request(Path::new(&root), &id)
}

/// Imports a Postman collection or environment, an OpenAPI document or a curl command.
/// Exactly one of `path` (a file) or `text` (pasted content) must be given.
#[tauri::command]
async fn import_into(
    root: String,
    folder_id: String,
    path: Option<String>,
    text: Option<String>,
) -> Result<ImportSummary, String> {
    let plan = match (&path, text) {
        (Some(path), None) => import::parse_file(Path::new(path)),
        (None, Some(text)) => import::parse(&text),
        _ => return Err("give either a file or pasted text".into()),
    }
    .map_err(|e| e.to_string())?;
    let summary =
        import::apply(Path::new(&root), &folder_id, &plan, &Keychain).map_err(|e| e.to_string())?;
    if let Some(path) = path {
        pulpo_core::sync::remember(
            Path::new(&root),
            &summary.folder_id,
            plan.format,
            Path::new(&path),
        );
    }
    Ok(summary)
}

/// Finds the chain of a folder and saves it as a flow; returns the flow's id and what
/// was found.
#[tauri::command]
async fn chain_to_flow(
    root: String,
    folder_id: String,
    environment_id: Option<String>,
    run: bool,
    name: String,
) -> Result<(String, String), String> {
    blocking(move || {
        let root = PathBuf::from(root);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let chain = runtime.block_on(pulpo_core::chain::chain(
            &root,
            &folder_id,
            environment_id.as_deref().filter(|e| !e.is_empty()),
            &Keychain,
            run,
        ))?;
        let flow = flow::save_flow(&root, &pulpo_core::chain::to_flow(&chain, &name))
            .map_err(|e| e.to_string())?;
        Ok((flow.id, pulpo_core::chain::to_text(&chain)))
    })
    .await
}

/// API contracts in the workspace's repository.
#[tauri::command]
async fn scan_contracts(root: String) -> Result<Vec<pulpo_core::sync::Contract>, String> {
    blocking(move || pulpo_core::sync::scan(Path::new(&root)).map_err(|e| e.to_string())).await
}

/// Drift of every folder linked to a contract (errors are reported per folder).
#[tauri::command]
async fn sync_status(root: String) -> Result<Vec<Result<pulpo_core::sync::Drift, String>>, String> {
    blocking(move || {
        let root = PathBuf::from(root);
        let linked = pulpo_core::sync::sources(&root).map_err(|e| e.to_string())?;
        Ok(linked
            .into_iter()
            .map(|(id, _)| pulpo_core::sync::drift(&root, &id, None))
            .collect())
    })
    .await
}

/// Links a folder ("" = the workspace) to a contract file (relative to the workspace).
#[tauri::command]
async fn sync_link(
    root: String,
    folder_id: String,
    contract: String,
) -> Result<pulpo_core::sync::Drift, String> {
    blocking(move || {
        let root = PathBuf::from(root);
        let file = root.join(&contract);
        pulpo_core::sync::link(&root, &folder_id, &file)
    })
    .await
}

/// Imports a contract into a new folder linked to it.
#[tauri::command]
async fn sync_import(root: String, contract: String) -> Result<ImportSummary, String> {
    blocking(move || {
        let root = PathBuf::from(root);
        let file = root.join(&contract);
        let plan = import::parse_file(&file).map_err(|e| e.to_string())?;
        let summary = import::apply(&root, "", &plan, &Keychain).map_err(|e| e.to_string())?;
        pulpo_core::sync::remember(&root, &summary.folder_id, plan.format, &file);
        Ok(summary)
    })
    .await
}

#[tauri::command]
async fn sync_apply(
    root: String,
    folder_id: String,
    selection: pulpo_core::sync::Selection,
) -> Result<pulpo_core::sync::Applied, String> {
    blocking(move || pulpo_core::sync::apply(Path::new(&root), &folder_id, &selection)).await
}

/// Gets a new OAuth 2.0 token (browser login for the authorization code grant) and caches it.
#[tauri::command]
async fn oauth_login(
    root: String,
    environment_id: Option<String>,
    config: OAuth2,
    settings: Option<Settings>,
) -> Result<TokenInfo, String> {
    run::oauth_login(
        Path::new(&root),
        environment_id.as_deref(),
        &Keychain,
        &config,
        &settings.unwrap_or_default(),
        &oauth::open_browser,
    )
    .await
}

#[tauri::command]
async fn oauth_status(
    root: String,
    environment_id: Option<String>,
    config: OAuth2,
) -> Result<Option<TokenInfo>, String> {
    run::oauth_status(
        Path::new(&root),
        environment_id.as_deref(),
        &Keychain,
        &config,
    )
}

#[tauri::command]
async fn oauth_forget(
    root: String,
    environment_id: Option<String>,
    config: OAuth2,
) -> Result<(), String> {
    run::oauth_forget(
        Path::new(&root),
        environment_id.as_deref(),
        &Keychain,
        &config,
    )
}

/// Reads an OpenID Connect discovery document to fill the OAuth 2.0 URLs.
#[tauri::command]
async fn oauth_discover(issuer: String) -> Result<Discovery, String> {
    oauth::discover(&issuer, &Settings::default()).await
}

// ------------------------------------------------------------------------------ flows

#[tauri::command]
async fn list_flows(root: String) -> Result<Vec<flow::Flow>, String> {
    flow::list_flows(Path::new(&root)).map_err(|e| e.to_string())
}

#[tauri::command]
async fn flow_plan(root: String, id: String) -> Result<Vec<flow::PlanNode>, String> {
    flow::plan(Path::new(&root), &id).map_err(|e| e.to_string())
}

/// The flow's file as text, for the YAML view.
#[tauri::command]
async fn flow_yaml(root: String, id: String) -> Result<String, String> {
    flow::load_flow(Path::new(&root), &id)
        .map(|f| flow::to_yaml(&f))
        .map_err(|e| e.to_string())
}

/// Saves the YAML view (written back in canonical form).
#[tauri::command]
async fn save_flow_yaml(root: String, id: String, yaml: String) -> Result<flow::Flow, String> {
    let mut parsed: flow::Flow =
        serde_norway::from_str(&yaml).map_err(|e| format!("invalid flow: {e}"))?;
    parsed.id = id;
    flow::save_flow(Path::new(&root), &parsed).map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_flow(root: String, name: String) -> Result<flow::Flow, String> {
    let new = flow::Flow {
        id: String::new(),
        version: 1,
        name,
        description: String::new(),
        steps: Vec::new(),
    };
    flow::save_flow(Path::new(&root), &new).map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_flow(root: String, id: String) -> Result<(), String> {
    flow::delete_flow(Path::new(&root), &id).map_err(|e| e.to_string())
}

/// The schema of a GraphQL endpoint: the cached one, or (with `refresh`) asked again.
#[tauri::command]
async fn graphql_schema(
    root: String,
    entry: RequestEntry,
    environment_id: Option<String>,
    refresh: bool,
) -> Result<Option<pulpo_core::graphql::Schema>, String> {
    let root = Path::new(&root);
    if !refresh {
        return Ok(pulpo_core::graphql::cached_schema(root, &entry));
    }
    pulpo_core::graphql::fetch_schema(root, &entry, environment_id.as_deref(), &Keychain)
        .await
        .map(Some)
}

/// Moves a step of a flow to another position (drag and drop in the diagram).
#[tauri::command]
async fn move_flow_step(
    root: String,
    flow_id: String,
    from: usize,
    to: usize,
) -> Result<flow::Flow, String> {
    let root = Path::new(&root);
    let mut f = flow::load_flow(root, &flow_id).map_err(|e| e.to_string())?;
    if from >= f.steps.len() || to >= f.steps.len() {
        return Err(format!("the flow has {} steps", f.steps.len()));
    }
    let step = f.steps.remove(from);
    f.steps.insert(to, step);
    flow::save_flow(root, &f).map_err(|e| e.to_string())
}

/// Adds a request at the end of a flow.
#[tauri::command]
async fn add_flow_step(
    root: String,
    flow_id: String,
    request_id: String,
) -> Result<flow::Flow, String> {
    let root = Path::new(&root);
    let mut f = flow::load_flow(root, &flow_id).map_err(|e| e.to_string())?;
    f.steps.push(flow::Step::request(&request_id));
    flow::save_flow(root, &f).map_err(|e| e.to_string())
}

/// Request id → names of the flows that use it.
#[tauri::command]
async fn flow_usage(
    root: String,
) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    flow::usage(Path::new(&root)).map_err(|e| e.to_string())
}

/// Runs a flow; each step is also sent as a `flow-step` event while it runs.
#[tauri::command]
async fn run_flow(
    app: tauri::AppHandle,
    root: String,
    id: String,
    environment_id: Option<String>,
) -> Result<flow::FlowReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let mut emit = |event: &flow::StepEvent| {
            let _ = app.emit("flow-step", event);
        };
        runtime.block_on(flow::run_flow(
            Path::new(&root),
            &id,
            environment_id.as_deref(),
            &Keychain,
            &mut emit,
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The pulpo:// link to a request.
#[tauri::command]
async fn request_link(root: String, request_id: String) -> Result<String, String> {
    pulpo_core::link::make(Path::new(&root), &request_id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkTarget {
    link: pulpo_core::link::Link,
    /// The first of the given workspaces the link points to.
    root: Option<String>,
}

/// Reads a pulpo:// link and finds its workspace among `roots` (the recent ones).
#[tauri::command]
async fn resolve_link(link: String, roots: Vec<String>) -> Result<LinkTarget, String> {
    blocking(move || {
        let link = pulpo_core::link::parse(&link)?;
        let root = pulpo_core::link::find(&roots, &link).map(str::to_string);
        Ok(LinkTarget { link, root })
    })
    .await
}

/// pulpo:// links the app was started with (clicked while it was closed).
#[derive(Default)]
struct StartupLinks(std::sync::Mutex<Vec<String>>);

#[tauri::command]
fn startup_links(state: tauri::State<'_, StartupLinks>) -> Vec<String> {
    state
        .0
        .lock()
        .map(|mut links| std::mem::take(&mut *links))
        .unwrap_or_default()
}

fn links_in<I: IntoIterator<Item = String>>(items: I) -> Vec<String> {
    items
        .into_iter()
        .filter(|a| a.starts_with(pulpo_core::link::SCHEME))
        .collect()
}

/// What the git panel shows: status, branches and the collection's changes since HEAD.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitPanel {
    status: pulpo_core::git::Status,
    branches: Vec<String>,
    /// `None` before the first commit.
    changes: Option<pulpo_core::diff::CollectionDiff>,
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn git_panel(root: String) -> Result<GitPanel, String> {
    blocking(move || {
        let root = PathBuf::from(root);
        let status = pulpo_core::git::status(&root)?;
        if !status.repo {
            return Ok(GitPanel {
                status,
                branches: Vec::new(),
                changes: None,
            });
        }
        Ok(GitPanel {
            branches: pulpo_core::git::branches(&root).unwrap_or_default(),
            changes: pulpo_core::diff::diff_refs(&root, "HEAD", None).ok(),
            status,
        })
    })
    .await
}

#[tauri::command]
async fn git_commit(root: String, message: String) -> Result<String, String> {
    blocking(move || pulpo_core::git::commit(Path::new(&root), &message)).await
}

#[tauri::command]
async fn git_switch(root: String, branch: String, create: bool) -> Result<(), String> {
    blocking(move || pulpo_core::git::switch(Path::new(&root), &branch, create)).await
}

#[tauri::command]
async fn git_pull(root: String) -> Result<String, String> {
    blocking(move || pulpo_core::git::pull(Path::new(&root))).await
}

#[tauri::command]
async fn git_push(root: String) -> Result<String, String> {
    blocking(move || pulpo_core::git::push(Path::new(&root))).await
}

/// Exports a folder ("" = the workspace) as Postman, OpenAPI or a .http file.
#[tauri::command]
async fn export_collection(
    root: String,
    folder_id: String,
    format: pulpo_core::export::ExportFormat,
) -> Result<pulpo_core::export::Exported, String> {
    pulpo_core::export::export(Path::new(&root), &folder_id, format).map_err(|e| e.to_string())
}

/// Exports an environment as a Postman environment (secret values left empty).
#[tauri::command]
async fn export_environment(
    root: String,
    environment_id: String,
) -> Result<pulpo_core::export::Exported, String> {
    pulpo_core::export::export_environment(Path::new(&root), &environment_id, &Keychain)
        .map_err(|e| e.to_string())
}

/// Writes an export where the user chose in the save dialog.
#[tauri::command]
async fn write_text_file(path: String, text: String) -> Result<(), String> {
    std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))
}

/// Every cached OAuth 2.0 token of an environment and the requests that use it.
#[tauri::command]
async fn oauth_tokens(
    root: String,
    environment_id: Option<String>,
) -> Result<Vec<run::ActiveToken>, String> {
    run::oauth_tokens(Path::new(&root), environment_id.as_deref(), &Keychain)
}

#[tauri::command]
async fn oauth_renew(
    root: String,
    environment_id: Option<String>,
    key: String,
) -> Result<TokenInfo, String> {
    run::oauth_renew(
        Path::new(&root),
        environment_id.as_deref(),
        &Keychain,
        &key,
        &oauth::open_browser,
    )
    .await
}

/// Revokes a token at the identity provider (when it can) and forgets it.
#[tauri::command]
async fn oauth_revoke(
    root: String,
    environment_id: Option<String>,
    key: String,
) -> Result<Option<String>, String> {
    run::oauth_revoke(Path::new(&root), environment_id.as_deref(), &Keychain, &key).await
}

#[tauri::command]
async fn oauth_forget_key(
    root: String,
    environment_id: Option<String>,
    key: String,
) -> Result<(), String> {
    run::oauth_forget_key(Path::new(&root), environment_id.as_deref(), &Keychain, &key)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Must be the first plugin: a second launch hands its pulpo:// link to this window.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
        let links = links_in(argv);
        if !links.is_empty() {
            let _ = app.emit("pulpo-link", links);
        }
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }));
    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(LoadStop::default())
        .manage(Sockets::default())
        .manage(Streams::default())
        .manage(Brokers::default())
        .manage(Mocks::default())
        .manage(StartupLinks(std::sync::Mutex::new(links_in(
            std::env::args().skip(1),
        ))))
        .setup(|app| {
            use tauri_plugin_deep_link::DeepLinkExt;
            // Installers register the scheme; this covers portable builds and `tauri dev`.
            #[cfg(any(windows, target_os = "linux"))]
            let _ = app.deep_link().register_all();
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                if let Ok(mut links) = app.state::<StartupLinks>().0.lock() {
                    for url in links_in(urls.iter().map(|u| u.to_string())) {
                        if !links.contains(&url) {
                            links.push(url);
                        }
                    }
                }
            }
            let handle = app.handle().clone();
            // macOS sends links to the running app this way.
            app.deep_link().on_open_url(move |event| {
                let links = links_in(event.urls().iter().map(|u| u.to_string()));
                if !links.is_empty() {
                    let _ = handle.emit("pulpo-link", links);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_info,
            send_http,
            open_workspace,
            init_workspace,
            save_request,
            create_request,
            create_folder,
            delete_request,
            list_environments,
            save_environment,
            create_environment,
            delete_environment,
            import_into,
            run_request,
            folder_scripts,
            save_folder_scripts,
            save_folder_auth,
            list_cookies,
            save_cookies,
            save_response,
            query_json,
            list_examples,
            load_data,
            run_report,
            load_test,
            stop_load_test,
            save_example,
            delete_example,
            folder_meta,
            save_folder_meta,
            rename_node,
            move_node,
            duplicate_node,
            delete_folder,
            list_certificates,
            save_certificates,
            save_folder_settings,
            local_values,
            generate_code,
            clear_local_values,
            oauth_login,
            oauth_status,
            oauth_forget,
            oauth_discover,
            oauth_tokens,
            oauth_renew,
            oauth_revoke,
            oauth_forget_key,
            export_collection,
            export_environment,
            write_text_file,
            git_panel,
            git_commit,
            git_switch,
            git_pull,
            git_push,
            request_link,
            resolve_link,
            startup_links,
            list_flows,
            flow_plan,
            flow_yaml,
            save_flow_yaml,
            create_flow,
            delete_flow,
            add_flow_step,
            move_flow_step,
            graphql_schema,
            ws_connect,
            ws_send,
            ws_close,
            sse_listen,
            sse_stop,
            grpc_schema,
            soap_operations,
            mqtt_connect,
            mock_routes,
            mock_start,
            mock_stop,
            mock_status,
            mqtt_publish,
            mqtt_close,
            flow_usage,
            run_flow,
            scan_contracts,
            chain_to_flow,
            sync_status,
            sync_link,
            sync_import,
            sync_apply
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pulpo");
}
