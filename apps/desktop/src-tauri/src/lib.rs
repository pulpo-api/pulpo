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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![core_info])
        .run(tauri::generate_context!())
        .expect("error while running Pulpo");
}
