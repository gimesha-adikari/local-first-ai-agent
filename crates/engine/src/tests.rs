//! Engine-boundary tests with a scripted provider, a thread-backed platform
//! and a temporary in-memory store. No model files are used.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use agent_core::{
    agent::{AgentErrorCode, AgentResult},
    chat::{ChatStore, ConversationId, GenerationState, ModelId, ModelSettings},
    inference::{
        Cancellation, ErrorCode, FinishReason, GenerationRequest, GenerationStats, InferenceError,
        InferenceProvider, ModelConfig, ModelInfo, Result, ResumeState, TextDelta,
    },
    knowledge::{DocumentChunk, DocumentFormat, append_chunks},
};
use inference::{InferenceWorker, Phase};
use serde_json::{Value, json};
use storage::{
    SqliteStore,
    knowledge::{DocumentImport, ImportOutcome},
};

use crate::{
    BackgroundTask, BlockingTask, Engine, Platform,
    chat::{ChatEvent, ChatEventSink, ChatEvents, ChatOptions},
    knowledge::{DocumentContents, DocumentSource, ExtractedDocument, ImportCheck},
};

pub(crate) struct Threads;

impl Platform for Threads {
    fn spawn_blocking(&self, task: BlockingTask) {
        thread::spawn(task);
    }

    fn spawn(&self, task: BackgroundTask) {
        thread::spawn(move || futures_executor::block_on(task));
    }

    fn available_memory(&self) -> Option<u64> {
        Some(16 << 30)
    }
}

/// Hold an admitted Chat task before its first poll.
#[derive(Default)]
struct QueuedChat(Mutex<Option<BackgroundTask>>);

impl QueuedChat {
    fn run(&self) {
        let task = self
            .0
            .lock()
            .unwrap()
            .take()
            .expect("an admitted Chat task");
        futures_executor::block_on(task);
    }
}

impl Platform for QueuedChat {
    fn spawn_blocking(&self, task: BlockingTask) {
        Threads.spawn_blocking(task);
    }

    fn spawn(&self, task: BackgroundTask) {
        assert!(self.0.lock().unwrap().replace(task).is_none());
    }

    fn available_memory(&self) -> Option<u64> {
        Threads.available_memory()
    }
}

/// What the scripted provider saw for one generation.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    prompt: String,
    history_turns: usize,
    evidence: Option<Vec<String>>,
    memories: Vec<String>,
}

#[derive(Clone, Default)]
pub(crate) struct Script(Arc<Mutex<Vec<Seen>>>);

impl Script {
    fn seen(&self) -> Vec<Seen> {
        self.0.lock().unwrap().clone()
    }
}

pub(crate) struct Scripted(pub(crate) Script);

impl InferenceProvider for Scripted {
    fn load(&mut self, _: &Path, config: ModelConfig) -> Result<ModelInfo> {
        Ok(ModelInfo {
            name: "Scripted".into(),
            file_name: "scripted.gguf".into(),
            architecture: "scripted".into(),
            file_bytes: 0,
            parameters: 0,
            trained_context: 4096,
            supports_thinking: false,
            supports_agent: false,
            config,
            load_ms: 0,
        })
    }

    fn unload(&mut self) {}

    fn generate(
        &mut self,
        request: &GenerationRequest,
        cancellation: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats> {
        self.0.0.lock().unwrap().push(Seen {
            prompt: request.prompt.clone(),
            history_turns: request.history.turns.len(),
            evidence: request.context.as_ref().and_then(|c| c.evidence.clone()),
            memories: request
                .context
                .as_ref()
                .map_or_else(Vec::new, |c| c.memories.clone()),
        });
        if let Some(observer) = &request.context_observer {
            observer.decoded(
                request
                    .context
                    .as_ref()
                    .map_or_default(|context| context.usage()),
                0,
            )?;
        }
        let reason = if request.prompt.contains("wait") {
            emit(TextDelta {
                text: "Partial".into(),
            })?;
            let started = Instant::now();
            while !cancellation.is_cancelled() && started.elapsed() < Duration::from_secs(5) {
                thread::sleep(Duration::from_millis(5));
            }
            FinishReason::Cancelled
        } else if request.prompt.contains("fail") {
            emit(TextDelta {
                text: "Half".into(),
            })?;
            return Err(InferenceError::new(
                ErrorCode::GenerationFailed,
                "Scripted failure.",
            ));
        } else if request.prompt == "Truncate this answer" {
            emit(TextDelta {
                text: "Prefix".into(),
            })?;
            FinishReason::TokenLimit
        } else {
            for piece in ["Hello", " there."] {
                emit(TextDelta { text: piece.into() })?;
            }
            FinishReason::EndOfGeneration
        };
        Ok(GenerationStats {
            reason,
            prompt_tokens: 3,
            prompt_tokens_per_second: None,
            generated_tokens: 2,
            time_to_first_token_ms: Some(1),
            elapsed_ms: 1,
            tokens_per_second: 1.0,
            excluded_turns: 0,
            output_budget: None,
            assistant_replay: (reason == FinishReason::TokenLimit)
                .then(|| ResumeState::encode(&json!({"generated": [17, 18]})).unwrap()),
            context_usage: request.context.as_ref().map(|c| c.usage()),
        })
    }
}

#[derive(Default)]
pub(crate) struct Recorder(Mutex<Vec<Value>>);

impl ChatEventSink for Recorder {
    fn send(&self, event: ChatEvent) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
    }
}

impl Recorder {
    fn kinds(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|event| event["kind"].as_str().unwrap().to_owned())
            .collect()
    }
}

pub(crate) struct Harness {
    pub engine: Engine,
    pub script: Script,
    pub model: ModelId,
}

pub(crate) fn harness() -> Harness {
    harness_with_platform(Arc::new(Threads))
}

fn harness_with_platform(platform: Arc<dyn Platform>) -> Harness {
    let script = Script::default();
    let provider = script.clone();
    let worker = InferenceWorker::spawn(move || Scripted(provider)).unwrap();
    let engine = Engine::new(
        platform,
        Arc::new(worker),
        SqliteStore::open(Path::new(":memory:")).unwrap(),
        ":memory:".into(),
    )
    .unwrap();
    let model = engine
        .store()
        .lock()
        .unwrap()
        .remember_model(
            Path::new("/synthetic/scripted.gguf"),
            "scripted.gguf",
            ModelSettings::recommended(2048),
        )
        .unwrap()
        .id;
    Harness {
        engine,
        script,
        model,
    }
}

impl Harness {
    /// Load with explicit settings, so no model file is inspected.
    fn load(&self) {
        let settings = ModelSettings {
            temperature: 0.0,
            ..ModelSettings::recommended(1024)
        };
        futures_executor::block_on(self.engine.load_model(self.model, settings)).unwrap();
    }

    fn conversation(&self) -> ConversationId {
        self.engine.store().lock().unwrap().create().unwrap().id
    }

    fn document(&self, name: &str, body: &str) -> i64 {
        let mut chunks = Vec::new();
        append_chunks(&mut chunks, body, None, || Ok(())).unwrap();
        let fingerprint = [name.len() as u8; 32];
        let outcome = self
            .engine
            .store()
            .lock()
            .unwrap()
            .save_knowledge(
                &DocumentImport {
                    name,
                    format: DocumentFormat::Txt,
                    fingerprint: &fingerprint,
                    file_bytes: body.len(),
                    pdf_coverage: None,
                    chunks: &chunks,
                },
                None,
                |_| Ok(()),
            )
            .unwrap();
        let ImportOutcome::Imported { document_id } = outcome else {
            panic!("expected a new document");
        };
        document_id
    }

    fn send(
        &self,
        id: ConversationId,
        content: &str,
        options: Value,
    ) -> (Result<agent_core::chat::Message>, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let events: ChatEvents = recorder.clone();
        let options: ChatOptions = serde_json::from_value(options).unwrap();
        let result = futures_executor::block_on(self.engine.send_message(
            id,
            content.into(),
            options,
            events,
        ));
        (result, recorder)
    }
}

fn knowledge(document: i64, grounding: &str) -> Value {
    json!({"knowledge": {"mode": "keyword", "documentIds": [document], "grounding": grounding}})
}

#[test]
fn chat_requires_a_loaded_model_and_the_operation_gate() {
    let h = harness();
    let id = h.conversation();
    let (result, _) = h.send(id, "Hi", json!({}));
    assert_eq!(result.unwrap_err().code, ErrorCode::NoModel);
    h.load();
    let lease = h.engine.reserve().unwrap();
    let (result, events) = h.send(id, "Hi", json!({}));
    assert_eq!(result.unwrap_err().code, ErrorCode::Busy);
    assert!(events.kinds().is_empty());
    drop(lease);
    assert!(h.engine.get_conversation(id).unwrap().messages.is_empty());
}

#[test]
fn ordinary_chat_streams_events_and_persists_the_answer() {
    let h = harness();
    h.load();
    let id = h.conversation();
    let (result, events) = h.send(id, "Say hello", json!({}));
    let message = result.unwrap();
    assert_eq!(message.state, GenerationState::Complete);
    assert_eq!(message.content, "Hello there.");
    assert_eq!(events.kinds(), ["started", "delta", "delta"]);
    let recorded = events.0.lock().unwrap().clone();
    assert_eq!(recorded[0]["user"]["content"], "Say hello");
    assert_eq!(recorded[0]["assistant"]["id"], message.id);
    assert_eq!(
        recorded[1],
        json!({"kind":"delta","id":message.id,"text":"Hello"})
    );
    let seen = h.script.seen();
    assert_eq!(seen[0].prompt, "Say hello");
    assert_eq!(seen[0].evidence, None);
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[1].content, "Hello there.");
    assert_eq!(saved[1].state, GenerationState::Complete);

    // A follow-up sees the earlier turn; regeneration replaces the attempt.
    let (_, _) = h.send(id, "Again", json!({}));
    assert_eq!(h.script.seen()[1].history_turns, 1);
    let regenerated = futures_executor::block_on(h.engine.regenerate(
        id,
        ChatOptions::default(),
        Arc::new(Recorder::default()),
    ))
    .unwrap();
    assert_eq!(regenerated.content, "Hello there.");
    assert_eq!(h.script.seen()[2].prompt, "Again");
}

#[test]
fn grounded_chat_supplies_evidence_without_history_and_records_citations() {
    let h = harness();
    h.load();
    let document = h.document("backend.md", "The backend listens on port 8080.\n");
    let id = h.conversation();
    h.send(id, "Earlier ordinary turn", json!({})).0.unwrap();
    let (result, events) = h.send(
        id,
        "Which port does the backend listen on?",
        knowledge(document, "documents"),
    );
    let message = result.unwrap();
    assert_eq!(events.kinds(), ["knowledge", "started", "delta", "delta"]);
    let recorded = events.0.lock().unwrap().clone();
    assert_eq!(recorded[0]["coverage"]["intent"], "question");
    let seen = h.script.seen().pop().unwrap();
    assert_eq!(seen.history_turns, 0, "evidence answers see no history");
    let evidence = seen.evidence.unwrap();
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].contains("8080"));
    assert!(message.context.is_some());
    let supplied = h
        .engine
        .store()
        .lock()
        .unwrap()
        .chat_supplied_evidence(message.id)
        .unwrap();
    assert_eq!(supplied.len(), 1);
}

#[test]
fn documents_only_without_evidence_answers_deterministically() {
    let h = harness();
    h.load();
    let document = h.document("backend.md", "The backend listens on port 8080.\n");
    h.engine
        .store()
        .lock()
        .unwrap()
        .create_memory("The walrus fact is private.")
        .unwrap();
    h.engine
        .store()
        .lock()
        .unwrap()
        .set_memory_enabled(true)
        .unwrap();
    let id = h.conversation();
    let (result, events) = h.send(id, "search for walrus", knowledge(document, "documents"));
    let message = result.unwrap();
    assert!(h.script.seen().is_empty(), "no generation without evidence");
    assert_eq!(
        message.content,
        "The text \u{201c}walrus\u{201d} wasn't found in the selected documents."
    );
    assert_eq!(message.state, GenerationState::Complete);
    assert_eq!(events.kinds(), ["knowledge", "started", "delta"]);
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved[1].content, message.content);
}

#[test]
fn memories_supplement_ordinary_chat_but_never_documents_only_answers() {
    let h = harness();
    h.load();
    // Retrieval is off until the user enables it.
    assert!(!h.engine.store().lock().unwrap().memory_enabled().unwrap());
    h.engine
        .store()
        .lock()
        .unwrap()
        .set_memory_enabled(true)
        .unwrap();
    h.engine
        .store()
        .lock()
        .unwrap()
        .create_memory("My backend port preference is 9090.")
        .unwrap();
    let document = h.document("backend.md", "The backend listens on port 8080.\n");
    let id = h.conversation();
    h.send(id, "What backend port do I prefer?", json!({}))
        .0
        .unwrap();
    let ordinary = h.script.seen().pop().unwrap();
    assert_eq!(ordinary.memories, ["My backend port preference is 9090."]);
    assert_eq!(ordinary.evidence, None);

    h.send(
        id,
        "Which port does the backend listen on?",
        knowledge(document, "documents"),
    )
    .0
    .unwrap();
    let strict = h.script.seen().pop().unwrap();
    assert!(strict.memories.is_empty());
    assert!(strict.evidence.is_some());

    h.engine
        .store()
        .lock()
        .unwrap()
        .set_memory_enabled(false)
        .unwrap();
    h.send(id, "What backend port do I prefer?", json!({}))
        .0
        .unwrap();
    assert!(h.script.seen().pop().unwrap().memories.is_empty());
}

#[test]
fn automatic_grounding_answers_unrelated_requests_as_ordinary_chat() {
    let h = harness();
    h.load();
    let document = h.document("backend.md", "The backend listens on port 8080.\n");
    let id = h.conversation();
    let (result, events) = h.send(
        id,
        "Write a four-line poem about rain",
        knowledge(document, "auto"),
    );
    let message = result.unwrap();
    assert_eq!(message.state, GenerationState::Complete);
    assert!(message.context.is_none());
    assert_eq!(events.0.lock().unwrap()[0]["coverage"]["intent"], "general");
    assert_eq!(h.script.seen()[0].evidence, None);
}

#[test]
fn stop_cancels_streaming_and_keeps_the_partial_response() {
    let h = Arc::new(harness());
    h.load();
    let id = h.conversation();
    let runner = Arc::clone(&h);
    let turn = thread::spawn(move || runner.send(id, "Please wait", json!({})).0);
    let started = Instant::now();
    while h.engine.inference_status().unwrap().phase != Phase::Generating {
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(5));
    }
    assert!(h.engine.stop_chat().unwrap());
    let message = turn.join().unwrap().unwrap();
    assert_eq!(message.state, GenerationState::Cancelled);
    assert_eq!(message.content, "Partial");
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved[1].state, GenerationState::Cancelled);
    assert_eq!(saved[1].content, "Partial");
    let receipts = h
        .engine
        .store()
        .lock()
        .unwrap()
        .intelligence_receipts(message.id)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["state"], "cancelled");
    assert_eq!(receipts[0]["external"], json!([]));
    assert_eq!(receipts[0]["used"][0]["kind"], "prompt");
    // The gate and cancellation slot are released for the next turn.
    assert!(!h.engine.stop_chat().unwrap());
    h.send(id, "Hello", json!({})).0.unwrap();
}

#[test]
fn generation_failure_is_persisted_as_failed_with_partial_text() {
    let h = harness();
    h.load();
    let id = h.conversation();
    let (result, _) = h.send(id, "Please fail", json!({}));
    assert_eq!(result.unwrap_err().code, ErrorCode::GenerationFailed);
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved[1].state, GenerationState::Failed);
    assert_eq!(saved[1].content, "Half");
    assert!(h.engine.reserve().is_ok(), "the gate is released");
}

#[test]
fn model_lifecycle_tracks_the_active_model() {
    let h = harness();
    assert_eq!(h.engine.active_model(), None);
    h.load();
    assert_eq!(h.engine.active_model(), Some(h.model));
    let bootstrap = serde_json::to_value(h.engine.bootstrap().unwrap()).unwrap();
    assert_eq!(bootstrap["activeModelId"], h.model);
    assert_eq!(bootstrap["databasePath"], ":memory:");
    assert_eq!(bootstrap["inference"]["phase"], "ready");

    // Context and threads cannot change while loaded; sampling can.
    let saved = h.engine.list_models().unwrap()[0].settings;
    assert!(!h.engine.list_models().unwrap()[0].automatic_settings);
    let mut changed = saved;
    changed.model.context_length = 2048;
    assert_eq!(
        h.engine
            .save_model_settings(h.model, changed)
            .unwrap_err()
            .code,
        ErrorCode::InvalidConfig
    );
    let mut sampling = saved;
    sampling.temperature = 0.5;
    h.engine.save_model_settings(h.model, sampling).unwrap();

    let recommended = serde_json::to_value(h.engine.recommended_chat_settings().unwrap()).unwrap();
    assert_eq!(recommended["trainedContext"], 4096);
    assert_eq!(recommended["settings"]["contextLength"], 4096);

    futures_executor::block_on(h.engine.unload_model()).unwrap();
    assert_eq!(h.engine.active_model(), None);
    assert!(
        h.engine
            .store()
            .lock()
            .unwrap()
            .chat_model_unloaded()
            .unwrap()
    );
    assert_eq!(
        h.engine
            .continuation_remaining(h.conversation(), 1)
            .unwrap_err()
            .code,
        ErrorCode::NoModel
    );
}

#[test]
fn recommended_settings_follow_available_memory() {
    use crate::models::recommended_for_model;
    let gib = 1_u64 << 30;
    let context = |available| {
        recommended_for_model(32_768, gib, available)
            .model
            .context_length
    };
    assert_eq!(context(Some(3 * gib)), 8192);
    assert_eq!(context(Some(2 * gib)), 4096);
    assert_eq!(context(Some(gib)), 2048);
    assert_eq!(context(None), 4096);
    assert_eq!(
        recommended_for_model(1024, gib, Some(8 * gib))
            .model
            .context_length,
        1024
    );
}

struct Fixture(&'static str);

struct FixtureContents(&'static str);

impl DocumentSource for Fixture {
    type Contents = FixtureContents;

    fn read(self, check: ImportCheck<'_>) -> AgentResult<FixtureContents> {
        check()?;
        Ok(FixtureContents(self.0))
    }
}

impl DocumentContents for FixtureContents {
    fn display_name(&self) -> &str {
        "fixture.txt"
    }

    fn format(&self) -> DocumentFormat {
        DocumentFormat::Txt
    }

    fn bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    fn extract(&self, check: ImportCheck<'_>) -> AgentResult<ExtractedDocument> {
        let mut chunks: Vec<DocumentChunk> = Vec::new();
        append_chunks(&mut chunks, self.0, None, check)?;
        Ok(ExtractedDocument {
            chunks,
            pdf_coverage: None,
        })
    }
}

#[test]
fn knowledge_import_commits_through_the_engine_and_reports_status() {
    let h = harness();
    let job = h.engine.start_import().unwrap();
    assert_eq!(
        serde_json::to_value(h.engine.import_status(None).unwrap()).unwrap(),
        json!({"jobId": 1, "state": {"phase": "selecting"}})
    );
    assert_eq!(
        h.engine.start_import().err().unwrap().code,
        AgentErrorCode::Busy
    );
    job.confirming().unwrap();
    assert!(!h.engine.cancel_import(job.id()).unwrap());
    let result = futures_executor::block_on(h.engine.import_document(
        &job,
        Fixture("Orchard gate."),
        None,
    ));
    let ImportOutcome::Imported { document_id } = result.clone().unwrap() else {
        panic!("expected a new document");
    };
    job.finish(result).unwrap();
    let status = serde_json::to_value(h.engine.import_status(Some(1)).unwrap()).unwrap();
    assert_eq!(status["state"]["phase"], "completed");
    let library = futures_executor::block_on(h.engine.knowledge_library()).unwrap();
    assert_eq!(library.len(), 1);
    let document = futures_executor::block_on(h.engine.knowledge_document(document_id)).unwrap();
    assert_eq!(document.name, "fixture.txt");

    // A second job can start after the first finished; a duplicate is reported.
    let next = h.engine.start_import().unwrap();
    assert_eq!(next.id(), 2);
    let duplicate = futures_executor::block_on(h.engine.import_document(
        &next,
        Fixture("Orchard gate."),
        None,
    ))
    .unwrap();
    assert!(matches!(duplicate, ImportOutcome::Duplicate { .. }));
    futures_executor::block_on(h.engine.delete_knowledge_document(document_id)).unwrap();
    assert!(
        futures_executor::block_on(h.engine.knowledge_library())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn keyword_retrieval_runs_without_an_embedding_model() {
    let h = harness();
    let document = h.document("backend.md", "The backend listens on port 8080.\n");
    let request: storage::knowledge::KnowledgeSearch = serde_json::from_value(
        json!({"query": "backend port", "documentIds": [document], "limit": 3}),
    )
    .unwrap();
    let results = futures_executor::block_on(
        h.engine
            .retrieve(request, storage::semantic::RetrievalMode::Keyword),
    )
    .unwrap();
    assert_eq!(results.hits.len(), 1);
    let request: storage::knowledge::KnowledgeSearch =
        serde_json::from_value(json!({"query": "backend port", "limit": 3})).unwrap();
    let error = futures_executor::block_on(
        h.engine
            .retrieve(request, storage::semantic::RetrievalMode::Semantic),
    )
    .unwrap_err();
    assert_eq!(error.code, AgentErrorCode::EmbeddingUnavailable);
    assert!(h.engine.reserve().is_ok(), "the gate is released");
    let status = serde_json::to_value(h.engine.embedding_status()).unwrap();
    assert_eq!(status["available"], false);
    assert_eq!(status["restore"], "not_remembered");
}

/// Wait for a condition that other threads establish, failing after 5 s.
fn eventually(mut condition: impl FnMut() -> bool, what: &str) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timed out: {what}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Poll a turn once, keep polling until `ready`, then drop the caller's future.
fn drop_turn_when<F: std::future::Future>(turn: F, mut ready: impl FnMut() -> bool) {
    use std::task::{Context, Poll, Waker};
    let mut turn = Box::pin(turn);
    let mut context = Context::from_waker(Waker::noop());
    let started = Instant::now();
    loop {
        assert!(
            matches!(turn.as_mut().poll(&mut context), Poll::Pending),
            "the turn finished before it was dropped"
        );
        if ready() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "turn never got ready"
        );
        thread::sleep(Duration::from_millis(5));
    }
    drop(turn);
}

#[test]
fn a_dropped_chat_future_can_be_stopped_and_still_persists_its_terminal_state() {
    let h = harness();
    h.load();
    let id = h.conversation();
    let events = Arc::new(Recorder::default());
    let turn = h.engine.send_message(
        id,
        "Please wait".into(),
        ChatOptions::default(),
        events.clone(),
    );
    drop_turn_when(turn, || events.kinds().contains(&"started".to_owned()));
    // Stop still reaches the turn whose caller went away.
    assert!(h.engine.stop_chat().unwrap());
    eventually(
        || h.engine.reserve().is_ok(),
        "the operation gate is released",
    );
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved[1].state, GenerationState::Cancelled);
    assert_eq!(saved[1].content, "Partial");
    let (next, _) = h.send(id, "Hello", json!({}));
    assert_eq!(next.unwrap().state, GenerationState::Complete);
}

#[test]
fn a_chat_future_dropped_right_after_acceptance_completes_in_the_background() {
    let platform = Arc::new(QueuedChat::default());
    let h = harness_with_platform(platform.clone());
    h.load();
    let id = h.conversation();
    let turn = h.engine.send_message(
        id,
        "Say hello".into(),
        ChatOptions::default(),
        Arc::new(Recorder::default()),
    );
    drop_turn_when(turn, || true);
    assert_eq!(h.engine.reserve().err().unwrap().code, ErrorCode::Busy);
    platform.run();
    assert!(h.engine.reserve().is_ok());
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[1].state, GenerationState::Complete);
    assert_eq!(saved[1].content, "Hello there.");
}

#[test]
fn a_dropped_failing_chat_future_records_the_failure() {
    let platform = Arc::new(QueuedChat::default());
    let h = harness_with_platform(platform.clone());
    h.load();
    let id = h.conversation();
    let turn = h.engine.send_message(
        id,
        "Please fail".into(),
        ChatOptions::default(),
        Arc::new(Recorder::default()),
    );
    drop_turn_when(turn, || true);
    assert_eq!(h.engine.reserve().err().unwrap().code, ErrorCode::Busy);
    platform.run();
    assert!(h.engine.reserve().is_ok());
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved[1].state, GenerationState::Failed);
    assert_eq!(saved[1].content, "Half");
}

#[test]
fn queued_stop_preserves_the_truncated_attempt_and_allowance() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };

    let platform = Arc::new(QueuedChat::default());
    let h = harness_with_platform(platform.clone());
    h.load();
    let id = h.conversation();
    let mut context = Context::from_waker(Waker::noop());
    let mut initial = Box::pin(h.engine.send_message(
        id,
        "Truncate this answer".into(),
        ChatOptions::default(),
        Arc::new(Recorder::default()),
    ));
    assert!(matches!(initial.as_mut().poll(&mut context), Poll::Pending));
    platform.run();
    let truncated = futures_executor::block_on(initial).unwrap();
    assert_eq!(truncated.state, GenerationState::Truncated);
    assert_eq!(
        h.engine.continuation_remaining(id, truncated.id).unwrap(),
        3
    );

    let events = Arc::new(Recorder::default());
    let mut continued = Box::pin(
        h.engine
            .continue_chat(id, truncated.id, Some(8), events.clone()),
    );
    assert!(matches!(
        continued.as_mut().poll(&mut context),
        Poll::Pending
    ));
    assert_eq!(h.engine.reserve().err().unwrap().code, ErrorCode::Busy);
    assert!(
        h.engine.stop_chat().unwrap(),
        "Stop must reach an accepted queued continuation"
    );
    platform.run();
    assert_eq!(
        futures_executor::block_on(continued).unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert!(events.kinds().is_empty());
    let messages = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].id, truncated.id);
    assert_eq!(messages[1].state, GenerationState::Truncated);
    assert_eq!(messages[1].content, "Prefix");
    assert_eq!(
        h.engine.continuation_remaining(id, truncated.id).unwrap(),
        3
    );
    assert!(h.engine.reserve().is_ok());
    assert!(!h.engine.stop_chat().unwrap());
}

struct PanickingSink {
    kind: &'static str,
    invoked: std::sync::atomic::AtomicBool,
}

impl ChatEventSink for PanickingSink {
    fn send(&self, event: ChatEvent) {
        let kind = match event {
            ChatEvent::Started { .. } => "started",
            ChatEvent::Delta { .. } => "delta",
            ChatEvent::Knowledge { .. } => "knowledge",
        };
        if kind == self.kind {
            self.invoked
                .store(true, std::sync::atomic::Ordering::Relaxed);
            panic!("intentional {kind} sink panic");
        }
    }
}

fn assert_chat_survives_sink_panic(kind: &'static str, deterministic: bool) {
    let h = harness();
    h.load();
    let id = h.conversation();
    let (prompt, options, answer) = if deterministic {
        let document = h.document("backend.md", "The backend listens on port 8080.");
        (
            "search for walrus",
            serde_json::from_value(knowledge(document, "documents")).unwrap(),
            "The text \u{201c}walrus\u{201d} wasn't found in the selected documents.",
        )
    } else {
        ("Say hello", ChatOptions::default(), "Hello there.")
    };
    let sink = Arc::new(PanickingSink {
        kind,
        invoked: false.into(),
    });
    let result =
        futures_executor::block_on(
            h.engine
                .send_message(id, prompt.into(), options, sink.clone()),
        );
    assert!(sink.invoked.load(std::sync::atomic::Ordering::Relaxed));
    let message = result.unwrap();
    assert_eq!(message.state, GenerationState::Complete);
    assert_eq!(message.content, answer);
    let saved = h.engine.get_conversation(id).unwrap().messages;
    assert_eq!(saved[1].state, GenerationState::Complete);
    assert_eq!(saved[1].content, answer);
    assert_eq!(
        h.send(id, "Continue normally", json!({})).0.unwrap().state,
        GenerationState::Complete
    );
}

#[test]
fn sink_panic_on_started_does_not_interrupt_chat() {
    assert_chat_survives_sink_panic("started", false);
}

#[test]
fn sink_panic_on_delta_does_not_stop_inference() {
    assert_chat_survives_sink_panic("delta", false);
}

#[test]
fn sink_panic_on_no_evidence_does_not_abandon_the_reply() {
    assert_chat_survives_sink_panic("knowledge", true);
}

/// A sink that reads the conversation synchronously for every event.
struct Reader {
    store: Arc<Mutex<SqliteStore>>,
    id: ConversationId,
    seen: Mutex<Vec<(String, usize)>>,
}

impl ChatEventSink for Reader {
    fn send(&self, event: ChatEvent) {
        let kind = serde_json::to_value(&event).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_owned();
        let messages = self.store.lock().unwrap().get(self.id).unwrap().messages;
        self.seen.lock().unwrap().push((kind, messages.len()));
    }
}

#[test]
fn event_sinks_may_read_the_conversation_without_deadlocking() {
    let h = Arc::new(harness());
    h.load();
    let document = h.document("backend.md", "The backend listens on port 8080.\n");
    let id = h.conversation();
    for (content, options) in [
        ("search for walrus", knowledge(document, "documents")),
        (
            "Which port does the backend listen on?",
            knowledge(document, "documents"),
        ),
        ("Say hello", json!({})),
    ] {
        let reader = Arc::new(Reader {
            store: Arc::clone(h.engine.store()),
            id,
            seen: Mutex::default(),
        });
        let (done, result) = std::sync::mpsc::channel();
        let (runner, sink) = (Arc::clone(&h), Arc::clone(&reader));
        let options: ChatOptions = serde_json::from_value(options).unwrap();
        thread::spawn(move || {
            let answer = futures_executor::block_on(runner.engine.send_message(
                id,
                content.into(),
                options,
                sink,
            ));
            let _ = done.send(answer);
        });
        let answer = result
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("deadlocked delivering events for {content:?}"))
            .unwrap();
        assert_eq!(answer.state, GenerationState::Complete);
        let kinds: Vec<_> = reader
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        let expected: &[&str] = if content == "Say hello" {
            &["started", "delta", "delta"]
        } else if content.starts_with("search") {
            &["knowledge", "started", "delta"]
        } else {
            &["knowledge", "started", "delta", "delta"]
        };
        assert_eq!(kinds, expected, "{content:?}");
        // The pending turn is visible to the sink from the first event.
        assert!(reader.seen.lock().unwrap().iter().all(|(_, n)| *n >= 2));
    }
}

#[test]
fn leases_from_another_engine_are_rejected_before_any_work() {
    let a = harness();
    let b = harness();
    let missing = std::path::PathBuf::from("/nonexistent/03a/model.gguf");

    let foreign = a.engine.reserve().unwrap();
    let error =
        futures_executor::block_on(b.engine.register_model(&foreign, missing.clone())).unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy, "{error:?}");
    assert_eq!(b.engine.list_models().unwrap().len(), 1);
    drop(foreign);
    // The engine's own lease reaches file inspection.
    let own = b.engine.reserve().unwrap();
    let error =
        futures_executor::block_on(b.engine.register_model(&own, missing.clone())).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidModel, "{error:?}");
    drop(own);

    if cfg!(target_os = "linux") {
        let foreign = a.engine.begin_embedding_selection().unwrap();
        let result = futures_executor::block_on(
            b.engine
                .select_embedding_model(foreign, missing.with_extension("bge.gguf")),
        );
        let error = result.err().expect("a foreign lease must be rejected");
        assert_eq!(error.code, AgentErrorCode::Busy, "{error:?}");
        assert_eq!(
            b.engine
                .store()
                .lock()
                .unwrap()
                .embedding_model_choice()
                .unwrap(),
            None
        );
        let status = serde_json::to_value(b.engine.embedding_status()).unwrap();
        assert_eq!(status["restore"], "not_remembered");
        // The foreign lease was consumed and released; both gates are free.
        assert!(a.engine.reserve().is_ok() && b.engine.reserve().is_ok());
    }
}

#[test]
fn adapter_discovery_and_receipt_follow_fitted_knowledge_and_memory() {
    use agent_core::{
        capability::{GENERATE, Locality},
        classification::{LIBRARY, MEMORY, MODEL_OUTPUT, SYSTEM_TEXT, TEXT_EXCERPT, USER_INPUT},
    };

    let h = harness();
    assert!(h.engine.inference_adapter().discover().unwrap().is_empty());
    h.load();
    let offers = h.engine.inference_adapter().discover().unwrap();
    assert!(offers.iter().any(|offer| offer.capability == GENERATE
        && offer.streaming
        && offer.locality == Locality::OnDevice));

    let document = h.document(
        "networking.txt",
        "The backend port is 8080 for the synthetic server.",
    );
    let memory = {
        let mut store = h.engine.store().lock().unwrap();
        store.set_memory_enabled(true).unwrap();
        store
            .create_memory("The backend port preference is synthetic-private-memory.")
            .unwrap()
    };
    let conversation = h.conversation();
    let earlier = h
        .send(conversation, "Write a poem about rain", json!({}))
        .0
        .unwrap();
    let message = h
        .send(
            conversation,
            "Which backend port is documented?",
            knowledge(document, "auto"),
        )
        .0
        .unwrap();

    let seen = h.script.seen().pop().unwrap();
    assert_eq!(seen.history_turns, 0);
    assert_eq!(seen.memories.len(), 1);
    assert_eq!(seen.evidence.unwrap().len(), 1);
    let db = h.engine.store().lock().unwrap();
    let saved = db.get(conversation).unwrap();
    assert_eq!(saved.messages.last().unwrap().labels, message.labels);
    for class in [
        LIBRARY,
        MEMORY,
        TEXT_EXCERPT,
        USER_INPUT,
        MODEL_OUTPUT,
        SYSTEM_TEXT,
    ] {
        assert!(message.labels.contains(class), "missing {}", class.id);
    }
    let records = db.intelligence_receipts(message.id).unwrap();
    let [receipt] = records.as_slice() else {
        panic!("expected one terminal receipt")
    };
    assert_eq!(receipt["provider"]["id"], "local.llama-cpp/1");
    assert_eq!(receipt["provider"]["locality"], "on_device");
    assert_eq!(receipt["local_authorization"], "builtin.chat/1");
    assert_eq!(receipt["application_retention"], "saved_locally");
    assert_eq!(receipt["external"], json!([]));
    let used = receipt["used"].as_array().unwrap();
    assert!(
        used.iter()
            .any(|source| source["kind"] == "knowledge" && source["record"] == document)
    );
    assert!(
        used.iter()
            .any(|source| source["kind"] == "memory" && source["record"] == memory.id)
    );
    assert!(
        receipt["withheld"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["kind"] == "history" && source["record"] == earlier.id)
    );
    let metadata = serde_json::to_string(receipt).unwrap();
    assert!(!metadata.contains("synthetic-private-memory"));
    assert!(!metadata.contains("Which backend port"));
}

fn glance(
    h: &Harness,
    input: Value,
    cancellation: Cancellation,
) -> Result<crate::chat::glance::Answer> {
    futures_executor::block_on(h.engine.run_glance(
        serde_json::from_value(input).unwrap(),
        crate::chat::glance::Origin::EditedClipboard,
        None,
        cancellation,
        Arc::new(Recorder::default()),
    ))
}

#[test]
fn local_glance_uses_selected_context_without_persisting_or_history() {
    let h = harness();
    h.load();
    {
        let mut db = h.engine.store().lock().unwrap();
        db.set_memory_enabled(true).unwrap();
        db.create_memory("Synthetic orchard memory: rainwater tanks hold 400 litres.")
            .unwrap();
    }
    let document = h.document(
        "orchard.md",
        "The orchard rainwater tanks refill in spring.\n",
    );
    let id = h.conversation();
    h.send(id, "SYNTHETIC_CHAT_HISTORY", json!({})).0.unwrap();
    let before = h.engine.store().lock().unwrap().dump_text_for_tests();

    let answer = glance(
        &h,
        json!({"text": "Synthetic orchard rainwater tanks note.", "recipe": "summarize",
            "share": false, "memory": true, "documents": [document]}),
        Cancellation::default(),
    )
    .unwrap();
    assert_eq!(answer.text, "Hello there.");
    assert!(answer.error.is_none());
    let seen = h.script.seen().pop().unwrap();
    assert!(seen.prompt.contains("quoted_context"));
    assert!(
        seen.prompt
            .contains("Synthetic orchard rainwater tanks note.")
    );
    assert_eq!(seen.history_turns, 0);
    assert!(
        seen.evidence
            .is_some_and(|e| e[0].contains("refill in spring"))
    );
    assert_eq!(seen.memories.len(), 1);
    let receipt = serde_json::to_value(answer.receipt.as_ref().unwrap()).unwrap();
    assert_eq!(receipt["state"], "completed");
    assert_eq!(receipt["application_retention"], "session");
    assert_eq!(receipt["external"], json!([]));
    let kinds: Vec<&str> = receipt["used"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    for kind in ["clipboard", "knowledge", "memory"] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }
    // Neither the source, its answer nor a receipt row reached the database.
    let after = h.engine.store().lock().unwrap().dump_text_for_tests();
    assert_eq!(before, after);
    assert!(h.engine.get_conversation(id).unwrap().messages.len() == 2);

    // Remember is the only write, and it records the origin.
    h.engine.remember_glance(&answer).unwrap();
    let memories = h
        .engine
        .store()
        .lock()
        .unwrap()
        .list_memories("", None, 10)
        .unwrap();
    assert_eq!(memories[0].content, "Hello there.");
    let provenance = memories[0].provenance.as_ref().unwrap();
    assert_eq!(provenance["source"], "contextual_panel");
    assert!(
        memories[0]
            .labels()
            .contains(agent_core::classification::CLIPBOARD_TEXT)
    );
}

#[test]
fn local_glance_stops_holds_the_chat_gate_and_refuses_to_remember_partial_output() {
    let h = Arc::new(harness());
    h.load();
    let cancellation = Cancellation::default();
    let runner = Arc::clone(&h);
    let token = cancellation.clone();
    let turn = thread::spawn(move || {
        glance(
            &runner,
            json!({"text": "Please wait on this synthetic text.", "recipe": "explain",
                "share": false, "memory": false, "documents": []}),
            token,
        )
    });
    let started = Instant::now();
    while h.engine.inference_status().unwrap().phase != Phase::Generating {
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(5));
    }
    // The panel shares the Chat worker: Chat waits rather than interleaving.
    let id = h.conversation();
    assert_eq!(
        h.send(id, "Hello", json!({})).0.unwrap_err().code,
        ErrorCode::Busy
    );
    cancellation.cancel();
    let answer = turn.join().unwrap().unwrap();
    assert_eq!(answer.text, "Partial");
    assert!(answer.error.is_some());
    let receipt = answer.receipt.as_ref().unwrap();
    assert_eq!(serde_json::to_value(receipt).unwrap()["state"], "cancelled");
    assert_eq!(
        h.engine.remember_glance(&answer).unwrap_err().code,
        ErrorCode::InvalidConfig
    );
    // The gate is released for the next request.
    h.send(id, "Hello", json!({})).0.unwrap();
    assert!(h.engine.get_conversation(id).unwrap().messages.len() == 2);
}
