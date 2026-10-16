use pulpo_core::http::{self, HttpRequest, HttpResponse, SendError};
use pulpo_core::Protocol;
use serde::Serialize;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![core_info, send_http])
        .run(tauri::generate_context!())
        .expect("error while running Pulpo");
}
