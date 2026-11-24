use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

use agent_core::{
    chat::{
        ChatStore, Conversation, ConversationId, ConversationSummary, GenerationState, Message,
        MessageId, ModelId, ModelReference, ModelSettings, Preferences, TurnAction,
        validate_system_prompt, validate_title,
    },
    inference::{ErrorCode, GenerationRequest, InferenceError, ModelInfo, Result},
};
use inference::{InferenceWorker, Pending, Phase, Status};
use serde::Serialize;
use storage::SqliteStore;
use tauri::{State, ipc::Channel};
use crate::agent::AgentSession;

pub struct Desktop {
    pub worker: Arc<InferenceWorker>,
    pub agent: Arc<AgentSession>,
    pub database: Arc<Mutex<SqliteStore>>,
    pub database_path: PathBuf,
    pub operations: Arc<AtomicBool>,
    pub active_model: AtomicI64,
}

fn database(db: &Mutex<SqliteStore>) -> Result<MutexGuard<'_, SqliteStore>> {
    db.lock().map_err(|_| {
        InferenceError::new(
            ErrorCode::Storage,
            "The conversation database is unavailable.",
        )
    })
}

pub(crate) struct OperationGuard(Arc<AtomicBool>);

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Desktop {
    pub(crate) fn reserve(&self) -> Result<OperationGuard> {
        self.operations.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| InferenceError::new(ErrorCode::Busy, "Wait for the current operation to finish."))?;
        Ok(OperationGuard(Arc::clone(&self.operations)))
    }

    fn active_model(&self) -> Option<ModelId> {
        let id = self.active_model.load(Ordering::Acquire);
        (id > 0).then_some(id)
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.agent.shutdown(&self.worker);
    }
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

#[tauri::command]
pub fn bootstrap(state: State<'_, Desktop>) -> Result<Bootstrap> {
    let db = database(&state.database)?;
    Ok(Bootstrap {
        preferences: db.preferences()?,
        database_path: state.database_path.to_string_lossy().into_owned(),
        models: db.recent_models()?,
        active_model_id: state.active_model(),
        inference: state.worker.status()?,
    })
}

#[tauri::command]
pub fn list_conversations(
    state: State<'_, Desktop>,
    query: &str,
) -> Result<Vec<ConversationSummary>> {
    if query.len() > 1024 || query.contains('\0') {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "The search is too long or invalid.",
        ));
    }
    database(&state.database)?.list(query.trim())
}

#[tauri::command]
pub fn get_conversation(state: State<'_, Desktop>, id: ConversationId) -> Result<Conversation> {
    database(&state.database)?.get(id)
}

#[tauri::command]
pub fn create_conversation(state: State<'_, Desktop>) -> Result<Conversation> {
    let _operation = state.reserve()?;
    database(&state.database)?.create()
}

#[tauri::command]
pub fn rename_conversation(
    state: State<'_, Desktop>,
    id: ConversationId,
    title: &str,
) -> Result<()> {
    let _operation = state.reserve()?;
    database(&state.database)?.rename(id, validate_title(title)?)
}

#[tauri::command]
pub fn set_system_prompt(
    state: State<'_, Desktop>,
    id: ConversationId,
    system_prompt: &str,
) -> Result<()> {
    let _operation = state.reserve()?;
    validate_system_prompt(system_prompt)?;
    database(&state.database)?.set_system_prompt(id, system_prompt)
}

#[tauri::command]
pub fn delete_conversation(state: State<'_, Desktop>, id: ConversationId) -> Result<()> {
    let _operation = state.reserve()?;
    database(&state.database)?.delete(id)
}

#[tauri::command]
pub fn save_preferences(state: State<'_, Desktop>, preferences: Preferences) -> Result<()> {
    let _operation = state.reserve()?;
    preferences.defaults.validate()?;
    database(&state.database)?.save_preferences(&preferences)
}

#[tauri::command]
pub async fn choose_model(state: State<'_, Desktop>) -> Result<Option<ModelReference>> {
    let _operation = state.reserve()?;
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Choose a GGUF model")
        .add_filter("GGUF models", &["gguf"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let path = file.path();
    let name = path
        .file_name()
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::PickerFailed,
                "The picker did not return a file name.",
            )
        })?
        .to_string_lossy();
    let mut db = database(&state.database)?;
    let defaults = db.preferences()?.defaults;
    db.remember_model(path, &name, defaults).map(Some)
}

#[tauri::command]
pub fn inference_status(state: State<'_, Desktop>) -> Result<Status> {
    state.worker.status()
}

async fn wait_for<T>(pending: Pending<T>) -> Result<T> {
    pending.await.map_err(|_| {
        InferenceError::new(ErrorCode::WorkerStopped, "The inference worker stopped.")
    })?
}

#[tauri::command]
pub async fn load_model(
    state: State<'_, Desktop>,
    model_id: ModelId,
    settings: ModelSettings,
) -> Result<ModelInfo> {
    let _operation = state.reserve()?;
    settings.validate()?;
    let path = database(&state.database)?.model(model_id)?.path;
    let info = wait_for(state.worker.load(path, settings.model)?).await?;
    let saved = {
        let mut db = database(&state.database)?;
        db.save_model_settings(model_id, settings)
            .and_then(|()| db.mark_model_used(model_id))
    };
    if let Err(error) = saved {
        let _ = wait_for(state.worker.unload()?).await;
        return Err(error);
    }
    state.active_model.store(model_id, Ordering::Release);
    Ok(info)
}

#[tauri::command]
pub fn save_model_settings(
    state: State<'_, Desktop>,
    model_id: ModelId,
    settings: ModelSettings,
) -> Result<()> {
    let _operation = state.reserve()?;
    settings.validate()?;
    if state.active_model() == Some(model_id)
        && let Some(model) = state.worker.status()?.model
        && model.config != settings.model
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Unload the model before changing context length or CPU threads.",
        ));
    }
    database(&state.database)?.save_model_settings(model_id, settings)
}

#[tauri::command]
pub async fn unload_model(state: State<'_, Desktop>) -> Result<()> {
    let _operation = state.reserve()?;
    wait_for(state.worker.unload()?).await?;
    state.active_model.store(0, Ordering::Release);
    Ok(())
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    Started {
        user: Option<Message>,
        assistant: Box<Message>,
    },
    Delta {
        id: MessageId,
        text: String,
    },
}

struct Transcript {
    text: String,
    checkpoint_at: Instant,
    checkpoint_bytes: usize,
}

impl Transcript {
    fn push(&mut self, text: &str, db: &Mutex<SqliteStore>, id: MessageId) -> Result<()> {
        self.text.push_str(text);
        if self.checkpoint_at.elapsed() >= Duration::from_millis(500)
            || self.text.len() - self.checkpoint_bytes >= 4096
        {
            database(db)?.checkpoint(id, &self.text)?;
            self.checkpoint_bytes = self.text.len();
            self.checkpoint_at = Instant::now();
        }
        Ok(())
    }
}

fn transcript_lock(transcript: &Mutex<Transcript>) -> Result<MutexGuard<'_, Transcript>> {
    transcript.lock().map_err(|_| {
        InferenceError::new(
            ErrorCode::WorkerStopped,
            "The generation transcript is unavailable.",
        )
    })
}

async fn run_turn(
    state: &Desktop,
    id: ConversationId,
    action: TurnAction,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    let _operation = state.reserve()?;
    let model_id = state
        .active_model()
        .ok_or_else(|| InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first."))?;
    let status = state.worker.status()?;
    if status.phase != Phase::Ready {
        return Err(InferenceError::new(
            ErrorCode::Busy,
            "Wait for the model to be ready.",
        ));
    }
    if let TurnAction::Send(content) = &action {
        GenerationRequest::new(content).validate()?;
    }
    let (turn, mut settings) = {
        let mut db = database(&state.database)?;
        let settings = db.model(model_id)?.reference.settings;
        (db.begin(id, action)?, settings)
    };
    settings.enable_thinking &= status.model.is_some_and(|model| model.supports_thinking);
    let mut assistant = turn.assistant;
    let message_id = assistant.id;
    let transcript = Arc::new(Mutex::new(Transcript {
        text: String::new(),
        checkpoint_at: Instant::now(),
        checkpoint_bytes: 0,
    }));
    let buffer = Arc::clone(&transcript);
    let db = Arc::clone(&state.database);
    let pending = {
        // Reserve the worker's cancellation slot before notifying the UI, and
        // hold back deltas until the initial message has been sent.
        let _first_event = transcript_lock(&transcript)?;
        let deltas = events.clone();
        let pending =
            state
                .worker
                .generate(settings.request(turn.prompt, turn.history), move |delta| {
                    transcript_lock(&buffer)?.push(&delta.text, &db, message_id)?;
                    let _ = deltas.send(ChatEvent::Delta {
                        id: message_id,
                        text: delta.text,
                    });
                    Ok(())
                });
        if pending.is_ok() {
            let _ = events.send(ChatEvent::Started {
                user: turn.user,
                assistant: Box::new(assistant.clone()),
            });
        }
        pending
    };
    let generated = match pending {
        Ok(pending) => wait_for(pending).await,
        Err(error) => Err(error),
    };

    assistant.content = std::mem::take(&mut transcript_lock(&transcript)?.text);
    assistant.state = generated.as_ref().map_or(GenerationState::Failed, |stats| {
        GenerationState::from_finish(stats.reason)
    });
    database(&state.database)?.finish(
        message_id,
        &assistant.content,
        assistant.state,
        generated.as_ref().ok(),
    )?;
    assistant.stats = Some(generated?);
    Ok(assistant)
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Desktop>,
    id: ConversationId,
    content: String,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    run_turn(&state, id, TurnAction::Send(content), events).await
}

#[tauri::command]
pub async fn regenerate(
    state: State<'_, Desktop>,
    id: ConversationId,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    run_turn(&state, id, TurnAction::Regenerate, events).await
}

#[tauri::command]
pub fn stop_generation(state: State<'_, Desktop>) -> Result<bool> {
    if state.agent.is_active() {
        return Err(InferenceError::new(
            ErrorCode::Busy,
            "Cancel the active agent task from Agent mode.",
        ));
    }
    state.worker.cancel()
}
