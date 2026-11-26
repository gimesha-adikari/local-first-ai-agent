//! Platform-independent intelligence coordination.
//!
//! The engine owns Chat orchestration, knowledge and semantic retrieval,
//! memory context and model lifecycle on top of `agent-core`, `inference`
//! and `storage`. It has no Tauri, webview or operating-system UI
//! dependencies. A host supplies the few platform services it needs through
//! [`Platform`] and delivers events through [`chat::ChatEventSink`]. Native
//! dialogs, IPC validation and window concerns stay in the host.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};

use agent_core::{
    agent::AgentResult,
    chat::{
        ChatStore, Conversation, ConversationId, ConversationSummary, ModelId, ModelReference,
        Preferences, validate_system_prompt, validate_title,
    },
    inference::{Cancellation, ErrorCode, InferenceError, Result},
};
use inference::{InferenceWorker, LocalInference, Status};
use serde::Serialize;
use storage::SqliteStore;

pub mod chat;
mod chat_session;
pub mod knowledge;
pub mod models;
pub mod network;
pub mod policy;
pub mod semantic;

use knowledge::KnowledgeImports;
use semantic::SemanticState;

/// Blocking work handed to the host, such as SQLite access or file hashing.
pub type BlockingTask = Box<dyn FnOnce() + Send + 'static>;

/// A future the host drives to completion in the background.
pub type BackgroundTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Services the engine needs from its host runtime.
pub trait Platform: Send + Sync + 'static {
    /// Run blocking work away from async callers.
    fn spawn_blocking(&self, task: BlockingTask);
    /// Drive a background future to completion.
    fn spawn(&self, task: BackgroundTask);
    /// Memory currently available for a model, in bytes, if known. It sizes
    /// recommended context lengths.
    fn available_memory(&self) -> Option<u64>;
}

/// Lock the conversation database, reporting a poisoned lock as a storage
/// failure.
pub fn database(db: &Mutex<SqliteStore>) -> Result<MutexGuard<'_, SqliteStore>> {
    db.lock().map_err(|_| {
        InferenceError::new(
            ErrorCode::Storage,
            "The conversation database is unavailable.",
        )
    })
}

/// Run blocking work on `platform`. `None` means the task stopped before it
/// returned, for example by panicking.
pub(crate) async fn run_blocking<T: Send + 'static>(
    platform: &dyn Platform,
    task: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (reply, result) = futures_channel::oneshot::channel();
    platform.spawn_blocking(Box::new(move || {
        let _ = reply.send(task());
    }));
    result.await.ok()
}

/// Exclusive lease on the CPU-bound operation gate shared by Chat, Agent,
/// model and embedding work. Dropping it releases the gate.
pub struct OperationGuard(Arc<AtomicBool>);

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The intelligence engine for one profile database.
pub struct Engine {
    platform: Arc<dyn Platform>,
    worker: Arc<InferenceWorker>,
    adapter: LocalInference,
    policy: Arc<policy::Policy>,
    database: Arc<Mutex<SqliteStore>>,
    database_path: PathBuf,
    operations: Arc<AtomicBool>,
    active_model: AtomicI64,
    chat_cancellation: Arc<Mutex<Option<Cancellation>>>,
    semantic: Arc<SemanticState>,
    imports: KnowledgeImports,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    preferences: Preferences,
    database_path: String,
    models: Vec<ModelReference>,
    active_model_id: Option<ModelId>,
    inference: Status,
}

impl Engine {
    /// Create the engine around an opened store and a running inference
    /// worker. Starts the embedding worker and restores the remembered
    /// embedding model in the background.
    pub fn new(
        platform: Arc<dyn Platform>,
        worker: Arc<InferenceWorker>,
        database: SqliteStore,
        database_path: PathBuf,
    ) -> AgentResult<Self> {
        let policy = Arc::new(policy::Policy::default());
        let semantic = Arc::new(SemanticState::new(Arc::clone(&policy))?);
        let database = Arc::new(Mutex::new(database));
        semantic.restore_remembered(Arc::clone(&database));
        Ok(Self {
            platform,
            adapter: LocalInference::new(Arc::clone(&worker)),
            policy,
            worker,
            database,
            database_path,
            operations: Arc::new(AtomicBool::new(false)),
            active_model: AtomicI64::new(0),
            chat_cancellation: Arc::new(Mutex::new(None)),
            semantic,
            imports: KnowledgeImports::default(),
        })
    }

    /// Reserve the shared operation gate, failing while another operation
    /// holds it.
    pub fn reserve(&self) -> Result<OperationGuard> {
        self.operations
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| {
                InferenceError::new(ErrorCode::Busy, "Wait for the current operation to finish.")
            })?;
        Ok(OperationGuard(Arc::clone(&self.operations)))
    }

    /// Whether `lease` was issued by this engine's operation gate.
    pub(crate) fn owns(&self, lease: &OperationGuard) -> bool {
        Arc::ptr_eq(&lease.0, &self.operations)
    }

    /// The shared store. The engine is its single owner; hosts use it for
    /// plain record access and Agent persistence.
    pub fn store(&self) -> &Arc<Mutex<SqliteStore>> {
        &self.database
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// The native inference worker, shared with the Agent runtime host.
    pub fn worker(&self) -> &Arc<InferenceWorker> {
        &self.worker
    }

    pub fn inference_adapter(&self) -> &LocalInference {
        &self.adapter
    }

    pub fn intelligence_policy(&self) -> &Arc<policy::Policy> {
        &self.policy
    }

    pub fn active_model(&self) -> Option<ModelId> {
        let id = self.active_model.load(Ordering::Acquire);
        (id > 0).then_some(id)
    }

    pub fn bootstrap(&self) -> Result<Bootstrap> {
        let db = database(&self.database)?;
        Ok(Bootstrap {
            preferences: db.preferences()?,
            database_path: self.database_path.to_string_lossy().into_owned(),
            models: db.recent_models()?,
            active_model_id: self.active_model(),
            inference: self.worker.status()?,
        })
    }

    /// Conversations matching an already validated search.
    pub fn list_conversations(&self, query: &str) -> Result<Vec<ConversationSummary>> {
        database(&self.database)?.list(query)
    }

    pub fn get_conversation(&self, id: ConversationId) -> Result<Conversation> {
        database(&self.database)?.get(id)
    }

    pub fn create_conversation(&self) -> Result<Conversation> {
        let _operation = self.reserve()?;
        database(&self.database)?.create()
    }

    pub fn rename_conversation(&self, id: ConversationId, title: &str) -> Result<()> {
        let _operation = self.reserve()?;
        database(&self.database)?.rename(id, validate_title(title)?)
    }

    pub fn set_system_prompt(&self, id: ConversationId, system_prompt: &str) -> Result<()> {
        let _operation = self.reserve()?;
        validate_system_prompt(system_prompt)?;
        database(&self.database)?.set_system_prompt(id, system_prompt)
    }

    pub fn delete_conversation(&self, id: ConversationId) -> Result<()> {
        let _operation = self.reserve()?;
        database(&self.database)?.delete(id)
    }

    pub fn save_preferences(&self, preferences: &Preferences) -> Result<()> {
        let _operation = self.reserve()?;
        preferences.defaults.validate()?;
        database(&self.database)?.save_preferences(preferences)
    }

    /// Run blocking work through the host. `None` means the task stopped
    /// before it returned, for example by panicking.
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        task: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        run_blocking(self.platform.as_ref(), task).await
    }

    /// Run a store operation on a blocking thread.
    pub async fn with_store<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut SqliteStore) -> AgentResult<T> + Send + 'static,
    ) -> AgentResult<T> {
        let database = Arc::clone(&self.database);
        self.blocking(move || {
            let mut db = database
                .lock()
                .map_err(|_| storage::knowledge::storage_error())?;
            operation(&mut db)
        })
        .await
        .ok_or_else(storage::knowledge::storage_error)?
    }
}

#[cfg(test)]
mod tests;
