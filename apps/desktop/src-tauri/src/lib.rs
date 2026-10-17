use pulpo_core::certificates::{self, ClientCertificate};
use pulpo_core::codegen::{self, CodeOptions, Language};
use pulpo_core::collection::{self, CollectionError, FolderMeta, RequestEntry, Scripts, Workspace};
use pulpo_core::cookies::{self, Cookie, Jar};
use pulpo_core::data::{self, Row};
use pulpo_core::environment::{self, Environment};
use pulpo_core::http::{self, Auth, HttpRequest, HttpResponse, SendError, Settings};
use pulpo_core::import::{self, ImportSummary};
use pulpo_core::keychain::Keychain;
use pulpo_core::load::{self, LoadOptions, LoadProgress, LoadSummary};
use pulpo_core::organize;
use pulpo_core::report;
use pulpo_core::run::{self, RunOptions, RunReport};
use pulpo_core::variables::{self, Variables};
use pulpo_core::Protocol;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;

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
    let plan = match (path, text) {
        (Some(path), None) => import::parse_file(Path::new(&path)),
        (None, Some(text)) => import::parse(&text),
        _ => return Err("give either a file or pasted text".into()),
    }
    .map_err(|e| e.to_string())?;
    import::apply(Path::new(&root), &folder_id, &plan, &Keychain).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(LoadStop::default())
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
            clear_local_values
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pulpo");
}
