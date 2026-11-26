//! Local knowledge library and explicit document import jobs.
//!
//! The host owns native selection, consent dialogs and sandboxed parsing
//! through [`DocumentSource`]. The engine owns the job lifecycle,
//! cancellation, deadline, fingerprint and the transactional commit.

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    capability::{IMPORT, Retention},
    classification::{LIBRARY, Labels, PDF_EXCERPT, TEXT_EXCERPT},
    inference::Cancellation,
    knowledge::{DocumentChunk, DocumentFormat, PdfCoverage},
    semantic::BGE_KEY,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use storage::{
    SqliteStore,
    knowledge::{
        DocumentImport, ImportOutcome, KnowledgeDocument, KnowledgeHit, KnowledgeSearch,
        LibraryDocument, WriteStage, storage_error,
    },
};

use crate::{
    Engine,
    policy::{Operation, Permission, Policy, Session},
};

const IMPORT_DEADLINE: Duration = Duration::from_secs(30);

/// Checks cancellation and the import deadline between bounded steps.
pub type ImportCheck<'a> = &'a dyn Fn() -> AgentResult<()>;

/// A document the host pinned from a native selection.
pub trait DocumentSource: Send + 'static {
    type Contents: DocumentContents;
    /// Read the pinned file, calling `check` between bounded reads.
    fn read(self, check: ImportCheck<'_>) -> AgentResult<Self::Contents>;
}

/// The bytes of a pinned document and its extraction.
pub trait DocumentContents {
    fn display_name(&self) -> &str;
    fn format(&self) -> DocumentFormat;
    fn bytes(&self) -> &[u8];
    /// Extract indexable chunks, calling `check` between bounded steps.
    fn extract(&self, check: ImportCheck<'_>) -> AgentResult<ExtractedDocument>;
}

pub struct ExtractedDocument {
    pub chunks: Vec<DocumentChunk>,
    pub pdf_coverage: Option<PdfCoverage>,
}

#[derive(Default)]
pub(crate) struct KnowledgeImports(Mutex<Option<Arc<ImportJob>>>);

pub struct ImportJob {
    id: u32,
    state: Mutex<ImportState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum ImportState {
    Selecting,
    Confirming,
    Reading,
    Extracting,
    Indexing,
    Committing,
    Cancelling,
    Completed { outcome: ImportOutcome },
    Cancelled,
    Failed { error: AgentError },
}

impl ImportState {
    fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled | Self::Failed { .. }
        )
    }

    fn cancellable(&self) -> bool {
        matches!(self, Self::Reading | Self::Extracting | Self::Indexing)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportStatus {
    job_id: u32,
    state: ImportState,
}

impl KnowledgeImports {
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
    pub fn id(&self) -> u32 {
        self.id
    }

    fn state(&self) -> AgentResult<MutexGuard<'_, ImportState>> {
        self.state.lock().map_err(|_| storage_error())
    }

    fn set_phase(&self, phase: ImportState) -> AgentResult<()> {
        let mut state = self.state()?;
        if matches!(*state, ImportState::Cancelling) {
            return Err(cancelled());
        }
        *state = phase;
        Ok(())
    }

    /// The host is asking the user to confirm the selected file.
    pub fn confirming(&self) -> AgentResult<()> {
        self.set_phase(ImportState::Confirming)
    }

    fn cancel(&self) -> AgentResult<bool> {
        let mut state = self.state()?;
        if !state.cancellable() {
            return Ok(false);
        }
        *state = ImportState::Cancelling;
        Ok(true)
    }

    /// Record the terminal state. A pending cancellation always wins.
    pub fn finish(&self, result: AgentResult<ImportOutcome>) -> AgentResult<()> {
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

/// Error for an import the user cancelled.
pub fn cancelled() -> AgentError {
    AgentError::new(
        AgentErrorCode::Cancelled,
        "The knowledge import was cancelled.",
    )
}

/// Error for an import worker that stopped before returning.
pub fn worker_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        "The knowledge import worker stopped unexpectedly.",
    )
}

fn import_selected(
    database: &Mutex<SqliteStore>,
    policy: &Policy,
    job: &ImportJob,
    selected: impl DocumentSource,
    replace: Option<i64>,
) -> AgentResult<ImportOutcome> {
    let started = Instant::now();
    let check_time = || {
        if started.elapsed() >= IMPORT_DEADLINE {
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

    let session = Session::new(Cancellation::default());
    let until = started + IMPORT_DEADLINE;
    let denied = |_| {
        AgentError::new(
            AgentErrorCode::ToolFailed,
            "The intelligence policy denied this import.",
        )
    };
    // The trusted host supplies the pinned native selection after confirmation.
    let observation = Operation::local(
        &session,
        IMPORT,
        "selected-document",
        Labels::from_source(LIBRARY),
        Retention::Session,
    );
    let observe = policy
        .grant(Permission::Observe, &observation, until)
        .map_err(denied)?;
    policy
        .authorize(Permission::Observe, &observation, observe)
        .map_err(denied)?;
    let contents = selected.read(&check)?;
    check()?;
    let class = match contents.format() {
        DocumentFormat::Pdf => PDF_EXCERPT,
        DocumentFormat::Txt | DocumentFormat::Markdown => TEXT_EXCERPT,
    };
    let labels = Labels::derived([Labels::from_source(LIBRARY)], class);
    let import = Operation::local(
        &session,
        IMPORT,
        "selected-document",
        labels,
        Retention::SavedLocally,
    );
    let process = policy
        .grant(Permission::Process, &import, until)
        .map_err(denied)?;
    let retain = policy
        .grant(Permission::Retain, &import, until)
        .map_err(denied)?;
    policy
        .authorize(Permission::Process, &import, process)
        .map_err(denied)?;
    job.set_phase(ImportState::Extracting)?;
    let fingerprint: [u8; 32] = Sha256::digest(contents.bytes()).into();
    let extracted = contents.extract(&check)?;
    job.set_phase(ImportState::Indexing)?;
    let mut database = database.lock().map_err(|_| storage_error())?;
    database.save_knowledge(
        &DocumentImport {
            name: contents.display_name(),
            format: contents.format(),
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
                policy
                    .authorize(Permission::Retain, &import, retain)
                    .map_err(denied)?;
                job.set_phase(ImportState::Committing)
            }
        },
    )
}

impl Engine {
    /// Start a new import job. Fails while another import is unfinished.
    pub fn start_import(&self) -> AgentResult<Arc<ImportJob>> {
        self.imports.start()
    }

    pub fn import_status(&self, job_id: Option<u32>) -> AgentResult<Option<ImportStatus>> {
        self.imports
            .current(job_id)?
            .map(|job| {
                Ok(ImportStatus {
                    job_id: job.id,
                    state: job.state()?.clone(),
                })
            })
            .transpose()
    }

    pub fn cancel_import(&self, job_id: u32) -> AgentResult<bool> {
        let Some(job) = self.imports.current(Some(job_id))? else {
            return Ok(false);
        };
        job.cancel()
    }

    /// Read, extract and commit a confirmed document, replacing `replace`
    /// when given.
    pub async fn import_document(
        &self,
        job: &Arc<ImportJob>,
        selected: impl DocumentSource,
        replace: Option<i64>,
    ) -> AgentResult<ImportOutcome> {
        job.set_phase(ImportState::Reading)?;
        let database = Arc::clone(&self.database);
        let job = Arc::clone(job);
        let policy = Arc::clone(&self.policy);
        self.blocking(move || import_selected(&database, &policy, &job, selected, replace))
            .await
            .ok_or_else(worker_error)?
    }

    pub async fn knowledge_library(&self) -> AgentResult<Vec<LibraryDocument>> {
        self.with_store(|db| db.knowledge_library(BGE_KEY)).await
    }

    pub async fn knowledge_documents(
        &self,
        before_id: Option<i64>,
        limit: u32,
    ) -> AgentResult<Vec<KnowledgeDocument>> {
        self.with_store(move |db| db.knowledge_documents(before_id, limit))
            .await
    }

    pub async fn knowledge_document(&self, document_id: i64) -> AgentResult<KnowledgeDocument> {
        self.with_store(move |db| db.knowledge_document(document_id))
            .await
    }

    pub async fn delete_knowledge_document(&self, document_id: i64) -> AgentResult<()> {
        self.with_store(move |db| db.delete_knowledge_document(document_id))
            .await
    }

    pub async fn search_knowledge(
        &self,
        request: KnowledgeSearch,
    ) -> AgentResult<Vec<KnowledgeHit>> {
        crate::semantic::authorize_retrieval(&self.policy)?;
        self.with_store(move |db| db.search_knowledge(&request))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_commit_are_mutually_exclusive_and_jobs_are_bounded() {
        let jobs = KnowledgeImports::default();
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
