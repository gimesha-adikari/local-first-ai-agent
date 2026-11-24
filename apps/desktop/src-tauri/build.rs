fn main() {
    let app_manifest = tauri_build::AppManifest::new().commands(&["core_status"]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);
    tauri_build::try_build(attributes).expect("failed to configure the Tauri application");
}
