//! Main-window model library commands. Network consent is presented natively
//! for the exact catalog entry before the engine can open a connection.

use agent_core::inference::Result;
use engine::{downloads::ModelCatalog, semantic::EmbeddingModelStatus};
use tauri::{State, WebviewWindow};

use crate::commands::Desktop;

#[tauri::command]
pub fn model_catalog(state: State<'_, Desktop>) -> Result<ModelCatalog> {
    state.engine.model_catalog()
}

#[tauri::command]
pub async fn start_model_download(
    state: State<'_, Desktop>,
    window: WebviewWindow,
    key: &str,
    restart: bool,
) -> Result<bool> {
    let artifact = state.engine.model_download_offer(key)?;
    let action = if restart {
        "Restart download"
    } else {
        "Download model"
    };
    let mut message = format!(
        "{}\n{} bytes · {}\nLicense terms: {}\n\nThe app will contact Hugging Face and its artifact CDN to retrieve this exact model. It sends no Chat, Knowledge or other personal content.",
        artifact.name, artifact.bytes, artifact.license, artifact.license_url,
    );
    if artifact == &engine::manifest::LFM25 {
        message.push_str("\n\nThe LFM license restricts some commercial uses.");
    }
    let answer = rfd::AsyncMessageDialog::new()
        .set_parent(&window)
        .set_title(action)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::OkCancelCustom(
            action.into(),
            "Cancel".into(),
        ))
        .show()
        .await;
    if answer != rfd::MessageDialogResult::Custom(action.into()) {
        return Ok(false);
    }
    state.engine.start_model_download(key, restart)?;
    Ok(true)
}

#[tauri::command]
pub fn cancel_model_download(state: State<'_, Desktop>, key: &str) -> Result<bool> {
    state.engine.cancel_model_download(key)
}

#[tauri::command]
pub fn finish_model_installation(state: State<'_, Desktop>, key: &str) -> Result<()> {
    state.engine.finish_model_installation(key)
}

#[tauri::command]
pub async fn remove_model_download(
    state: State<'_, Desktop>,
    key: &str,
    installed: bool,
) -> Result<()> {
    state.engine.remove_model_download(key, installed).await
}

#[tauri::command]
pub async fn use_downloaded_embedding(
    state: State<'_, Desktop>,
    key: &str,
) -> Result<EmbeddingModelStatus> {
    state.engine.use_downloaded_embedding(key).await
}
