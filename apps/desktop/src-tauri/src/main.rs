#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn core_status() -> &'static str {
    agent_core::status()
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![core_status])
        .run(tauri::generate_context!())
        .expect("failed to run the desktop application");
}
