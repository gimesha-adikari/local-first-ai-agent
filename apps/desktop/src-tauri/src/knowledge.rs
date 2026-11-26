//! Main-window IPC for explicit local knowledge operations. The host owns the
//! native picker, the consent dialog and the sandboxed parser adapter; the
//! engine owns the import job and the commit.

use std::sync::Arc;

use agent_core::agent::AgentResult;
use engine::{
    Engine,
    knowledge::{ImportJob, ImportStatus},
};
use storage::knowledge::{
    ImportOutcome, KnowledgeDocument, KnowledgeHit, KnowledgeSearch, validate_document_id,
};
use tauri::{State, WebviewWindow, async_runtime};

use crate::commands::Desktop;

#[tauri::command]
pub fn knowledge_start_import(
    window: WebviewWindow,
    state: State<'_, Desktop>,
) -> AgentResult<u32> {
    start(window, &state, None)
}

#[tauri::command]
pub fn knowledge_reimport(
    window: WebviewWindow,
    state: State<'_, Desktop>,
    document_id: i64,
) -> AgentResult<u32> {
    validate_document_id(document_id)?;
    start(window, &state, Some(document_id))
}

fn start(window: WebviewWindow, desktop: &Desktop, replace: Option<i64>) -> AgentResult<u32> {
    let job = desktop.engine.start_import()?;
    let id = job.id();
    let engine = Arc::clone(&desktop.engine);
    async_runtime::spawn(async move {
        let result = choose_and_import(window, &engine, &job, replace).await;
        let _ = job.finish(result);
    });
    Ok(id)
}

#[tauri::command]
pub fn knowledge_import_status(
    state: State<'_, Desktop>,
    job_id: Option<u32>,
) -> AgentResult<Option<ImportStatus>> {
    state.engine.import_status(job_id)
}

#[tauri::command]
pub fn knowledge_cancel_import(state: State<'_, Desktop>, job_id: u32) -> AgentResult<bool> {
    state.engine.cancel_import(job_id)
}

#[tauri::command]
pub async fn knowledge_library(
    state: State<'_, Desktop>,
) -> AgentResult<Vec<storage::knowledge::LibraryDocument>> {
    state.engine.knowledge_library().await
}

#[tauri::command]
pub async fn knowledge_list_documents(
    state: State<'_, Desktop>,
    before_id: Option<i64>,
    limit: u32,
) -> AgentResult<Vec<KnowledgeDocument>> {
    state.engine.knowledge_documents(before_id, limit).await
}

#[tauri::command]
pub async fn knowledge_get_document(
    state: State<'_, Desktop>,
    document_id: i64,
) -> AgentResult<KnowledgeDocument> {
    state.engine.knowledge_document(document_id).await
}

#[tauri::command]
pub async fn knowledge_delete_document(
    state: State<'_, Desktop>,
    document_id: i64,
) -> AgentResult<()> {
    state.engine.delete_knowledge_document(document_id).await
}

#[tauri::command]
pub async fn knowledge_search(
    state: State<'_, Desktop>,
    request: KnowledgeSearch,
) -> AgentResult<Vec<KnowledgeHit>> {
    state.engine.search_knowledge(request).await
}

#[cfg(target_os = "linux")]
mod source {
    use agent_core::{agent::AgentResult, knowledge::DocumentFormat};
    use agent_tools::knowledge::{KnowledgeFileContents, SelectedKnowledgeFile};
    use engine::knowledge::{DocumentContents, DocumentSource, ExtractedDocument, ImportCheck};

    /// A natively selected file, read and parsed by the sandboxed tool adapter.
    pub struct PinnedFile(pub SelectedKnowledgeFile);

    pub struct PinnedContents(KnowledgeFileContents);

    impl DocumentSource for PinnedFile {
        type Contents = PinnedContents;

        fn read(self, check: ImportCheck<'_>) -> AgentResult<PinnedContents> {
            self.0.read(check).map(PinnedContents)
        }
    }

    impl DocumentContents for PinnedContents {
        fn display_name(&self) -> &str {
            &self.0.display_name
        }

        fn format(&self) -> DocumentFormat {
            self.0.format
        }

        fn bytes(&self) -> &[u8] {
            self.0.bytes()
        }

        fn extract(&self, check: ImportCheck<'_>) -> AgentResult<ExtractedDocument> {
            let extracted = self.0.extract(check)?;
            Ok(ExtractedDocument {
                chunks: extracted.chunks,
                pdf_coverage: extracted.pdf_coverage,
            })
        }
    }
}

#[cfg(target_os = "linux")]
async fn choose_and_import(
    window: WebviewWindow,
    engine: &Engine,
    job: &Arc<ImportJob>,
    replace: Option<i64>,
) -> AgentResult<ImportOutcome> {
    use agent_tools::knowledge::SelectedKnowledgeFile;
    use engine::knowledge::{cancelled, worker_error};
    use rfd::{AsyncFileDialog, AsyncMessageDialog, MessageButtons, MessageDialogResult};

    if let Some(id) = replace {
        engine.knowledge_document(id).await?;
    }
    let file = AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Select a local knowledge document")
        .add_filter("TXT, Markdown and PDF", &["txt", "md", "markdown", "pdf"])
        .add_filter("All files", &["*"]) // The host also accepts uppercase suffixes.
        .pick_file()
        .await
        .ok_or_else(cancelled)?;
    let path = file.path().to_owned();
    let selected = async_runtime::spawn_blocking(move || SelectedKnowledgeFile::pin(&path))
        .await
        .map_err(|_| worker_error())??;

    job.confirming()?;
    let (title, description) = if replace.is_some() {
        (
            "Replace knowledge document?",
            "The app will save extracted text in its local database and replace the existing document and search index. The original file stays on disk.",
        )
    } else {
        (
            "Save extracted text locally?",
            "The app will save extracted text and the file name in its local database for keyword search. You can delete the imported document from the library.",
        )
    };
    let consent = AsyncMessageDialog::new()
        .set_parent(&window)
        .set_title(title)
        .set_description(description)
        .set_buttons(MessageButtons::YesNo)
        .show()
        .await;
    if consent != MessageDialogResult::Yes {
        return Err(cancelled());
    }

    engine
        .import_document(job, source::PinnedFile(selected), replace)
        .await
}

#[cfg(not(target_os = "linux"))]
async fn choose_and_import(
    _: WebviewWindow,
    _: &Engine,
    _: &Arc<ImportJob>,
    _: Option<i64>,
) -> AgentResult<ImportOutcome> {
    use agent_core::agent::{AgentError, AgentErrorCode};
    Err(AgentError::new(
        AgentErrorCode::UnsupportedPlatform,
        "Local knowledge import is supported only on Linux.",
    ))
}
