//! Main-window model library commands for the curated model catalog.

use agent_core::inference::Result;
use engine::{downloads::ModelCatalog, semantic::EmbeddingModelStatus};
use tauri::State;

use crate::commands::Desktop;

#[tauri::command]
pub fn model_catalog(state: State<'_, Desktop>) -> Result<ModelCatalog> {
    state.engine.model_catalog()
}

#[tauri::command]
pub async fn start_model_download(
    state: State<'_, Desktop>,
    key: &str,
    restart: bool,
) -> Result<()> {
    state.engine.start_model_download(key, restart)
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
