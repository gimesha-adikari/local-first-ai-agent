//! Chat orchestration: routing, knowledge and memory context, streaming,
//! persistence and cancellation.

use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use agent_core::{
    agent::{AgentError, AgentErrorCode},
    capability::{Generate, Invocation, Invoke},
    chat::{
        ChatHistory, ChatStore, ConversationId, GenerationState, Message, MessageId, ModelId,
        PendingTurn, TurnAction,
    },
    chat_context::{
        AttemptContext, ChatContext, ChatIntent, ContextObserver, EvidenceTask, Grounding,
        no_evidence_answer,
    },
    chat_route::{RoutePlan, route, routed_question},
    classification::{
        Classified, HISTORY, Labels, MEMORY, MODEL_OUTPUT, QUERY, SYSTEM_TEXT, USER_INPUT,
    },
    inference::{
        Cancellation, ErrorCode, GenerationRequest, GenerationStats, InferenceError, Result,
    },
    intelligence::{ContextSource, SessionState, SourceKind},
    knowledge::{MAX_DOCUMENT_ID, MAX_DOCUMENTS},
    semantic::{BGE_KEY, UnitVector},
};
use inference::{LocalInference, Pending, Phase};
use serde::{Deserialize, Serialize};
use storage::{
    SqliteStore, chat_context::KnowledgeCoverage, knowledge::KnowledgeHit, semantic::RetrievalMode,
};

use crate::{
    Engine, OperationGuard, Platform,
    chat_session::{Candidates, ChatSession},
    database,
    network::NetworkBoundary,
    policy::Policy,
    semantic::SemanticState,
};

#[path = "remote_chat.rs"]
mod remote;

/// Events streamed to the surface that started a Chat turn.
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

/// Receives best-effort Chat events. Delivery failures and unwind panics do
/// not stop generation or persistence. A sink may read the conversation
/// synchronously but must return before waiting for more events from the turn.
pub trait ChatEventSink: Send + Sync + 'static {
    fn send(&self, event: ChatEvent);
}

pub type ChatEvents = Arc<dyn ChatEventSink>;

fn deliver(events: &ChatEvents, event: ChatEvent) {
    // A surface callback must not unwind through the transcript guard or
    // the dedicated inference worker.
    let _ = catch_unwind(AssertUnwindSafe(|| events.send(event)));
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeSelection {
    pub mode: RetrievalMode,
    pub document_ids: Vec<i64>,
    /// Omitted means automatic: documents are available without blocking
    /// unrelated requests.
    #[serde(default)]
    pub grounding: Grounding,
}

impl KnowledgeSelection {
    pub fn validate(&self) -> Result<()> {
        if self.document_ids.is_empty()
            || self.document_ids.len() > MAX_DOCUMENTS
            || self
                .document_ids
                .iter()
                .any(|id| !(1..=MAX_DOCUMENT_ID).contains(id))
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Select 1–128 valid knowledge document IDs.",
            ));
        }
        Ok(())
    }
}

/// The bounded keyword form of a question used for retrieval.
pub fn retrieval_query(question: &str) -> Option<String> {
    let mut query = String::new();
    for term in question
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(16)
    {
        let remaining = 256 - query.len() - usize::from(!query.is_empty());
        let boundary = term.floor_char_boundary(remaining);
        if boundary == 0 {
            break;
        }
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(&term[..boundary]);
        if boundary < term.len() || query.len() >= 255 {
            break;
        }
    }
    query.chars().any(char::is_alphanumeric).then_some(query)
}

/// Map a retrieval failure onto the Chat error contract.
pub fn retrieval_error(error: AgentError) -> InferenceError {
    let code = match error.code {
        AgentErrorCode::EmbeddingUnavailable => ErrorCode::EmbeddingUnavailable,
        AgentErrorCode::EmbeddingIncompatible => ErrorCode::EmbeddingIncompatible,
        AgentErrorCode::EmbeddingIndexUnavailable => ErrorCode::EmbeddingIndexUnavailable,
        AgentErrorCode::InvalidArguments => ErrorCode::InvalidConfig,
        AgentErrorCode::Cancelled => ErrorCode::Cancelled,
        AgentErrorCode::TimedOut => ErrorCode::TimedOut,
        AgentErrorCode::StorageFailed => ErrorCode::Storage,
        _ => ErrorCode::GenerationFailed,
    };
    InferenceError::new(code, error.message)
}

/// Registers a turn's cancellation for Stop and clears it when dropped.
struct ActiveChat(Arc<Mutex<Option<Cancellation>>>);

impl ActiveChat {
    fn register(
        slot: &Arc<Mutex<Option<Cancellation>>>,
        cancellation: Cancellation,
    ) -> Result<Self> {
        *cancellation_lock(slot)? = Some(cancellation);
        Ok(Self(Arc::clone(slot)))
    }
}

impl Drop for ActiveChat {
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

pub(crate) async fn wait_for<T>(pending: Pending<T>) -> Result<T> {
    pending.await.map_err(|_| {
        InferenceError::new(ErrorCode::WorkerStopped, "The inference worker stopped.")
    })?
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

pub(crate) fn dense_unusable(error: &AgentError) -> bool {
    matches!(
        error.code,
        AgentErrorCode::EmbeddingUnavailable
            | AgentErrorCode::EmbeddingIndexUnavailable
            | AgentErrorCode::EmbeddingIncompatible
    )
}

pub(crate) struct PreparedKnowledge {
    pub(crate) coverage: KnowledgeCoverage,
    pub(crate) sources: Vec<KnowledgeHit>,
    pub(crate) task: EvidenceTask,
}

/// Retrieve evidence for a routed request. Under automatic grounding a
/// question with no relevant passage becomes General, and an unusable dense
/// model or index falls back to keyword ranking with the requested mode kept
/// in coverage.
pub(crate) fn prepare_knowledge(
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

struct StreamContext {
    previous: Option<GenerationStats>,
    cancellation: Cancellation,
    sources: Vec<KnowledgeHit>,
    coverage: Option<KnowledgeCoverage>,
    candidates: Candidates,
}

/// Everything a detached Chat turn needs, shared with the engine.
#[derive(Clone)]
struct ChatRuntime {
    platform: Arc<dyn Platform>,
    adapter: LocalInference,
    policy: Arc<Policy>,
    database: Arc<Mutex<SqliteStore>>,
    semantic: Arc<SemanticState>,
    boundary: Arc<NetworkBoundary>,
}

impl Engine {
    /// Send a new user message and stream the assistant response.
    pub async fn send_message(
        &self,
        id: ConversationId,
        content: String,
        options: ChatOptions,
        events: ChatEvents,
    ) -> Result<Message> {
        self.start_turn(id, TurnAction::Send(content), options, events)
            .await
    }

    /// Generate a new assistant attempt for the latest user message.
    pub async fn regenerate(
        &self,
        id: ConversationId,
        options: ChatOptions,
        events: ChatEvents,
    ) -> Result<Message> {
        self.start_turn(id, TurnAction::Regenerate, options, events)
            .await
    }

    async fn start_turn(
        &self,
        id: ConversationId,
        action: TurnAction,
        options: ChatOptions,
        events: ChatEvents,
    ) -> Result<Message> {
        let operation = self.reserve()?;
        let cancellation = Cancellation::default();
        let active = ActiveChat::register(&self.chat_cancellation, cancellation.clone())?;
        let model_id = self.active_model();
        let runtime = self.chat_runtime();
        self.detach(operation, active, async move {
            runtime
                .run_turn(id, action, options, events, cancellation, model_id)
                .await
        })
        .await
    }

    fn chat_runtime(&self) -> ChatRuntime {
        ChatRuntime {
            platform: Arc::clone(&self.platform),
            adapter: self.adapter.clone(),
            policy: Arc::clone(&self.policy),
            database: Arc::clone(&self.database),
            semantic: Arc::clone(&self.semantic),
            boundary: Arc::clone(&self.boundary),
        }
    }

    /// Run an accepted turn to completion independently of the caller. The
    /// task owns the operation lease and the Stop registration until the
    /// turn's terminal state is persisted and the worker no longer streams
    /// into it, and releases both before the result is delivered. A caller
    /// that stops waiting leaves the turn running; Stop still reaches it.
    async fn detach(
        &self,
        operation: OperationGuard,
        active: ActiveChat,
        turn: impl Future<Output = Result<Message>> + Send + 'static,
    ) -> Result<Message> {
        let (reply, result) = futures_channel::oneshot::channel();
        self.platform.spawn(Box::pin(async move {
            let outcome = turn.await;
            drop(active);
            drop(operation);
            let _ = reply.send(outcome);
        }));
        result.await.map_err(|_| {
            InferenceError::new(ErrorCode::WorkerStopped, "The Chat operation stopped.")
        })?
    }

    /// Tokens left for continuing a truncated response with the active model.
    pub fn continuation_remaining(&self, id: ConversationId, message_id: MessageId) -> Result<u32> {
        let model = self.active_model().ok_or_else(|| {
            InferenceError::new(
                ErrorCode::NoModel,
                "Load the original model to continue this response.",
            )
        })?;
        database(&self.database)?.continuation_remaining(id, message_id, model)
    }

    /// Continue a truncated assistant response in place.
    pub async fn continue_chat(
        &self,
        id: ConversationId,
        message_id: MessageId,
        max_tokens: Option<u32>,
        events: ChatEvents,
    ) -> Result<Message> {
        let operation = self.reserve()?;
        let cancellation = Cancellation::default();
        let active = ActiveChat::register(&self.chat_cancellation, cancellation.clone())?;
        let model = self.active_model();
        let runtime = self.chat_runtime();
        self.detach(operation, active, async move {
            runtime
                .continue_turn(id, message_id, max_tokens, model, events, cancellation)
                .await
        })
        .await
    }

    /// Cancel the active Chat preparation or generation. Returns whether
    /// anything was cancelled.
    pub fn stop_chat(&self) -> Result<bool> {
        if let Some(cancellation) = cancellation_lock(&self.chat_cancellation)?.as_ref() {
            cancellation.cancel();
            return Ok(true);
        }
        self.worker.cancel()
    }
}

impl ChatRuntime {
    async fn blocking<T: Send + 'static>(
        &self,
        task: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        crate::run_blocking(self.platform.as_ref(), task).await
    }

    async fn run_turn(
        &self,
        id: ConversationId,
        action: TurnAction,
        options: ChatOptions,
        events: ChatEvents,
        cancellation: Cancellation,
        model_id: Option<ModelId>,
    ) -> Result<Message> {
        // The saved selection decides the target. A remote provider is used
        // only when local-only mode is off and it is explicitly selected;
        // there is no fallback in either direction.
        let selection = database(&self.database)?.intelligence_settings()?;
        let remote = crate::providers::remote_target(&self.database, selection)?;
        let local = match &remote {
            Some(_) => None,
            None => {
                let model_id = model_id.ok_or_else(|| {
                    InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first.")
                })?;
                let status = self.adapter.status()?;
                if status.phase != Phase::Ready {
                    return Err(InferenceError::new(
                        ErrorCode::Busy,
                        "Wait for the model to be ready.",
                    ));
                }
                Some((model_id, status))
            }
        };
        let question = match &action {
            TurnAction::Send(content) => content.clone(),
            TurnAction::Regenerate => database(&self.database)?.chat_regeneration_prompt(id)?,
        };
        GenerationRequest::new(&question).validate()?;
        let recipe = match &local {
            Some((model_id, status)) => {
                let mut settings = database(&self.database)?
                    .model(*model_id)?
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
                Some((*model_id, settings))
            }
            None => {
                if options.enable_thinking {
                    return Err(InferenceError::new(
                        ErrorCode::UnsupportedCapability,
                        "Thinking mode is available only with a local model. Send without thinking.",
                    ));
                }
                if options
                    .max_tokens
                    .is_some_and(|value| !(1..=32_768).contains(&value))
                {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "Use 1–32768 output tokens.",
                    ));
                }
                None
            }
        };

        let knowledge = options.knowledge;
        let strict = knowledge
            .as_ref()
            .is_some_and(|selection| selection.grounding == Grounding::Documents);
        let mut previous = None;
        let plan = match &knowledge {
            Some(selection) => {
                selection.validate()?;
                previous = database(&self.database)?.previous_document_question_with_source(
                    id,
                    matches!(action, TurnAction::Regenerate),
                )?;
                Some(route(
                    &question,
                    previous.as_ref().map(|(question, _)| question.as_str()),
                    selection.grounding,
                ))
            }
            None => None,
        };
        // A refinement restates the previous question for the model; the saved
        // user message keeps the original text.
        let prompt = plan
            .as_ref()
            .filter(|plan| plan.refinement.is_some())
            .map(|plan| {
                routed_question(
                    plan,
                    &question,
                    previous.as_ref().map(|(question, _)| question.as_str()),
                )
            });
        // Only questions are ranked against the request. Searches and overviews
        // read the selected documents directly and never need the dense model.
        let query = plan
            .as_ref()
            .filter(|plan| plan.intent == ChatIntent::Question)
            .and_then(|plan| retrieval_query(&plan.query));
        if query.is_some()
            || plan
                .as_ref()
                .is_some_and(|plan| plan.intent != ChatIntent::General)
        {
            crate::semantic::authorize_retrieval(&self.policy).map_err(retrieval_error)?;
        }
        let mut requested_mode = None;
        let vector = match (&knowledge, &query) {
            (Some(selection), Some(query)) if selection.mode != RetrievalMode::Keyword => {
                match self
                    .semantic
                    .embed_chat_query(
                        Classified::from_source(query.clone(), USER_INPUT)
                            .map(QUERY, |value| value),
                        cancellation.clone(),
                    )
                    .await
                {
                    Ok(vector) => Some(vector),
                    Err(error)
                        if selection.grounding == Grounding::Auto && dense_unusable(&error) =>
                    {
                        requested_mode = Some(selection.mode);
                        None
                    }
                    Err(error) => return Err(retrieval_error(error)),
                }
            }
            _ => None,
        };
        check_chat_cancellation(&cancellation)?;
        let model_name = match (&local, &remote) {
            (Some((_, status)), _) => status.model.as_ref().map(|model| model.file_name.clone()),
            (None, Some((saved, _))) => Some(format!(
                "{} · {}",
                saved.settings.name, saved.settings.model
            )),
            (None, None) => None,
        };
        let store = Arc::clone(&self.database);
        let preparation = cancellation.clone();
        let (mut turn, context, sources, memories, coverage, deterministic) = self
            .blocking(move || {
                let mut db = database(&store)?;
                check_chat_cancellation(&preparation)?;
                let retrieval = match (knowledge, plan) {
                    (Some(selection), Some(plan)) => Some(
                        prepare_knowledge(
                            &db,
                            &selection,
                            &plan,
                            query.as_deref(),
                            vector.as_ref().map(Classified::value),
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
                // Grounded Chat without evidence gets a fixed reply instead of a
                // model answer that could draw on history or memory.
                let deterministic =
                    (grounded && sources.is_empty()).then(|| no_evidence_answer(&task));
                // Personal memory never supplements documents-only answers.
                let memories = if strict || deterministic.is_some() {
                    Vec::new()
                } else {
                    db.relevant_memory_records(&question)?
                };
                let memory_sources = memories
                    .iter()
                    .map(|memory| ContextSource {
                        kind: SourceKind::Memory,
                        record: Some(memory.id),
                        labels: memory.labels(),
                    })
                    .collect();
                let context = (grounded || !memories.is_empty()).then(|| ChatContext {
                    evidence: grounded
                        .then(|| sources.iter().map(|hit| hit.text.clone()).collect()),
                    memories: memories.into_iter().map(|memory| memory.content).collect(),
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
                    // Ordinary Chat with documents selected keeps its routing
                    // coverage without becoming contextual, so history and
                    // Continue still work.
                    (None, Some(route)) => match recipe {
                        Some((model_id, settings)) => db.begin_routed_chat_attempt(
                            id,
                            action,
                            model_name.as_deref(),
                            model_id,
                            settings,
                            route,
                        )?,
                        None => {
                            db.begin_routed_remote_chat(id, action, model_name.as_deref(), route)?
                        }
                    },
                    (None, None) => match recipe {
                        Some((model_id, settings)) => db.begin_chat_attempt(
                            id,
                            action,
                            model_name.as_deref(),
                            model_id,
                            settings,
                        )?,
                        None => db.begin_chat(id, action, model_name.as_deref())?,
                    },
                };
                Ok((
                    turn,
                    context,
                    sources,
                    memory_sources,
                    coverage,
                    deterministic,
                ))
            })
            .await
            .ok_or_else(|| {
                InferenceError::new(ErrorCode::WorkerStopped, "Chat preparation stopped.")
            })??;
        if let Some(text) = deterministic {
            return self.finish_without_generation(turn, &events, coverage, text, cancellation);
        }
        let mut candidates = context_candidates(&turn, false);
        if let Some(prompt) = prompt {
            turn.prompt = prompt;
            if let Some((_, source)) = previous {
                candidates.required.push(source);
            }
        }
        if strict
            && turn
                .assistant
                .context
                .as_ref()
                .is_some_and(|context| context.memory_enabled == Some(true))
        {
            candidates.withheld.push(ContextSource {
                kind: SourceKind::Memory,
                record: None,
                labels: Labels::from_source(MEMORY),
            });
        }
        if context.is_some() {
            candidates.required.push(ContextSource {
                kind: SourceKind::SystemPrompt,
                record: None,
                labels: Labels::from_source(SYSTEM_TEXT),
            });
        }
        candidates.knowledge = sources
            .iter()
            .map(|hit| ContextSource {
                kind: SourceKind::Knowledge,
                record: Some(hit.document_id),
                labels: hit.source.labels(),
            })
            .collect();
        candidates.memories = memories;
        // Evidence answers see no earlier turns: ordinary answers in history must
        // never stand in for documents, and contextual answers are already omitted.
        if context
            .as_ref()
            .is_some_and(|context| context.evidence.is_some())
        {
            turn.history.turns.clear();
            candidates.withheld.append(&mut candidates.history);
        }
        let Some((_, settings)) = recipe else {
            let target = remote.ok_or_else(|| {
                InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first.")
            })?;
            return self
                .stream_remote(
                    remote::RemoteTurn {
                        turn,
                        context,
                        candidates,
                        coverage,
                        sources,
                        max_tokens: options.max_tokens,
                        cancellation,
                    },
                    target,
                    events,
                )
                .await;
        };
        let mut request = settings.request(
            std::mem::take(&mut turn.prompt),
            std::mem::take(&mut turn.history),
        );
        request.chat = Some(agent_core::inference::ChatGeneration {
            adaptive: options.max_tokens.is_none(),
            replay: None,
        });
        request.context = context;
        self.stream_chat(
            turn,
            request,
            events,
            StreamContext {
                previous: None,
                cancellation,
                sources,
                coverage,
                candidates,
            },
        )
        .await
    }

    /// Save and stream a fixed reply for grounded Chat without evidence.
    fn finish_without_generation(
        &self,
        turn: agent_core::chat::PendingTurn,
        events: &ChatEvents,
        coverage: Option<KnowledgeCoverage>,
        text: String,
        cancellation: Cancellation,
    ) -> Result<Message> {
        let mut candidates = context_candidates(&turn, false);
        candidates.required.truncate(1);
        candidates.withheld.append(&mut candidates.history);
        let mut session = ChatSession::new(candidates, cancellation, SYSTEM_TEXT);
        let mut assistant = turn.assistant;
        let id = assistant.id;
        if let Err(error) = session.authorize(&self.policy, None) {
            let state = if error.code == ErrorCode::Cancelled {
                GenerationState::Cancelled
            } else {
                GenerationState::Failed
            };
            let receipt = session.receipt(
                if state == GenerationState::Cancelled {
                    SessionState::Cancelled
                } else {
                    SessionState::Failed
                },
                None,
            );
            database(&self.database)?.finish_intelligence_chat(id, "", state, None, &receipt)?;
            return Err(error);
        }
        session.fitted(Default::default(), 0)?;
        if let Some(context) = assistant.context.clone() {
            database(&self.database)?.checkpoint_chat_evidence(
                id,
                &context,
                agent_core::chat_context::ContextUsage::default(),
                &[],
            )?;
        }
        // Event sinks run without the store lock; they may read the turn.
        if let Some(coverage) = coverage {
            deliver(events, ChatEvent::Knowledge { coverage });
        }
        deliver(
            events,
            ChatEvent::Started {
                user: turn.user,
                assistant: Box::new(assistant.clone()),
            },
        );
        deliver(
            events,
            ChatEvent::Delta {
                id,
                text: text.clone(),
            },
        );
        let mut db = database(&self.database)?;
        let receipt = session.receipt(SessionState::Completed, None);
        db.finish_intelligence_chat(id, &text, GenerationState::Complete, None, &receipt)?;
        assistant.labels = receipt.output_labels;
        assistant.content = text;
        assistant.state = GenerationState::Complete;
        if assistant.context.is_some() {
            assistant.context = db.chat_attempt_context(id)?.map(Box::new);
        }
        Ok(assistant)
    }

    async fn stream_chat(
        &self,
        turn: agent_core::chat::PendingTurn,
        mut request: GenerationRequest,
        events: ChatEvents,
        context: StreamContext,
    ) -> Result<Message> {
        let StreamContext {
            previous,
            cancellation,
            sources,
            coverage,
            candidates,
        } = context;
        let mut assistant = turn.assistant;
        let message_id = assistant.id;
        let session = Arc::new(Mutex::new(Some(ChatSession::new(
            candidates,
            cancellation.clone(),
            MODEL_OUTPUT,
        ))));
        let store = Arc::clone(&self.database);
        let context = assistant.context.clone();
        let fitted = Arc::clone(&session);
        request.context_observer = Some(ContextObserver::new(move |usage, excluded_history| {
            let mut session = session_lock(&fitted)?;
            let labels = session
                .as_mut()
                .ok_or_else(session_error)?
                .fitted(usage, excluded_history)?;
            database(&store)?.checkpoint_intelligence_context(
                message_id,
                labels,
                context.as_deref(),
                usage,
                &sources,
            )
        }));
        let transcript = Arc::new(Mutex::new(Transcript {
            text: assistant.content.clone(),
            checkpoint_at: Instant::now(),
            checkpoint_bytes: assistant.content.len(),
        }));
        let buffer = Arc::clone(&transcript);
        let db = Arc::clone(&self.database);
        let pending = {
            // Reserve the worker's cancellation slot before notifying the UI, and
            // hold back deltas until the initial message has been sent.
            let _first_event = transcript_lock(&transcript)?;
            let deltas = Arc::clone(&events);
            let stream_session = Arc::clone(&session);
            let pending = self.adapter.offer::<Generate>().and_then(|offer| {
                let offer = offer.ok_or_else(|| {
                    InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first.")
                })?;
                session_lock(&session)?
                    .as_mut()
                    .ok_or_else(session_error)?
                    .authorize(&self.policy, Some(&offer))?;
                let pending = self.adapter.invoke(
                    Invocation::<Generate>::new(request, cancellation),
                    move |delta| {
                        if !session_lock(&stream_session)?
                            .as_ref()
                            .is_some_and(ChatSession::is_decoded)
                        {
                            return Err(session_error());
                        }
                        transcript_lock(&buffer)?.push(&delta.text, &db, message_id)?;
                        deliver(
                            &deltas,
                            ChatEvent::Delta {
                                id: message_id,
                                text: delta.text,
                            },
                        );
                        Ok(())
                    },
                )?;
                session_lock(&session)?
                    .as_mut()
                    .ok_or_else(session_error)?
                    .submitted(&offer);
                Ok(pending)
            });
            if pending.is_ok() {
                if let Some(coverage) = coverage {
                    deliver(&events, ChatEvent::Knowledge { coverage });
                }
                deliver(
                    &events,
                    ChatEvent::Started {
                        user: turn.user,
                        assistant: Box::new(assistant.clone()),
                    },
                );
            }
            pending
        };
        let generated = match pending {
            Ok(pending) => wait_for(pending).await,
            Err(error) => Err(error),
        };
        let content = std::mem::take(&mut transcript_lock(&transcript)?.text);
        let session = session_lock(&session)?.take().ok_or_else(session_error)?;
        let invocation_stats = generated.as_ref().ok().cloned();
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
                let persisted = if error.code == ErrorCode::Cancelled && previous.is_none() {
                    GenerationState::Cancelled
                } else {
                    state_after
                };
                let receipt = session.receipt(
                    if error.code == ErrorCode::Cancelled {
                        SessionState::Cancelled
                    } else {
                        SessionState::Failed
                    },
                    None,
                );
                database(&self.database)?
                    .finish_intelligence_chat(message_id, &content, persisted, stats, &receipt)?;
                return Err(error);
            }
        };
        assistant.content = content;
        assistant.state = GenerationState::from_output(generated.reason, &assistant.content);
        let mut db = database(&self.database)?;
        let terminal = if generated.reason == agent_core::inference::FinishReason::Cancelled {
            SessionState::Cancelled
        } else {
            SessionState::Completed
        };
        let receipt = session.receipt(terminal, invocation_stats.as_ref());
        db.finish_intelligence_chat(
            message_id,
            &assistant.content,
            assistant.state,
            Some(&generated),
            &receipt,
        )?;
        assistant.labels = receipt.output_labels;
        if assistant.context.is_some() {
            assistant.context = db.chat_attempt_context(message_id)?.map(Box::new);
        }
        assistant.stats = Some(generated);
        Ok(assistant)
    }

    async fn continue_turn(
        &self,
        id: ConversationId,
        message_id: MessageId,
        max_tokens: Option<u32>,
        model: Option<ModelId>,
        events: ChatEvents,
        cancellation: Cancellation,
    ) -> Result<Message> {
        check_chat_cancellation(&cancellation)?;
        if max_tokens.is_some_and(|value| !(1..=4096).contains(&value)) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Use 1–4096 continuation tokens.",
            ));
        }
        let model = model.ok_or_else(|| {
            InferenceError::new(
                ErrorCode::NoModel,
                "Load the original model to continue this response.",
            )
        })?;
        let status = self.adapter.status()?;
        if status.phase != Phase::Ready {
            return Err(InferenceError::new(
                ErrorCode::Busy,
                "Wait for the model to be ready.",
            ));
        }
        let (mut plan, saved_limit) = {
            let mut db = database(&self.database)?;
            let saved_limit = db.model(model)?.reference.settings.max_tokens;
            (
                db.begin_continuation(id, message_id, model, &cancellation)?,
                saved_limit,
            )
        };
        let candidates = context_candidates(&plan.turn, true);
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
        self.stream_chat(
            plan.turn,
            request,
            events,
            StreamContext {
                previous: Some(plan.previous_stats),
                cancellation,
                sources: Vec::new(),
                coverage: None,
                candidates,
            },
        )
        .await
    }
}

fn session_error() -> InferenceError {
    InferenceError::new(
        ErrorCode::GenerationFailed,
        "The fitted Chat session is unavailable.",
    )
}

fn session_lock(
    session: &Mutex<Option<ChatSession>>,
) -> Result<MutexGuard<'_, Option<ChatSession>>> {
    session.lock().map_err(|_| session_error())
}

fn history_sources(history: &ChatHistory) -> Vec<ContextSource> {
    history
        .turns
        .iter()
        .map(|turn| {
            turn.provenance.clone().unwrap_or_else(|| ContextSource {
                kind: SourceKind::History,
                record: None,
                labels: Labels::from_source(HISTORY),
            })
        })
        .collect()
}

fn context_candidates(turn: &PendingTurn, continuing: bool) -> Candidates {
    let mut required = vec![turn.prompt_source.clone()];
    if !turn.history.system.is_empty() {
        required.push(ContextSource {
            kind: SourceKind::SystemPrompt,
            record: None,
            labels: Labels::from_source(USER_INPUT),
        });
    }
    if continuing {
        required.push(ContextSource {
            kind: SourceKind::AssistantPrefix,
            record: Some(turn.assistant.id),
            labels: turn.assistant.labels.clone(),
        });
    }
    Candidates {
        required,
        history: history_sources(&turn.history),
        knowledge: Vec::new(),
        memories: Vec::new(),
        withheld: Vec::new(),
    }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;
