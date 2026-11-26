//! Chat orchestration: routing, knowledge and memory context, streaming,
//! persistence and cancellation.

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use agent_core::{
    agent::{AgentError, AgentErrorCode},
    chat::{ChatStore, ConversationId, GenerationState, Message, MessageId, TurnAction},
    chat_context::{
        AttemptContext, ChatContext, ChatIntent, ContextObserver, EvidenceTask, Grounding,
        no_evidence_answer,
    },
    chat_route::{RoutePlan, route, routed_question},
    inference::{
        Cancellation, ErrorCode, GenerationRequest, GenerationStats, InferenceError, Result,
    },
    knowledge::{MAX_DOCUMENT_ID, MAX_DOCUMENTS},
    semantic::{BGE_KEY, UnitVector},
};
use inference::{Pending, Phase};
use serde::{Deserialize, Serialize};
use storage::{
    SqliteStore, chat_context::KnowledgeCoverage, knowledge::KnowledgeHit, semantic::RetrievalMode,
};

use crate::{Engine, database};

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

/// Receives Chat events. Delivery is best effort: a closed surface never
/// stops generation or persistence.
pub trait ChatEventSink: Send + Sync + 'static {
    fn send(&self, event: ChatEvent);
}

pub type ChatEvents = Arc<dyn ChatEventSink>;

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

#[derive(Default)]
struct StreamContext {
    previous: Option<GenerationStats>,
    cancellation: Cancellation,
    sources: Vec<KnowledgeHit>,
    coverage: Option<KnowledgeCoverage>,
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
        self.run_turn(id, TurnAction::Send(content), options, events)
            .await
    }

    /// Generate a new assistant attempt for the latest user message.
    pub async fn regenerate(
        &self,
        id: ConversationId,
        options: ChatOptions,
        events: ChatEvents,
    ) -> Result<Message> {
        self.run_turn(id, TurnAction::Regenerate, options, events)
            .await
    }

    async fn run_turn(
        &self,
        id: ConversationId,
        action: TurnAction,
        options: ChatOptions,
        events: ChatEvents,
    ) -> Result<Message> {
        let _operation = self.reserve()?;
        let cancellation = Cancellation::default();
        *cancellation_lock(&self.chat_cancellation)? = Some(cancellation.clone());
        let _active_chat = ChatCancellation(&self.chat_cancellation);
        let model_id = self
            .active_model()
            .ok_or_else(|| InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first."))?;
        let status = self.worker.status()?;
        if status.phase != Phase::Ready {
            return Err(InferenceError::new(
                ErrorCode::Busy,
                "Wait for the model to be ready.",
            ));
        }
        let question = match &action {
            TurnAction::Send(content) => content.clone(),
            TurnAction::Regenerate => database(&self.database)?.chat_regeneration_prompt(id)?,
        };
        GenerationRequest::new(&question).validate()?;
        let mut settings = database(&self.database)?
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
                previous = database(&self.database)?
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
                match self
                    .semantic
                    .embed_chat_query(query.clone(), cancellation.clone())
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
        let model_name = status.model.map(|model| model.file_name);
        let store = Arc::clone(&self.database);
        let preparation = cancellation.clone();
        let (mut turn, context, sources, coverage, deterministic) = self
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
                // Grounded Chat without evidence gets a fixed reply instead of a
                // model answer that could draw on history or memory.
                let deterministic =
                    (grounded && sources.is_empty()).then(|| no_evidence_answer(&task));
                // Personal memory never supplements documents-only answers.
                let memories = if strict || deterministic.is_some() {
                    Vec::new()
                } else {
                    db.relevant_memories(&question)?
                };
                let context = (grounded || !memories.is_empty()).then(|| ChatContext {
                    evidence: grounded
                        .then(|| sources.iter().map(|hit| hit.text.clone()).collect()),
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
                    // Ordinary Chat with documents selected keeps its routing
                    // coverage without becoming contextual, so history and
                    // Continue still work.
                    (None, Some(route)) => db.begin_routed_chat_attempt(
                        id,
                        action,
                        model_name.as_deref(),
                        model_id,
                        settings,
                        route,
                    )?,
                    (None, None) => db.begin_chat_attempt(
                        id,
                        action,
                        model_name.as_deref(),
                        model_id,
                        settings,
                    )?,
                };
                Ok((turn, context, sources, coverage, deterministic))
            })
            .await
            .ok_or_else(|| {
                InferenceError::new(ErrorCode::WorkerStopped, "Chat preparation stopped.")
            })??;
        if let Some(text) = deterministic {
            return self.finish_without_generation(turn, &events, coverage, text);
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
        self.stream_chat(
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
        &self,
        turn: agent_core::chat::PendingTurn,
        events: &ChatEvents,
        coverage: Option<KnowledgeCoverage>,
        text: String,
    ) -> Result<Message> {
        let mut assistant = turn.assistant;
        let id = assistant.id;
        let mut db = database(&self.database)?;
        if let Some(context) = assistant.context.clone() {
            db.checkpoint_chat_evidence(
                id,
                &context,
                agent_core::chat_context::ContextUsage::default(),
                &[],
            )?;
        }
        if let Some(coverage) = coverage {
            events.send(ChatEvent::Knowledge { coverage });
        }
        events.send(ChatEvent::Started {
            user: turn.user,
            assistant: Box::new(assistant.clone()),
        });
        events.send(ChatEvent::Delta {
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
        } = context;
        let mut assistant = turn.assistant;
        let message_id = assistant.id;
        if let Some(context) = assistant.context.clone() {
            let store = Arc::clone(&self.database);
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
        let db = Arc::clone(&self.database);
        let pending = {
            // Reserve the worker's cancellation slot before notifying the UI, and
            // hold back deltas until the initial message has been sent.
            let _first_event = transcript_lock(&transcript)?;
            let deltas = Arc::clone(&events);
            let pending =
                self.worker
                    .generate_with_cancellation(request, cancellation, move |delta| {
                        transcript_lock(&buffer)?.push(&delta.text, &db, message_id)?;
                        deltas.send(ChatEvent::Delta {
                            id: message_id,
                            text: delta.text,
                        });
                        Ok(())
                    });
            if pending.is_ok() {
                if let Some(coverage) = coverage {
                    events.send(ChatEvent::Knowledge { coverage });
                }
                events.send(ChatEvent::Started {
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
                database(&self.database)?.finish(message_id, &content, state_after, stats)?;
                return Err(error);
            }
        };
        assistant.content = content;
        assistant.state = GenerationState::from_output(generated.reason, &assistant.content);
        let mut db = database(&self.database)?;
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
        let _operation = self.reserve()?;
        if max_tokens.is_some_and(|value| !(1..=4096).contains(&value)) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Use 1–4096 continuation tokens.",
            ));
        }
        let model = self.active_model().ok_or_else(|| {
            InferenceError::new(
                ErrorCode::NoModel,
                "Load the original model to continue this response.",
            )
        })?;
        let status = self.worker.status()?;
        if status.phase != Phase::Ready {
            return Err(InferenceError::new(
                ErrorCode::Busy,
                "Wait for the model to be ready.",
            ));
        }
        let (mut plan, saved_limit) = {
            let mut db = database(&self.database)?;
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
        self.stream_chat(
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

#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;
