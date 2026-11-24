fn main() {
    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "bootstrap",
        "list_conversations",
        "get_conversation",
        "create_conversation",
        "rename_conversation",
        "set_system_prompt",
        "delete_conversation",
        "send_message",
        "regenerate",
        "stop_generation",
        "choose_model",
        "load_model",
        "save_model_settings",
        "unload_model",
        "save_preferences",
        "inference_status",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);
    tauri_build::try_build(attributes).expect("failed to configure the Tauri application");
}
