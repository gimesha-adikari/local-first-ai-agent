//! Main-window commands for local embeddings and multi-mode retrieval.

use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    inference::Cancellation,
    knowledge::knowledge_limit,
    semantic::{BGE_KEY, BGE_SHA256, CONTEXT_TOKENS, DIMENSIONS, MAX_DOCUMENT_WINDOWS, UnitVector},
};
use inference::embedding::{BgeProvider, EmbeddingWorker, THREADS, unavailable};
use serde::Serialize;
use storage::{
    SqliteStore,
    knowledge::{KnowledgeSearch, storage_error, validate_document_id},
    semantic::{IndexCoverage, RetrievalMode, RetrievalResults},
};
use tauri::{State, WebviewWindow};

use crate::{
    commands::{Desktop, OperationGuard},
    knowledge::with_store,
};

const INDEX_DEADLINE: Duration = Duration::from_secs(300);
const QUERY_DEADLINE: Duration = Duration::from_secs(15);

pub struct SemanticState {
    worker: EmbeddingWorker,
    loaded: Arc<AtomicBool>,
    latest: Mutex<Option<Arc<IndexJob>>>,
    restore: Arc<Mutex<RestoreState>>,
}

/// Outcome of restoring the remembered embedding model at launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreState {
    /// No embedding model has been chosen.
    NotRemembered,
    /// Verification and loading are in progress.
    Pending,
    Restored,
    /// Chosen and verified in this session. It is restored at the next launch.
    Selected,
    /// The user unloaded the model; it is not loaded at launch.
    Unloaded,
    /// The remembered file is not readable now, for example on an unmounted
    /// drive. The choice is kept and retried at the next launch.
    Missing,
    /// The file no longer matches the supported checksum. The choice was
    /// forgotten.
    Incompatible,
    Failed,
}

impl SemanticState {
    pub fn new() -> AgentResult<Self> {
        Ok(Self {
            worker: EmbeddingWorker::spawn()?,
            loaded: Arc::new(AtomicBool::new(false)),
            latest: Mutex::new(None),
            restore: Arc::new(Mutex::new(RestoreState::NotRemembered)),
        })
    }

    fn set_restore(&self, state: RestoreState) {
        if let Ok(mut restore) = self.restore.lock() {
            *restore = state;
        }
    }

    /// Restore the remembered embedding model on the embedding worker. The
    /// file is reverified by checksum and metadata before it is used. Chat
    /// and dense retrieval report the model unavailable until this finishes.
    pub fn restore_remembered(&self, database: Arc<Mutex<SqliteStore>>) {
        let choice = database
            .lock()
            .ok()
            .and_then(|db| db.embedding_model_choice().ok().flatten());
        let path = match choice {
            None => return self.set_restore(RestoreState::NotRemembered),
            Some((_, false)) => return self.set_restore(RestoreState::Unloaded),
            Some((path, true)) if !path.is_file() => {
                return self.set_restore(RestoreState::Missing);
            }
            Some((path, true)) => path,
        };
        self.set_restore(RestoreState::Pending);
        let loaded = Arc::clone(&self.loaded);
        let restore = Arc::clone(&self.restore);
        let submitted = self.worker.submit(move |provider| {
            let result = provider.load(&path);
            loaded.store(provider.available(), Ordering::Release);
            let state = match &result {
                Ok(()) => RestoreState::Restored,
                Err(error) if error.code == AgentErrorCode::EmbeddingIncompatible => {
                    if let Ok(mut db) = database.lock() {
                        let _ = db.forget_embedding_model();
                    }
                    RestoreState::Incompatible
                }
                Err(_) if !path.is_file() => RestoreState::Missing,
                Err(_) => RestoreState::Failed,
            };
            if let Ok(mut current) = restore.lock() {
                *current = state;
            }
            Ok(())
        });
        if submitted.is_err() {
            self.set_restore(RestoreState::Failed);
        }
    }

    // The Chat host holds the desktop operation lease across this worker call
    // and the subsequent native generation.
    pub(crate) async fn embed_chat_query(
        &self,
        query: String,
        cancellation: Cancellation,
    ) -> AgentResult<UnitVector> {
        if !self.loaded.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        self.worker
            .submit(move |provider| {
                embed_query(provider, &query, &cancellation.with_timeout(QUERY_DEADLINE))
            })?
            .await
            .map_err(|_| unavailable())?
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn load_fixture_embedding(&self, path: std::path::PathBuf) -> AgentResult<()> {
        let loaded = Arc::clone(&self.loaded);
        let pending = self.worker.submit(move |provider| {
            provider.load(&path)?;
            loaded.store(provider.available(), Ordering::Release);
            Ok(())
        })?;
        tauri::async_runtime::block_on(pending).map_err(|_| unavailable())?
    }

    fn latest(&self) -> AgentResult<MutexGuard<'_, Option<Arc<IndexJob>>>> {
        self.latest.lock().map_err(|_| storage_error())
    }
}

impl Drop for SemanticState {
    fn drop(&mut self) {
        if let Ok(latest) = self.latest.lock()
            && let Some(job) = latest.as_ref()
        {
            job.cancellation.cancel();
        }
    }
}

struct IndexJob {
    cancellation: Cancellation,
    progress: Mutex<IndexProgress>,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IndexPhase {
    Indexing,
    Cancelling,
    Committing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    job_id: u32,
    document_id: i64,
    phase: IndexPhase,
    processed_chunks: usize,
    total_chunks: usize,
    window_count: Option<usize>,
    error: Option<AgentError>,
}

impl IndexJob {
    fn progress(&self) -> AgentResult<MutexGuard<'_, IndexProgress>> {
        self.progress.lock().map_err(|_| storage_error())
    }

    fn cancel(&self) -> AgentResult<bool> {
        let mut progress = self.progress()?;
        if progress.phase != IndexPhase::Indexing {
            return Ok(false);
        }
        self.cancellation.cancel();
        progress.phase = IndexPhase::Cancelling;
        Ok(true)
    }

    fn begin_commit(&self, time: &Cancellation) -> AgentResult<()> {
        let mut progress = self.progress()?;
        check(time)?;
        progress.phase = IndexPhase::Committing;
        Ok(())
    }

    fn finish(&self, result: AgentResult<usize>, lease: Option<OperationGuard>) {
        if let Ok(mut progress) = self.progress() {
            match result {
                _ if progress.phase == IndexPhase::Cancelling => {
                    progress.phase = IndexPhase::Cancelled;
                }
                Ok(windows) => {
                    progress.phase = IndexPhase::Completed;
                    progress.window_count = Some(windows);
                }
                Err(error) if error.code == AgentErrorCode::Cancelled => {
                    progress.phase = IndexPhase::Cancelled;
                }
                Err(error) => {
                    progress.error = Some(error);
                    progress.phase = IndexPhase::Failed;
                }
            }
            // Readers observe terminal progress only after the CPU lease is
            // released. A competing command waits for this progress lock.
            drop(lease);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingModelStatus {
    supported: bool,
    available: bool,
    name: &'static str,
    sha256: &'static str,
    preprocessing: u32,
    dimensions: usize,
    context_tokens: usize,
    cpu_threads: i32,
    restore: RestoreState,
}

fn model_status(state: &SemanticState) -> EmbeddingModelStatus {
    EmbeddingModelStatus {
        restore: state
            .restore
            .lock()
            .map_or(RestoreState::Failed, |restore| *restore),
        supported: cfg!(target_os = "linux"),
        available: state.loaded.load(Ordering::Acquire),
        name: "bge-small-en-v1.5-f16",
        sha256: BGE_SHA256,
        preprocessing: BGE_KEY.preprocessing,
        dimensions: DIMENSIONS,
        context_tokens: CONTEXT_TOKENS,
        cpu_threads: THREADS,
    }
}

#[tauri::command]
pub fn knowledge_embedding_status(state: State<'_, SemanticState>) -> EmbeddingModelStatus {
    model_status(&state)
}

#[tauri::command]
pub async fn knowledge_choose_embedding_model(
    window: WebviewWindow,
    desktop: State<'_, Desktop>,
    state: State<'_, SemanticState>,
) -> AgentResult<Option<EmbeddingModelStatus>> {
    if !cfg!(target_os = "linux") {
        return Err(AgentError::new(
            AgentErrorCode::UnsupportedPlatform,
            "Embedding model loading is supported on Linux.",
        ));
    }
    let lease = reserve(&desktop)?;
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Choose the local BGE embedding model")
        .add_filter("GGUF", &["gguf"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let path = file.path().to_path_buf();
    let remembered = path.clone();
    let loaded = Arc::clone(&state.loaded);
    state
        .worker
        .submit(move |provider| {
            let _lease = lease;
            let result = provider.load(&path);
            loaded.store(provider.available(), Ordering::Release);
            result
        })?
        .await
        .map_err(|_| unavailable())??;
    // Remember only a file that passed verification.
    desktop
        .database
        .lock()
        .map_err(|_| storage_error())?
        .remember_embedding_model(&remembered)
        .map_err(|_| storage_error())?;
    state.set_restore(RestoreState::Selected);
    Ok(Some(model_status(&state)))
}

#[tauri::command]
pub async fn knowledge_unload_embedding_model(
    desktop: State<'_, Desktop>,
    state: State<'_, SemanticState>,
) -> AgentResult<()> {
    let lease = reserve(&desktop)?;
    let loaded = Arc::clone(&state.loaded);
    state
        .worker
        .submit(move |provider| {
            let _lease = lease;
            provider.unload();
            loaded.store(false, Ordering::Release);
            Ok(())
        })?
        .await
        .map_err(|_| unavailable())??;
    // Keep the remembered file, but respect the unload at the next launch.
    desktop
        .database
        .lock()
        .map_err(|_| storage_error())?
        .set_embedding_restore(false)
        .map_err(|_| storage_error())?;
    state.set_restore(RestoreState::Unloaded);
    Ok(())
}

#[tauri::command]
pub async fn knowledge_embedding_coverage(
    desktop: State<'_, Desktop>,
    document_ids: Option<Vec<i64>>,
) -> AgentResult<IndexCoverage> {
    with_store(Arc::clone(&desktop.database), move |db| {
        db.embedding_coverage(BGE_KEY, document_ids.as_deref())
    })
    .await
}

/// Creates or replaces the vector generation for exactly one current document.
#[tauri::command]
pub fn knowledge_reindex(
    desktop: State<'_, Desktop>,
    state: State<'_, SemanticState>,
    document_id: i64,
) -> AgentResult<u32> {
    validate_document_id(document_id)?;
    let lease = reserve(&desktop)?;
    if !state.loaded.load(Ordering::Acquire) {
        return Err(unavailable());
    }
    let mut latest = state.latest()?;
    let job_id = match latest.as_ref() {
        Some(job) => job
            .progress()?
            .job_id
            .checked_add(1)
            .ok_or_else(storage_error)?,
        None => 1,
    };
    let job = Arc::new(IndexJob {
        cancellation: Cancellation::default(),
        progress: Mutex::new(IndexProgress {
            job_id,
            document_id,
            phase: IndexPhase::Indexing,
            processed_chunks: 0,
            total_chunks: 0,
            window_count: None,
            error: None,
        }),
    });
    let database = Arc::clone(&desktop.database);
    let background_job = Arc::clone(&job);
    let worker_job = Arc::clone(&job);
    let pending = state.worker.submit(move |provider| {
        let result = run_index(provider, &database, document_id, &worker_job);
        worker_job.finish(result, Some(lease));
        Ok(())
    })?;
    *latest = Some(job);
    tauri::async_runtime::spawn(async move {
        if pending.await.is_err() {
            background_job.finish(Err(unavailable()), None);
        }
    });
    Ok(job_id)
}

#[tauri::command]
pub fn knowledge_index_progress(
    state: State<'_, SemanticState>,
    job_id: Option<u32>,
) -> AgentResult<Option<IndexProgress>> {
    let latest = state.latest()?;
    let Some(job) = latest.as_ref() else {
        return Ok(None);
    };
    let progress = job.progress()?;
    Ok(job_id
        .is_none_or(|id| id == progress.job_id)
        .then(|| progress.clone()))
}

#[tauri::command]
pub fn knowledge_cancel_index(state: State<'_, SemanticState>, job_id: u32) -> AgentResult<bool> {
    let latest = state.latest()?;
    let Some(job) = latest.as_ref() else {
        return Ok(false);
    };
    if job.progress()?.job_id != job_id {
        return Ok(false);
    }
    job.cancel()
}

#[tauri::command]
pub async fn knowledge_retrieve(
    desktop: State<'_, Desktop>,
    state: State<'_, SemanticState>,
    request: KnowledgeSearch,
    mode: RetrievalMode,
) -> AgentResult<RetrievalResults> {
    request.validate()?;
    let database = Arc::clone(&desktop.database);
    if mode == RetrievalMode::Keyword {
        return with_store(database, move |db| {
            db.retrieve_knowledge(&request, mode, BGE_KEY, None)
        })
        .await;
    }
    let lease = reserve(&desktop)?;
    if !state.loaded.load(Ordering::Acquire) {
        return Err(unavailable());
    }
    state
        .worker
        .submit(move |provider| {
            let _lease = lease;
            run_retrieval(provider, &database, &request, mode)
        })?
        .await
        .map_err(|_| unavailable())?
}

fn run_retrieval(
    provider: &BgeProvider,
    database: &Mutex<SqliteStore>,
    request: &KnowledgeSearch,
    mode: RetrievalMode,
) -> AgentResult<RetrievalResults> {
    let deadline = Cancellation::default().with_timeout(QUERY_DEADLINE);
    let vector = embed_query(provider, &request.query, &deadline)?;
    let db = database.lock().map_err(|_| storage_error())?;
    check(&deadline)?;
    let result = db.retrieve_knowledge(request, mode, BGE_KEY, Some(&vector))?;
    check(&deadline)?;
    Ok(result)
}

fn embed_query(
    provider: &BgeProvider,
    query: &str,
    cancellation: &Cancellation,
) -> AgentResult<UnitVector> {
    check(cancellation)?;
    provider.session()?.query(query, || check(cancellation))
}

fn run_index(
    provider: &BgeProvider,
    database: &Mutex<SqliteStore>,
    document_id: i64,
    job: &IndexJob,
) -> AgentResult<usize> {
    let deadline = job.cancellation.with_timeout(INDEX_DEADLINE);
    let checkpoint = || check(&deadline);
    checkpoint()?;
    let snapshot = database
        .lock()
        .map_err(|_| storage_error())?
        .embedding_snapshot(document_id)?;
    job.progress()?.total_chunks = snapshot.chunks.len();
    let mut session = provider.session()?;
    let mut embeddings = Vec::with_capacity(snapshot.chunks.len());
    let mut count = 0;
    for chunk in &snapshot.chunks {
        let windows = session.passage(&chunk.text, checkpoint)?;
        count += windows.len();
        if count > MAX_DOCUMENT_WINDOWS {
            return Err(knowledge_limit());
        }
        embeddings.push(windows);
        job.progress()?.processed_chunks = embeddings.len();
    }
    database
        .lock()
        .map_err(|_| storage_error())?
        .install_embeddings(&snapshot, BGE_KEY, &embeddings, checkpoint, || {
            job.begin_commit(&deadline)
        })
}

fn check(cancellation: &Cancellation) -> AgentResult<()> {
    if cancellation.is_explicitly_cancelled() {
        return Err(AgentError::new(
            AgentErrorCode::Cancelled,
            "The knowledge operation was cancelled.",
        ));
    }
    if cancellation.is_deadline_elapsed() {
        return Err(AgentError::new(
            AgentErrorCode::TimedOut,
            "The local embedding deadline expired.",
        ));
    }
    Ok(())
}

fn reserve(desktop: &Desktop) -> AgentResult<OperationGuard> {
    desktop.reserve().map_err(|_| {
        AgentError::new(
            AgentErrorCode::Busy,
            "Wait for the current chat, Agent, or embedding operation to finish.",
        )
    })
}

#[cfg(test)]
#[path = "semantic_tests.rs"]
mod tests;
