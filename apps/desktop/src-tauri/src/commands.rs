use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    agent::AgentSession,
    chat_context::{KnowledgeSelection, retrieval_error, retrieval_query},
    semantic::SemanticState,
};
use agent_core::{
    agent::{AgentError, AgentErrorCode},
    chat::{
        ChatStore, Conversation, ConversationId, ConversationSummary, GenerationState, Message,
        MessageId, ModelId, ModelReference, ModelSettings, Preferences, TurnAction,
        validate_system_prompt, validate_title,
    },
    chat_context::{
        AttemptContext, ChatContext, ChatIntent, ContextObserver, EvidenceTask, Grounding,
        no_evidence_answer,
    },
    chat_route::{RoutePlan, route, routed_question},
    inference::{
        Cancellation, ErrorCode, GenerationRequest, GenerationStats, InferenceError, ModelInfo,
        Result,
    },
    semantic::{BGE_KEY, UnitVector},
};
use inference::{InferenceWorker, Pending, Phase, Status};
use serde::{Deserialize, Serialize};
use storage::{
    SqliteStore, chat_context::KnowledgeCoverage, knowledge::KnowledgeHit, semantic::RetrievalMode,
};
use tauri::{State, ipc::Channel};

pub struct Desktop {
    pub worker: Arc<InferenceWorker>,
    pub agent: Arc<AgentSession>,
    pub database: Arc<Mutex<SqliteStore>>,
    pub database_path: PathBuf,
    pub operations: Arc<AtomicBool>,
    pub active_model: AtomicI64,
    pub chat_cancellation: Mutex<Option<Cancellation>>,
}

pub(crate) fn database(db: &Mutex<SqliteStore>) -> Result<MutexGuard<'_, SqliteStore>> {
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

struct ChatCancellation<'a>(&'a Mutex<Option<Cancellation>>);

impl Drop for ChatCancellation<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = None;
        }
    }
}

fn cancellation_lock(
    slot: &Mutex<Option<Cancellation>>,
) -> Result<MutexGuard<'_, Option<Cancellation>>> {
    slot.lock().map_err(|_| {
        InferenceError::new(
            ErrorCode::WorkerStopped,
            "The Chat cancellation slot is unavailable.",
        )
    })
}

fn check_chat_cancellation(cancellation: &Cancellation) -> Result<()> {
    if cancellation.is_explicitly_cancelled() {
        return Err(InferenceError::new(
            ErrorCode::Cancelled,
            "Chat preparation was cancelled.",
        ));
    }
    Ok(())
}

impl Desktop {
    pub(crate) fn reserve(&self) -> Result<OperationGuard> {
        self.operations
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| {
                InferenceError::new(ErrorCode::Busy, "Wait for the current operation to finish.")
            })?;
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
    database(&state.database)?.delete(id)?;
    state.agent.forget_conversation(id);
    Ok(())
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
        .to_string_lossy()
        .into_owned();
    let path = path.to_path_buf();
    let inspected = path.clone();
    let (key, recommended) = tauri::async_runtime::spawn_blocking(move || {
        let key = crate::bundled_model::content_key(&inspected).map_err(|_| {
            InferenceError::new(ErrorCode::InvalidModel, "Could not read the GGUF model.")
        })?;
        let (trained_context, bytes) = inference::trained_context(&inspected)?;
        Ok::<_, InferenceError>((key, recommended_for_model(trained_context, bytes)))
    })
    .await
    .map_err(|_| {
        InferenceError::new(ErrorCode::InvalidModel, "Could not inspect the GGUF model.")
    })??;
    // A sampled match only nominates candidates. The same weights at another
    // path reuse the listed row and its settings only when full digests match.
    let candidates = database(&state.database)?.models_with_content_key(&key)?;
    let mut digest = None;
    if !candidates.is_empty() {
        let chosen = path.clone();
        let unknown: Vec<_> = candidates
            .iter()
            .filter(|(_, _, sha)| sha.is_none())
            .map(|(id, path, _)| (*id, path.clone()))
            .collect();
        let (chosen, known) = tauri::async_runtime::spawn_blocking(move || {
            let chosen = crate::bundled_model::sha256_hex(&chosen).ok();
            let known: Vec<_> = unknown
                .into_iter()
                .filter_map(|(id, path)| {
                    crate::bundled_model::sha256_hex(&path)
                        .ok()
                        .map(|sha| (id, sha))
                })
                .collect();
            (chosen, known)
        })
        .await
        .map_err(|_| {
            InferenceError::new(ErrorCode::InvalidModel, "Could not inspect the GGUF model.")
        })?;
        let mut db = database(&state.database)?;
        for (id, sha) in &known {
            db.set_model_sha256(*id, sha)?;
        }
        if let Some(chosen) = &chosen
            && let Some(existing) = db.model_with_sha256(chosen)?
        {
            db.mark_model_used(existing.id)?;
            return db.model(existing.id).map(|model| Some(model.reference));
        }
        digest = chosen;
    }
    let mut db = database(&state.database)?;
    let reference = db.remember_model(&path, &name, recommended)?;
    db.set_model_content_key(reference.id, &key)?;
    if let Some(digest) = digest {
        db.set_model_sha256(reference.id, &digest)?;
    }
    db.model(reference.id).map(|model| Some(model.reference))
}

/// Listed models after collapsing copies of the same weights.
#[tauri::command]
pub fn list_models(state: State<'_, Desktop>) -> Result<Vec<ModelReference>> {
    database(&state.database)?.recent_models()
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

/// Recommended settings for a model file on this machine.
async fn recommended_for_file(path: PathBuf, current: ModelSettings) -> Result<ModelSettings> {
    let mut settings = tauri::async_runtime::spawn_blocking(move || {
        let (trained_context, bytes) = inference::trained_context(&path)?;
        Ok::<_, InferenceError>(recommended_for_model(trained_context, bytes))
    })
    .await
    .map_err(|_| {
        InferenceError::new(ErrorCode::InvalidModel, "Could not inspect the GGUF model.")
    })??;
    // Thinking is a per-request choice; keep whatever the row already holds.
    settings.enable_thinking = current.enable_thinking;
    Ok(settings)
}

/// Loads a model. Settings equal to the saved settings mean "use this
/// model's settings": automatic models are re-recommended for the current
/// hardware. Different settings are an explicit choice and become custom.
#[tauri::command]
pub async fn load_model(
    state: State<'_, Desktop>,
    model_id: ModelId,
    settings: ModelSettings,
) -> Result<ModelInfo> {
    let _operation = state.reserve()?;
    settings.validate()?;
    let saved = database(&state.database)?.model(model_id)?;
    let explicit = settings != saved.reference.settings;
    let automatic = saved.reference.automatic_settings && !explicit;
    let settings = if automatic {
        recommended_for_file(saved.path.clone(), saved.reference.settings).await?
    } else {
        settings
    };
    settings.validate()?;
    let info = wait_for(state.worker.load(saved.path, settings.model)?).await?;
    let stored = {
        let mut db = database(&state.database)?;
        db.set_model_settings(model_id, settings, automatic)
            .and_then(|()| db.mark_model_used(model_id))
            .and_then(|()| db.set_last_chat_model(model_id))
    };
    if let Err(error) = stored {
        let _ = wait_for(state.worker.unload()?).await;
        return Err(error);
    }
    state.active_model.store(model_id, Ordering::Release);
    Ok(info)
}

/// Saves settings for later loads. Changed settings become custom.
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
    let mut db = database(&state.database)?;
    let saved = db.model(model_id)?.reference;
    let automatic = saved.automatic_settings && saved.settings == settings;
    db.set_model_settings(model_id, settings, automatic)
}

/// Returns a model to automatic settings and stores the current
/// recommendation. A loaded model keeps its context and threads until it is
/// loaded again.
#[tauri::command]
pub async fn reset_model_settings(
    state: State<'_, Desktop>,
    model_id: ModelId,
) -> Result<ModelReference> {
    let _operation = state.reserve()?;
    let saved = database(&state.database)?.model(model_id)?;
    let mut settings = recommended_for_file(saved.path, saved.reference.settings).await?;
    if state.active_model() == Some(model_id)
        && let Some(model) = state.worker.status()?.model
    {
        // Sampling applies now; context and threads apply at the next load.
        settings.model = model.config;
        settings.max_tokens = settings.max_tokens.min(model.config.context_length - 1);
    }
    let mut db = database(&state.database)?;
    db.set_model_settings(model_id, settings, true)?;
    Ok(db.model(model_id)?.reference)
}

#[tauri::command]
pub async fn unload_model(state: State<'_, Desktop>) -> Result<()> {
    let _operation = state.reserve()?;
    wait_for(state.worker.unload()?).await?;
    state.active_model.store(0, Ordering::Release);
    database(&state.database)?.set_chat_model_unloaded(true)?;
    Ok(())
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    Started {
        user: Option<Message>,
        assistant: Box<Message>,
    },
    Knowledge {
        coverage: KnowledgeCoverage,
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

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatOptions {
    #[serde(default)]
    enable_thinking: bool,
    max_tokens: Option<u32>,
    knowledge: Option<KnowledgeSelection>,
}

#[cfg(test)]
#[test]
fn chat_options_default_to_direct_and_use_camel_case() {
    let direct: ChatOptions = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(!direct.enable_thinking);
    assert_eq!(direct.max_tokens, None);
    let explicit: ChatOptions =
        serde_json::from_value(serde_json::json!({"enableThinking":true,"maxTokens":768})).unwrap();
    assert!(explicit.enable_thinking);
    assert_eq!(explicit.max_tokens, Some(768));
    assert!(
        serde_json::from_value::<ChatOptions>(serde_json::json!({"enable_thinking":true})).is_err()
    );
}

async fn run_turn(
    state: &Desktop,
    semantic: &SemanticState,
    id: ConversationId,
    action: TurnAction,
    options: ChatOptions,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    let _operation = state.reserve()?;
    let cancellation = Cancellation::default();
    *cancellation_lock(&state.chat_cancellation)? = Some(cancellation.clone());
    let _active_chat = ChatCancellation(&state.chat_cancellation);
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
    let question = match &action {
        TurnAction::Send(content) => content.clone(),
        TurnAction::Regenerate => database(&state.database)?.chat_regeneration_prompt(id)?,
    };
    GenerationRequest::new(&question).validate()?;
    let mut settings = database(&state.database)?
        .model(model_id)?
        .reference
        .settings;
    settings.enable_thinking = options.enable_thinking;
    if let Some(max_tokens) = options.max_tokens {
        settings.max_tokens = max_tokens;
    }
    settings.validate()?;
    if settings.enable_thinking
        && !status
            .model
            .as_ref()
            .is_some_and(|model| model.supports_thinking)
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "This model does not support thinking mode. Send without thinking.",
        ));
    }

    let knowledge = options.knowledge;
    let strict = knowledge
        .as_ref()
        .is_some_and(|selection| selection.grounding == Grounding::Documents);
    let mut previous = None;
    let plan = match &knowledge {
        Some(selection) => {
            selection.validate()?;
            previous = database(&state.database)?
                .previous_document_question(id, matches!(action, TurnAction::Regenerate))?;
            Some(route(&question, previous.as_deref(), selection.grounding))
        }
        None => None,
    };
    // A refinement restates the previous question for the model; the saved
    // user message keeps the original text.
    let prompt = plan
        .as_ref()
        .filter(|plan| plan.refinement.is_some())
        .map(|plan| routed_question(plan, &question, previous.as_deref()));
    // Only questions are ranked against the request. Searches and overviews
    // read the selected documents directly and never need the dense model.
    let query = plan
        .as_ref()
        .filter(|plan| plan.intent == ChatIntent::Question)
        .and_then(|plan| retrieval_query(&plan.query));
    let mut requested_mode = None;
    let vector = match (&knowledge, &query) {
        (Some(selection), Some(query)) if selection.mode != RetrievalMode::Keyword => {
            match semantic
                .embed_chat_query(query.clone(), cancellation.clone())
                .await
            {
                Ok(vector) => Some(vector),
                Err(error) if selection.grounding == Grounding::Auto && dense_unusable(&error) => {
                    requested_mode = Some(selection.mode);
                    None
                }
                Err(error) => return Err(retrieval_error(error)),
            }
        }
        _ => None,
    };
    check_chat_cancellation(&cancellation)?;
    let model_name = status.model.map(|model| model.file_name);
    let store = Arc::clone(&state.database);
    let preparation = cancellation.clone();
    let (mut turn, context, sources, coverage, deterministic) =
        tauri::async_runtime::spawn_blocking(move || {
            let mut db = database(&store)?;
            check_chat_cancellation(&preparation)?;
            let retrieval = match (knowledge, plan) {
                (Some(selection), Some(plan)) => Some(
                    prepare_knowledge(
                        &db,
                        &selection,
                        &plan,
                        query.as_deref(),
                        vector.as_ref(),
                        requested_mode,
                    )
                    .map_err(retrieval_error)?,
                ),
                _ => None,
            };
            let memory_enabled = db.memory_enabled()?;
            let (sources, coverage, task) = retrieval.map_or_else(
                || (Vec::new(), None, EvidenceTask::Answer),
                |retrieval| (retrieval.sources, Some(retrieval.coverage), retrieval.task),
            );
            // A General route keeps the documents available but supplies no
            // evidence, so it is an ordinary attempt unless memories apply.
            let grounded = coverage
                .as_ref()
                .is_some_and(|coverage| coverage.intent != Some(ChatIntent::General));
            // Grounded Chat without evidence gets a fixed reply instead of a model
            // answer that could draw on history or memory.
            let deterministic = (grounded && sources.is_empty()).then(|| no_evidence_answer(&task));
            // Personal memory never supplements documents-only answers.
            let memories = if strict || deterministic.is_some() {
                Vec::new()
            } else {
                db.relevant_memories(&question)?
            };
            let context = (grounded || !memories.is_empty()).then(|| ChatContext {
                evidence: grounded.then(|| sources.iter().map(|hit| hit.text.clone()).collect()),
                memories,
                task,
            });
            check_chat_cancellation(&preparation)?;
            let turn = match (&context, &coverage) {
                (Some(_), _) => db.begin_chat_with_context(
                    id,
                    action,
                    model_name.as_deref(),
                    AttemptContext {
                        coverage: coverage.clone(),
                        memory_enabled: Some(memory_enabled),
                        fitted: None,
                    },
                )?,
                // Ordinary Chat with documents selected keeps its routing coverage
                // without becoming contextual, so history and Continue still work.
                (None, Some(route)) => db.begin_routed_chat_attempt(
                    id,
                    action,
                    model_name.as_deref(),
                    model_id,
                    settings,
                    route,
                )?,
                (None, None) => {
                    db.begin_chat_attempt(id, action, model_name.as_deref(), model_id, settings)?
                }
            };
            Ok((turn, context, sources, coverage, deterministic))
        })
        .await
        .map_err(|_| {
            InferenceError::new(ErrorCode::WorkerStopped, "Chat preparation stopped.")
        })??;
    if let Some(text) = deterministic {
        return finish_without_generation(state, turn, events, coverage, text);
    }
    if let Some(prompt) = prompt {
        turn.prompt = prompt;
    }
    // Evidence answers see no earlier turns: ordinary answers in history must
    // never stand in for documents, and contextual answers are already omitted.
    if context
        .as_ref()
        .is_some_and(|context| context.evidence.is_some())
    {
        turn.history.turns.clear();
    }
    let mut request = settings.request(
        std::mem::take(&mut turn.prompt),
        std::mem::take(&mut turn.history),
    );
    request.chat = Some(agent_core::inference::ChatGeneration {
        adaptive: options.max_tokens.is_none(),
        replay: None,
    });
    request.context = context;
    stream_chat(
        state,
        turn,
        request,
        events,
        StreamContext {
            previous: None,
            cancellation,
            sources,
            coverage,
        },
    )
    .await
}

/// Save and stream a fixed reply for grounded Chat without evidence.
fn finish_without_generation(
    state: &Desktop,
    turn: agent_core::chat::PendingTurn,
    events: Channel<ChatEvent>,
    coverage: Option<KnowledgeCoverage>,
    text: String,
) -> Result<Message> {
    let mut assistant = turn.assistant;
    let id = assistant.id;
    let mut db = database(&state.database)?;
    if let Some(context) = assistant.context.clone() {
        db.checkpoint_chat_evidence(
            id,
            &context,
            agent_core::chat_context::ContextUsage::default(),
            &[],
        )?;
    }
    if let Some(coverage) = coverage {
        let _ = events.send(ChatEvent::Knowledge { coverage });
    }
    let _ = events.send(ChatEvent::Started {
        user: turn.user,
        assistant: Box::new(assistant.clone()),
    });
    let _ = events.send(ChatEvent::Delta {
        id,
        text: text.clone(),
    });
    db.finish(id, &text, GenerationState::Complete, None)?;
    assistant.content = text;
    assistant.state = GenerationState::Complete;
    if assistant.context.is_some() {
        assistant.context = db.chat_attempt_context(id)?.map(Box::new);
    }
    Ok(assistant)
}

fn dense_unusable(error: &AgentError) -> bool {
    matches!(
        error.code,
        AgentErrorCode::EmbeddingUnavailable
            | AgentErrorCode::EmbeddingIndexUnavailable
            | AgentErrorCode::EmbeddingIncompatible
    )
}

struct PreparedKnowledge {
    coverage: KnowledgeCoverage,
    sources: Vec<KnowledgeHit>,
    task: EvidenceTask,
}

/// Retrieve evidence for a routed request. Under automatic grounding a
/// question with no relevant passage becomes General, and an unusable dense
/// model or index falls back to keyword ranking with the requested mode kept
/// in coverage.
fn prepare_knowledge(
    db: &SqliteStore,
    selection: &KnowledgeSelection,
    plan: &RoutePlan,
    query: Option<&str>,
    vector: Option<&UnitVector>,
    requested_mode: Option<RetrievalMode>,
) -> agent_core::agent::AgentResult<PreparedKnowledge> {
    let ids = &selection.document_ids;
    let (retrieval, task, mut intent) = match plan.intent {
        ChatIntent::Search => (
            db.search_for_chat(&plan.query, ids, selection.mode)?,
            EvidenceTask::Search {
                target: plan.query.clone(),
            },
            ChatIntent::Search,
        ),
        ChatIntent::Overview => {
            let retrieval = db.overview_for_chat(ids, selection.mode)?;
            let total_chunks = retrieval
                .coverage
                .overview
                .iter()
                .map(|document| document.total_chunks)
                .sum();
            (
                retrieval,
                EvidenceTask::Overview { total_chunks },
                ChatIntent::Overview,
            )
        }
        ChatIntent::Question | ChatIntent::General => {
            let mut requested_mode = requested_mode;
            let mode = if requested_mode.is_some() {
                RetrievalMode::Keyword
            } else {
                selection.mode
            };
            let retrieval = match db.retrieve_for_chat(query, ids, mode, BGE_KEY, vector) {
                Err(error)
                    if selection.grounding == Grounding::Auto
                        && mode != RetrievalMode::Keyword
                        && dense_unusable(&error) =>
                {
                    requested_mode = Some(mode);
                    db.retrieve_for_chat(query, ids, RetrievalMode::Keyword, BGE_KEY, None)?
                }
                result => result?,
            };
            let mut retrieval = retrieval;
            retrieval.coverage.requested_mode = requested_mode;
            (retrieval, EvidenceTask::Answer, ChatIntent::Question)
        }
    };
    // Automatic grounding answers an unrelated request as ordinary Chat, but
    // an explicit document question without evidence stays grounded.
    if intent == ChatIntent::Question
        && retrieval.sources.is_empty()
        && selection.grounding == Grounding::Auto
        && !plan.refers_to_documents
    {
        intent = ChatIntent::General;
    }
    let mut coverage = retrieval.coverage;
    coverage.intent = Some(intent);
    coverage.grounding = Some(selection.grounding);
    Ok(PreparedKnowledge {
        coverage,
        sources: retrieval.sources,
        task,
    })
}

#[derive(Default)]
struct StreamContext {
    previous: Option<GenerationStats>,
    cancellation: Cancellation,
    sources: Vec<KnowledgeHit>,
    coverage: Option<KnowledgeCoverage>,
}

async fn stream_chat(
    state: &Desktop,
    turn: agent_core::chat::PendingTurn,
    mut request: GenerationRequest,
    events: Channel<ChatEvent>,
    context: StreamContext,
) -> Result<Message> {
    let StreamContext {
        previous,
        cancellation,
        sources,
        coverage,
    } = context;
    let mut assistant = turn.assistant;
    let message_id = assistant.id;
    if let Some(context) = assistant.context.clone() {
        let store = Arc::clone(&state.database);
        request.context_observer = Some(ContextObserver::new(move |usage| {
            database(&store)?.checkpoint_chat_evidence(message_id, &context, usage, &sources)
        }));
    }
    let transcript = Arc::new(Mutex::new(Transcript {
        text: assistant.content.clone(),
        checkpoint_at: Instant::now(),
        checkpoint_bytes: assistant.content.len(),
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
                .generate_with_cancellation(request, cancellation, move |delta| {
                    transcript_lock(&buffer)?.push(&delta.text, &db, message_id)?;
                    let _ = deltas.send(ChatEvent::Delta {
                        id: message_id,
                        text: delta.text,
                    });
                    Ok(())
                });
        if pending.is_ok() {
            if let Some(coverage) = coverage {
                let _ = events.send(ChatEvent::Knowledge { coverage });
            }
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
    let content = std::mem::take(&mut transcript_lock(&transcript)?.text);
    let generated = match generated {
        Ok(mut stats) => {
            if let Some(prior) = &previous {
                stats.generated_tokens = stats
                    .generated_tokens
                    .saturating_add(prior.generated_tokens);
            }
            stats
        }
        Err(error) => {
            let (state_after, stats) = if content == assistant.content && previous.is_some() {
                (GenerationState::Truncated, previous.as_ref())
            } else {
                (GenerationState::Failed, None)
            };
            database(&state.database)?.finish(message_id, &content, state_after, stats)?;
            return Err(error);
        }
    };
    assistant.content = content;
    assistant.state = GenerationState::from_output(generated.reason, &assistant.content);
    let mut db = database(&state.database)?;
    db.finish(
        message_id,
        &assistant.content,
        assistant.state,
        Some(&generated),
    )?;
    if assistant.context.is_some() {
        assistant.context = db.chat_attempt_context(message_id)?.map(Box::new);
    }
    assistant.stats = Some(generated);
    Ok(assistant)
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Desktop>,
    semantic: State<'_, SemanticState>,
    id: ConversationId,
    content: String,
    options: Option<ChatOptions>,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    run_turn(
        &state,
        &semantic,
        id,
        TurnAction::Send(content),
        options.unwrap_or_default(),
        events,
    )
    .await
}

#[tauri::command]
pub async fn regenerate(
    state: State<'_, Desktop>,
    semantic: State<'_, SemanticState>,
    id: ConversationId,
    options: Option<ChatOptions>,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    run_turn(
        &state,
        &semantic,
        id,
        TurnAction::Regenerate,
        options.unwrap_or_default(),
        events,
    )
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendedChatSettings {
    settings: ModelSettings,
    trained_context: Option<u32>,
}

pub(crate) fn recommended_for_model(trained_context: u32, file_bytes: u64) -> ModelSettings {
    // Leave room for the inference context and other allocations on Linux.
    // MemAvailable includes reclaimable cache, such as mapped model pages.
    let available = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| {
            let line = info
                .lines()
                .find_map(|line| line.strip_prefix("MemAvailable:"))?;
            let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
            kib.checked_mul(1024)
        });
    let context = match available {
        Some(bytes) if bytes >= file_bytes.saturating_add(2 << 30) => 8192,
        Some(bytes) if bytes < file_bytes.saturating_add(1 << 30) => 2048,
        _ => 4096,
    };
    ModelSettings::recommended(trained_context.min(context))
}

#[tauri::command]
pub fn recommended_chat_settings(state: State<'_, Desktop>) -> Result<RecommendedChatSettings> {
    let model = state.worker.status()?.model;
    let trained_context = model.as_ref().map(|model| model.trained_context);
    let settings = model.map_or_else(
        || ModelSettings::recommended(8192),
        |model| recommended_for_model(model.trained_context, model.file_bytes),
    );
    Ok(RecommendedChatSettings {
        settings,
        trained_context,
    })
}

#[tauri::command]
pub fn chat_continuation_remaining(
    state: State<'_, Desktop>,
    id: ConversationId,
    message_id: MessageId,
) -> Result<u32> {
    let model = state.active_model().ok_or_else(|| {
        InferenceError::new(
            ErrorCode::NoModel,
            "Load the original model to continue this response.",
        )
    })?;
    database(&state.database)?.continuation_remaining(id, message_id, model)
}

#[tauri::command]
pub async fn continue_chat(
    state: State<'_, Desktop>,
    id: ConversationId,
    message_id: MessageId,
    max_tokens: Option<u32>,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    let _operation = state.reserve()?;
    if max_tokens.is_some_and(|value| !(1..=4096).contains(&value)) {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Use 1–4096 continuation tokens.",
        ));
    }
    let model = state.active_model().ok_or_else(|| {
        InferenceError::new(
            ErrorCode::NoModel,
            "Load the original model to continue this response.",
        )
    })?;
    let status = state.worker.status()?;
    if status.phase != Phase::Ready {
        return Err(InferenceError::new(
            ErrorCode::Busy,
            "Wait for the model to be ready.",
        ));
    }
    let (mut plan, saved_limit) = {
        let mut db = database(&state.database)?;
        let saved_limit = db.model(model)?.reference.settings.max_tokens;
        (db.begin_continuation(id, message_id, model)?, saved_limit)
    };
    let mut request = plan.settings.request(
        std::mem::take(&mut plan.turn.prompt),
        std::mem::take(&mut plan.turn.history),
    );
    request.max_tokens = max_tokens.unwrap_or(saved_limit);
    plan.previous_stats.assistant_replay = Some(plan.replay.clone());
    request.chat = Some(agent_core::inference::ChatGeneration {
        adaptive: true,
        replay: Some(plan.replay),
    });
    stream_chat(
        &state,
        plan.turn,
        request,
        events,
        StreamContext {
            previous: Some(plan.previous_stats),
            ..StreamContext::default()
        },
    )
    .await
}

#[tauri::command]
pub fn stop_generation(state: State<'_, Desktop>) -> Result<bool> {
    if state.agent.is_active() {
        return Err(InferenceError::new(
            ErrorCode::Busy,
            "Cancel the active agent task from Agent mode.",
        ));
    }
    if let Some(cancellation) = cancellation_lock(&state.chat_cancellation)?.as_ref() {
        cancellation.cancel();
        return Ok(true);
    }
    state.worker.cancel()
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;
