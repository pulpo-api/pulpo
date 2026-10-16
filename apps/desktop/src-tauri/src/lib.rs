use pulpo_core::collection::{self, CollectionError, RequestEntry, Workspace};
use pulpo_core::http::{self, HttpRequest, HttpResponse, SendError};
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

#[tauri::command]
async fn send_http(request: HttpRequest) -> Result<HttpResponse, SendError> {
    http::send(&request).await
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
            delete_request
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pulpo");
}
