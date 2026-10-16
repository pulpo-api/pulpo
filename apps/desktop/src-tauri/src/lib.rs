use pulpo_core::collection::{self, CollectionError, RequestEntry, Workspace};
use pulpo_core::environment::{self, Environment};
use pulpo_core::http::{self, HttpRequest, HttpResponse, SendError};
use pulpo_core::keychain::Keychain;
use pulpo_core::variables::{self, Variables};
use pulpo_core::Protocol;
use serde::Serialize;
use std::path::Path;

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
    let vars = match (root, environment_id) {
        (Some(root), Some(id)) => environment::variables_for(Path::new(&root), &id, &Keychain)
            .map_err(|e| SendError::Other(e.to_string()))?,
        _ => Variables::new(),
    };
    let resolved = variables::resolve_request(&request, &vars)
        .map_err(|missing| SendError::Unresolved(missing.join(", ")))?;
    http::send(&resolved).await
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
            delete_environment
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pulpo");
}
