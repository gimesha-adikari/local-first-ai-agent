fn main() {
    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "choose_model",
        "inference_status",
        "load_model",
        "unload_model",
        "generate",
        "stop_generation",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);
    tauri_build::try_build(attributes).expect("failed to configure the Tauri application");
}
