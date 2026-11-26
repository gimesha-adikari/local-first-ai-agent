//! Main-window IPC for explicit local knowledge operations.

use std::sync::{Arc, Mutex, MutexGuard};

use agent_core::agent::{AgentError, AgentErrorCode, AgentResult};
use serde::Serialize;
use storage::{
    SqliteStore,
    knowledge::{
        ImportOutcome, KnowledgeDocument, KnowledgeHit, KnowledgeSearch, storage_error,
        validate_document_id,
    },
};
use tauri::{State, WebviewWindow, async_runtime};

use crate::commands::Desktop;

#[derive(Default)]
pub struct KnowledgeJobs(Mutex<Option<Arc<ImportJob>>>);

struct ImportJob {
    id: u32,
    state: Mutex<ImportState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum ImportState {
    Selecting,
    #[cfg(target_os = "linux")]
    Confirming,
    #[cfg(target_os = "linux")]
    Reading,
    #[cfg(target_os = "linux")]
    Extracting,
    #[cfg(target_os = "linux")]
    Indexing,
    #[cfg(target_os = "linux")]
    Committing,
    Cancelling,
    Completed {
        outcome: ImportOutcome,
    },
    Cancelled,
    Failed {
        error: AgentError,
    },
}

impl ImportState {
    fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled | Self::Failed { .. }
        )
    }

    fn cancellable(&self) -> bool {
        #[cfg(target_os = "linux")]
        return matches!(self, Self::Reading | Self::Extracting | Self::Indexing);
        #[cfg(not(target_os = "linux"))]
        false
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportStatus {
    job_id: u32,
    state: ImportState,
}

impl KnowledgeJobs {
    fn start(&self) -> AgentResult<Arc<ImportJob>> {
        let mut current = self.0.lock().map_err(|_| storage_error())?;
        let id = match current.as_ref() {
            Some(job) => {
                if !job.state()?.terminal() {
                    return Err(AgentError::new(
                        AgentErrorCode::Busy,
                        "A knowledge import is already in progress.",
                    ));
                }
                job.id.checked_add(1).ok_or_else(storage_error)?
            }
            None => 1,
        };
        let job = Arc::new(ImportJob {
            id,
            state: Mutex::new(ImportState::Selecting),
        });
        *current = Some(Arc::clone(&job));
        Ok(job)
    }

    fn current(&self, id: Option<u32>) -> AgentResult<Option<Arc<ImportJob>>> {
        let current = self.0.lock().map_err(|_| storage_error())?;
        Ok(current
            .as_ref()
            .filter(|job| id.is_none_or(|id| id == job.id))
            .cloned())
    }
}

impl ImportJob {
    fn state(&self) -> AgentResult<MutexGuard<'_, ImportState>> {
        self.state.lock().map_err(|_| storage_error())
    }

    #[cfg(target_os = "linux")]
    fn set_phase(&self, phase: ImportState) -> AgentResult<()> {
        let mut state = self.state()?;
        if matches!(*state, ImportState::Cancelling) {
            return Err(cancelled());
        }
        *state = phase;
        Ok(())
    }

    fn cancel(&self) -> AgentResult<bool> {
        let mut state = self.state()?;
        if !state.cancellable() {
            return Ok(false);
        }
        *state = ImportState::Cancelling;
        Ok(true)
    }

    fn finish(&self, result: AgentResult<ImportOutcome>) -> AgentResult<()> {
        let mut state = self.state()?;
        *state = match result {
            _ if matches!(*state, ImportState::Cancelling) => ImportState::Cancelled,
            Ok(outcome) => ImportState::Completed { outcome },
            Err(error) if error.code == AgentErrorCode::Cancelled => ImportState::Cancelled,
            Err(error) => ImportState::Failed { error },
        };
        Ok(())
    }
}

#[tauri::command]
pub fn knowledge_start_import(
    window: WebviewWindow,
    desktop: State<'_, Desktop>,
    jobs: State<'_, KnowledgeJobs>,
) -> AgentResult<u32> {
    start(window, &desktop, &jobs, None)
}

#[tauri::command]
pub fn knowledge_reimport(
    window: WebviewWindow,
    desktop: State<'_, Desktop>,
    jobs: State<'_, KnowledgeJobs>,
    document_id: i64,
) -> AgentResult<u32> {
    validate_document_id(document_id)?;
    start(window, &desktop, &jobs, Some(document_id))
}

fn start(
    window: WebviewWindow,
    desktop: &Desktop,
    jobs: &KnowledgeJobs,
    replace: Option<i64>,
) -> AgentResult<u32> {
    let job = jobs.start()?;
    let id = job.id;
    let database = Arc::clone(&desktop.database);
    async_runtime::spawn(async move {
        let result = choose_and_import(window, database, &job, replace).await;
        let _ = job.finish(result);
    });
    Ok(id)
}

#[tauri::command]
pub fn knowledge_import_status(
    jobs: State<'_, KnowledgeJobs>,
    job_id: Option<u32>,
) -> AgentResult<Option<ImportStatus>> {
    jobs.current(job_id)?
        .map(|job| {
            Ok(ImportStatus {
                job_id: job.id,
                state: job.state()?.clone(),
            })
        })
        .transpose()
}

#[tauri::command]
pub fn knowledge_cancel_import(jobs: State<'_, KnowledgeJobs>, job_id: u32) -> AgentResult<bool> {
    let Some(job) = jobs.current(Some(job_id))? else {
        return Ok(false);
    };
    job.cancel()
}

#[tauri::command]
pub async fn knowledge_library(
    desktop: State<'_, Desktop>,
) -> AgentResult<Vec<storage::knowledge::LibraryDocument>> {
    with_store(Arc::clone(&desktop.database), |db| {
        db.knowledge_library(agent_core::semantic::BGE_KEY)
    })
    .await
}

#[tauri::command]
pub async fn knowledge_list_documents(
    desktop: State<'_, Desktop>,
    before_id: Option<i64>,
    limit: u32,
) -> AgentResult<Vec<KnowledgeDocument>> {
    with_store(Arc::clone(&desktop.database), move |db| {
        db.knowledge_documents(before_id, limit)
    })
    .await
}

#[tauri::command]
pub async fn knowledge_get_document(
    desktop: State<'_, Desktop>,
    document_id: i64,
) -> AgentResult<KnowledgeDocument> {
    with_store(Arc::clone(&desktop.database), move |db| {
        db.knowledge_document(document_id)
    })
    .await
}

#[tauri::command]
pub async fn knowledge_delete_document(
    desktop: State<'_, Desktop>,
    document_id: i64,
) -> AgentResult<()> {
    with_store(Arc::clone(&desktop.database), move |db| {
        db.delete_knowledge_document(document_id)
    })
    .await
}

#[tauri::command]
pub async fn knowledge_search(
    desktop: State<'_, Desktop>,
    request: KnowledgeSearch,
) -> AgentResult<Vec<KnowledgeHit>> {
    with_store(Arc::clone(&desktop.database), move |db| {
        db.search_knowledge(&request)
    })
    .await
}

pub(crate) async fn with_store<T: Send + 'static>(
    database: Arc<Mutex<SqliteStore>>,
    operation: impl FnOnce(&mut SqliteStore) -> AgentResult<T> + Send + 'static,
) -> AgentResult<T> {
    async_runtime::spawn_blocking(move || {
        let mut db = database.lock().map_err(|_| storage_error())?;
        operation(&mut db)
    })
    .await
    .map_err(|_| storage_error())?
}

#[cfg(target_os = "linux")]
async fn choose_and_import(
    window: WebviewWindow,
    database: Arc<Mutex<SqliteStore>>,
    job: &Arc<ImportJob>,
    replace: Option<i64>,
) -> AgentResult<ImportOutcome> {
    use agent_tools::knowledge::SelectedKnowledgeFile;
    use rfd::{AsyncFileDialog, AsyncMessageDialog, MessageButtons, MessageDialogResult};

    if let Some(id) = replace {
        with_store(Arc::clone(&database), move |db| db.knowledge_document(id)).await?;
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

    job.set_phase(ImportState::Confirming)?;
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

    job.set_phase(ImportState::Reading)?;
    let job = Arc::clone(job);
    async_runtime::spawn_blocking(move || import_selected(&database, &job, selected, replace))
        .await
        .map_err(|_| worker_error())?
}

#[cfg(not(target_os = "linux"))]
async fn choose_and_import(
    _: WebviewWindow,
    _: Arc<Mutex<SqliteStore>>,
    _: &Arc<ImportJob>,
    _: Option<i64>,
) -> AgentResult<ImportOutcome> {
    Err(AgentError::new(
        AgentErrorCode::UnsupportedPlatform,
        "Local knowledge import is supported only on Linux.",
    ))
}

#[cfg(target_os = "linux")]
fn import_selected(
    database: &Mutex<SqliteStore>,
    job: &ImportJob,
    selected: agent_tools::knowledge::SelectedKnowledgeFile,
    replace: Option<i64>,
) -> AgentResult<ImportOutcome> {
    use std::time::{Duration, Instant};

    use sha2::{Digest, Sha256};
    use storage::knowledge::{DocumentImport, WriteStage};

    let started = Instant::now();
    let check_time = || {
        if started.elapsed() >= Duration::from_secs(30) {
            return Err(AgentError::new(
                AgentErrorCode::TimedOut,
                "The knowledge import deadline expired.",
            ));
        }
        Ok(())
    };
    let check = || {
        if matches!(*job.state()?, ImportState::Cancelling) {
            return Err(cancelled());
        }
        check_time()
    };

    let contents = selected.read(check)?;
    job.set_phase(ImportState::Extracting)?;
    let fingerprint: [u8; 32] = Sha256::digest(contents.bytes()).into();
    let extracted = contents.extract(check)?;
    job.set_phase(ImportState::Indexing)?;
    let mut database = database.lock().map_err(|_| storage_error())?;
    database.save_knowledge(
        &DocumentImport {
            name: &contents.display_name,
            format: contents.format,
            file_bytes: contents.bytes().len(),
            fingerprint: &fingerprint,
            pdf_coverage: extracted.pdf_coverage,
            chunks: &extracted.chunks,
        },
        replace,
        |stage| match stage {
            WriteStage::Indexing => check(),
            WriteStage::Committing => {
                check_time()?;
                job.set_phase(ImportState::Committing)
            }
        },
    )
}

#[cfg(target_os = "linux")]
fn cancelled() -> AgentError {
    AgentError::new(
        AgentErrorCode::Cancelled,
        "The knowledge import was cancelled.",
    )
}

#[cfg(target_os = "linux")]
fn worker_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        "The knowledge import worker stopped unexpectedly.",
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_commit_are_mutually_exclusive_and_jobs_are_bounded() {
        let jobs = KnowledgeJobs::default();
        let job = jobs.start().unwrap();
        assert_eq!(jobs.start().err().unwrap().code, AgentErrorCode::Busy);
        assert!(!job.cancel().unwrap()); // Native selection has its own Cancel action.
        job.set_phase(ImportState::Reading).unwrap();
        assert!(job.cancel().unwrap());
        let result = job.set_phase(ImportState::Committing);
        assert_eq!(result.unwrap_err().code, AgentErrorCode::Cancelled);
        job.finish(Err(cancelled())).unwrap();
        assert!(matches!(*job.state().unwrap(), ImportState::Cancelled));

        let next = jobs.start().unwrap();
        assert!(jobs.current(Some(job.id)).unwrap().is_none());
        next.set_phase(ImportState::Committing).unwrap();
        assert!(!next.cancel().unwrap());
    }
}
