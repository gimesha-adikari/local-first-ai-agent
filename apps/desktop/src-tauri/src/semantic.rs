//! Main-window commands for local embeddings and multi-mode retrieval.

use agent_core::agent::AgentResult;
use engine::semantic::{EmbeddingModelStatus, IndexProgress};
use storage::{
    knowledge::KnowledgeSearch,
    semantic::{IndexCoverage, RetrievalMode, RetrievalResults},
};
use tauri::{State, WebviewWindow};

use crate::commands::Desktop;

#[tauri::command]
pub fn knowledge_embedding_status(state: State<'_, Desktop>) -> EmbeddingModelStatus {
    state.engine.embedding_status()
}

#[tauri::command]
pub async fn knowledge_choose_embedding_model(
    window: WebviewWindow,
    state: State<'_, Desktop>,
) -> AgentResult<Option<EmbeddingModelStatus>> {
    let lease = state.engine.begin_embedding_selection()?;
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Choose the local BGE embedding model")
        .add_filter("GGUF", &["gguf"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    state
        .engine
        .select_embedding_model(lease, file.path().to_path_buf())
        .await
        .map(Some)
}

#[tauri::command]
pub async fn knowledge_unload_embedding_model(state: State<'_, Desktop>) -> AgentResult<()> {
    state.engine.unload_embedding_model().await
}

#[tauri::command]
pub async fn knowledge_embedding_coverage(
    state: State<'_, Desktop>,
    document_ids: Option<Vec<i64>>,
) -> AgentResult<IndexCoverage> {
    state.engine.embedding_coverage(document_ids).await
}

/// Creates or replaces the vector generation for exactly one current document.
#[tauri::command]
pub fn knowledge_reindex(state: State<'_, Desktop>, document_id: i64) -> AgentResult<u32> {
    state.engine.reindex(document_id)
}

#[tauri::command]
pub fn knowledge_index_progress(
    state: State<'_, Desktop>,
    job_id: Option<u32>,
) -> AgentResult<Option<IndexProgress>> {
    state.engine.index_progress(job_id)
}

#[tauri::command]
pub fn knowledge_cancel_index(state: State<'_, Desktop>, job_id: u32) -> AgentResult<bool> {
    state.engine.cancel_index(job_id)
}

#[tauri::command]
pub async fn knowledge_retrieve(
    state: State<'_, Desktop>,
    request: KnowledgeSearch,
    mode: RetrievalMode,
) -> AgentResult<RetrievalResults> {
    state.engine.retrieve(request, mode).await
}
