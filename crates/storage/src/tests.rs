use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

use agent_core::{
    chat::Appearance,
    inference::{Cancellation, FinishReason},
};

use super::*;

struct TestDatabase {
    directory: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "agent-chat-test-{}-{}-{}",
            std::process::id(),
            unix_ms().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        Self { directory }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("chat.sqlite3")
    }

    fn open_encrypted(&self) -> SqliteStore {
        use crate::profile::{DatabaseKey, Profile};
        Profile::claim(&self.path())
            .unwrap()
            .open(&DatabaseKey::from_bytes(&[83; 32]).unwrap())
            .unwrap()
    }

    fn open(&self) -> SqliteStore {
        SqliteStore::open_test_fixture(&self.path()).unwrap()
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn in_memory() -> SqliteStore {
    SqliteStore::open_test_fixture(Path::new(":memory:")).unwrap()
}

fn finish_complete(store: &mut SqliteStore, id: MessageId, response: &str) {
    store
        .finish(id, response, GenerationState::Complete, None)
        .unwrap();
}

#[test]
fn initializes_schema_and_preserves_conversations_across_reopen() {
    let database = TestDatabase::new();
    let mut store = database.open();
    let version: i64 = store
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let defaults = store.preferences().unwrap().defaults;
    assert_eq!(defaults.model.context_length, 8192);
    assert_eq!(defaults.max_tokens, 4096);
    assert_eq!(defaults.model.threads, 4);
    assert!(!defaults.enable_thinking);
    let initial = store.create().unwrap();
    store.rename(initial.id, "Saved conversation").unwrap();
    drop(store);

    let reopened = database.open();
    let saved = reopened.get(initial.id).unwrap();
    assert_eq!(saved.title, "Saved conversation");
    assert_eq!(saved.created_at, initial.created_at);
    assert!(saved.updated_at >= initial.created_at);
    assert!(saved.messages.is_empty());
}

#[test]
fn refuses_a_newer_schema_and_rolls_back_a_failed_migration() {
    let database = TestDatabase::new();
    let manual = Connection::open(database.path()).unwrap();
    manual
        .execute_batch(
            "CREATE TABLE models (preserved TEXT);
         INSERT INTO models VALUES ('keep this');",
        )
        .unwrap();
    manual
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(manual);

    let error = SqliteStore::open_test_fixture(&database.path())
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Storage);
    assert!(error.message.contains("unsupported schema version"));
    let manual = Connection::open(database.path()).unwrap();
    let version: i64 = manual
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION + 1);
    manual.pragma_update(None, "user_version", 0).unwrap();
    drop(manual);

    let error = SqliteStore::open_test_fixture(&database.path())
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Storage);
    let manual = Connection::open(database.path()).unwrap();
    let version: i64 = manual
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let schema_tables: i64 = manual
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let preserved: String = manual
        .query_row("SELECT preserved FROM models", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    assert_eq!(schema_tables, 1);
    assert_eq!(preserved, "keep this");
}

#[test]
fn creates_renames_searches_and_deletes_with_literal_parameters() {
    let mut store = in_memory();
    let first = store.create().unwrap();
    let second = store.create().unwrap();
    store.rename(first.id, "  Project %_ Night  ").unwrap();
    store
        .set_system_prompt(first.id, "Use only the supplied details.")
        .unwrap();
    let pending = store
        .begin(first.id, TurnAction::Send("The label is violet.".into()))
        .unwrap();
    assert_eq!(pending.history.system, "Use only the supplied details.");
    assert_eq!(store.delete(first.id).unwrap_err().code, ErrorCode::Busy);
    finish_complete(&mut store, pending.assistant.id, "Violet.");
    assert_eq!(store.get(first.id).unwrap().title, "Project %_ Night");
    assert_eq!(store.list("project").unwrap()[0].id, first.id);
    assert_eq!(store.list("%_").unwrap()[0].id, first.id);
    assert_eq!(store.list("VIOLET").unwrap()[0].id, first.id);
    assert!(store.list("' OR 1=1 --").unwrap().is_empty());
    assert_eq!(store.list("").unwrap().len(), 2);
    assert!(store.get(second.id).unwrap().messages.is_empty());

    store.delete(first.id).unwrap();
    assert_eq!(store.get(first.id).unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(store.list("").unwrap()[0].id, second.id);
    let messages: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM messages WHERE conversation_id = ?1",
            [first.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(messages, 0);
}

#[test]
fn enforces_foreign_keys_positions_and_streaming_state_constraints() {
    let mut store = in_memory();
    let foreign_keys: i64 = store
        .connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let bad = store
        .connection
        .execute(
            "INSERT INTO messages (conversation_id, position, role, state, content, created_at)
         VALUES (999, 0, 'user', 'complete', 'orphan', 0)",
            [],
        )
        .unwrap_err();
    assert_eq!(
        bad.sqlite_extended_error_code(),
        Some(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
    );

    let conversation = store.create().unwrap();
    let pending = store
        .begin(conversation.id, TurnAction::Send("Hola".into()))
        .unwrap();
    let insert =
        "INSERT INTO messages (conversation_id, position, role, state, content, created_at)
                  VALUES (?1, ?2, ?3, ?4, '', 0)";
    assert!(
        store
            .connection
            .execute(insert, params![conversation.id, 0, "user", "complete"])
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(insert, params![conversation.id, 2, "user", "streaming"])
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(
                insert,
                params![conversation.id, 2, "assistant", "streaming"]
            )
            .is_err()
    );
    assert_eq!(pending.assistant.position, 1);
}

#[test]
fn persists_ordered_attempts_and_uses_latest_completed_regeneration() {
    let mut store = in_memory();
    let conversation = store.create().unwrap();
    assert_eq!(
        store
            .begin(conversation.id, TurnAction::Regenerate)
            .unwrap_err()
            .code,
        ErrorCode::InvalidConfig
    );
    assert_eq!(
        store
            .begin(conversation.id, TurnAction::Send("  ".into()))
            .unwrap_err()
            .code,
        ErrorCode::InvalidConfig
    );
    assert!(store.get(conversation.id).unwrap().messages.is_empty());

    let pending = store
        .begin(
            conversation.id,
            TurnAction::Send("The phrase is 你好.".into()),
        )
        .unwrap();
    assert_eq!(pending.user.as_ref().unwrap().position, 0);
    assert_eq!(pending.assistant.position, 1);
    assert_eq!(store.get(conversation.id).unwrap().messages.len(), 2);
    store.checkpoint(pending.assistant.id, "你好").unwrap();
    assert_eq!(
        store.get(conversation.id).unwrap().messages[1].content,
        "你好"
    );
    store
        .finish(
            pending.assistant.id,
            "你好!",
            GenerationState::Cancelled,
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .checkpoint(pending.assistant.id, "a late write")
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        store
            .finish(
                pending.assistant.id,
                "a late finish",
                GenerationState::Complete,
                None
            )
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );

    let attempt = store
        .begin(conversation.id, TurnAction::Regenerate)
        .unwrap();
    assert!(attempt.user.is_none());
    assert_eq!(attempt.prompt, "The phrase is 你好.");
    assert_eq!(attempt.assistant.position, 2);
    let stats = GenerationStats {
        reason: FinishReason::EndOfGeneration,
        prompt_tokens: 12,
        prompt_tokens_per_second: Some(20.0),
        generated_tokens: 7,
        time_to_first_token_ms: Some(30),
        elapsed_ms: 60,
        tokens_per_second: 2.0,
        excluded_turns: 0,
        output_budget: None,
        context_usage: None,
        assistant_replay: None,
    };
    store
        .finish(
            attempt.assistant.id,
            "The phrase is 你好.",
            GenerationState::Complete,
            Some(&stats),
        )
        .unwrap();

    let followup = store
        .begin(conversation.id, TurnAction::Send("Repeat it.".into()))
        .unwrap();
    assert_eq!(followup.history.turns.len(), 1);
    assert_eq!(followup.history.turns[0].assistant, "The phrase is 你好.");
    let messages = store.get(conversation.id).unwrap().messages;
    assert_eq!(
        messages
            .iter()
            .map(|message| message.position)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count(),
        2
    );
    assert_eq!(messages[1].state, GenerationState::Cancelled);
    assert_eq!(messages[2].stats.as_ref().unwrap().generated_tokens, 7);
}

#[test]
fn prevents_second_instance_recovery_and_recovers_checkpoints_after_close() {
    let database = TestDatabase::new();
    let mut active = database.open();
    let conversation = active.create().unwrap();
    let alternate = active.create().unwrap();
    let pending = active
        .begin(conversation.id, TurnAction::Send("Start here.".into()))
        .unwrap();
    active
        .checkpoint(pending.assistant.id, "Saved partial text")
        .unwrap();
    let second_open = SqliteStore::open_test_fixture(&database.path())
        .err()
        .unwrap();
    assert_eq!(second_open.code, ErrorCode::Busy);
    assert_eq!(
        active
            .begin(alternate.id, TurnAction::Send("Conflict".into()))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let original = active.get(conversation.id).unwrap();
    assert_eq!(original.messages[1].state, GenerationState::Streaming);
    drop(active);

    let mut recovered = database.open();
    let restored = recovered.get(conversation.id).unwrap();
    assert_eq!(restored.messages[0].content, "Start here.");
    assert_eq!(restored.messages[1].state, GenerationState::Interrupted);
    assert_eq!(restored.messages[1].content, "Saved partial text");
    let next = recovered
        .begin(conversation.id, TurnAction::Regenerate)
        .unwrap();
    finish_complete(&mut recovered, next.assistant.id, "Recovered.");
    assert_eq!(
        recovered.get(conversation.id).unwrap().messages[2].state,
        GenerationState::Complete
    );
}

#[test]
fn persists_preferences_and_model_references_without_accessing_model_files() {
    let database = TestDatabase::new();
    let mut store = database.open();
    assert_eq!(store.preferences().unwrap().defaults.model.threads, 4);
    let path = database.directory.join("disconnected.gguf");
    let mut preferences = Preferences {
        appearance: Appearance::Light,
        enter_to_send: false,
        ..Preferences::default()
    };
    preferences.defaults.model.threads = 2;
    store.save_preferences(&preferences).unwrap();
    let model = store
        .remember_model(&path, "disconnected.gguf", preferences.defaults)
        .unwrap();
    let mut alternative = preferences.defaults;
    alternative.model.threads = 3;
    store.save_model_settings(model.id, alternative).unwrap();
    let selected = store
        .remember_model(&path, "disconnected.gguf", preferences.defaults)
        .unwrap();
    assert_eq!(selected.id, model.id);
    assert_eq!(selected.settings.model.threads, 3);
    store.mark_model_used(model.id).unwrap();
    assert_eq!(store.recent_models().unwrap().len(), 1);
    drop(store);

    let reopened = database.open();
    let restored = reopened.preferences().unwrap();
    assert!(matches!(restored.appearance, Appearance::Light));
    assert!(!restored.enter_to_send);
    assert_eq!(restored.defaults.model.threads, 2);
    let remembered = reopened.model(model.id).unwrap();
    assert_eq!(remembered.path, path);
    assert_eq!(remembered.reference.settings.model.threads, 3);
    assert!(!path.exists());
    assert!(
        !serde_json::to_string(&remembered.reference)
            .unwrap()
            .contains("agent-chat-test-")
    );
}

fn synthetic_replay_stats() -> GenerationStats {
    GenerationStats {
        reason: FinishReason::TokenLimit,
        prompt_tokens: 12,
        prompt_tokens_per_second: None,
        generated_tokens: 2,
        time_to_first_token_ms: Some(12),
        elapsed_ms: 25,
        tokens_per_second: 2.0,
        excluded_turns: 0,
        output_budget: None,
        context_usage: None,
        assistant_replay: Some(agent_core::inference::ResumeState::encode(&serde_json::json!({
            "prompt_fingerprint": 42, "generated": [13, 17], "utf8_tail": [226, 130], "excluded_turns": 0,
        })).unwrap()),
    }
}

#[test]
fn continuation_reopens_same_attempt_and_retains_checkpoint_on_restart() {
    let database = TestDatabase::new();
    let mut store = database.open();
    let id = store.create().unwrap().id;
    let settings = ModelSettings {
        enable_thinking: true,
        ..ModelSettings::default()
    };
    let model = store
        .remember_model(Path::new("/synthetic/model.gguf"), "model.gguf", settings)
        .unwrap();
    let turn = store
        .begin_chat_attempt(
            id,
            TurnAction::Send("Use a Markdown fence.".into()),
            Some("model.gguf"),
            model.id,
            settings,
        )
        .unwrap();
    let assistant = turn.assistant.id;
    let stats = synthetic_replay_stats();
    assert!(
        !serde_json::to_string(&stats)
            .unwrap()
            .contains("assistantReplay")
    );
    store
        .finish(
            assistant,
            "<think>Draft</think>\n```rust\n",
            GenerationState::Truncated,
            Some(&stats),
        )
        .unwrap();
    assert_eq!(
        store
            .continuation_remaining(id, assistant, model.id)
            .unwrap(),
        3
    );
    drop(store);
    let mut store = database.open();
    let cancelled = Cancellation::default();
    cancelled.cancel();
    let result = store.begin_continuation(id, assistant, model.id, &cancelled);
    assert_eq!(result.err().unwrap().code, ErrorCode::Cancelled);
    assert_eq!(
        store
            .continuation_remaining(id, assistant, model.id)
            .unwrap(),
        3
    );
    let cancellation = Cancellation::default();
    assert!(
        store
            .begin_continuation(id, assistant, model.id + 1, &cancellation)
            .is_err()
    );
    let plan = store
        .begin_continuation(id, assistant, model.id, &cancellation)
        .unwrap();
    assert_eq!(plan.turn.assistant.id, assistant);
    assert!(plan.turn.user.is_none());
    assert!(plan.settings.enable_thinking);
    assert!(plan.turn.assistant.content.ends_with("```rust\n"));
    assert_eq!(
        plan.replay.decode::<serde_json::Value>().unwrap()["utf8_tail"],
        serde_json::json!([226, 130])
    );
    assert!(
        store
            .begin_continuation(id, assistant, model.id, &cancellation)
            .is_err()
    );
    let append = plan.turn.assistant.content + "fn main() {}";
    store.checkpoint(assistant, &append).unwrap();
    drop(store);
    let mut store = database.open();
    let conversation = store.get(id).unwrap();
    assert_eq!(conversation.messages.len(), 2);
    assert_eq!(conversation.messages[1].state, GenerationState::Interrupted);
    assert_eq!(conversation.messages[1].content, append);
    assert!(
        store
            .continuation_remaining(id, assistant, model.id)
            .is_err()
    );
    store.delete(id).unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM chat_attempt_replay", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn continuation_is_bounded_and_preserves_latest_attempt_ownership() {
    let mut store = in_memory();
    let id = store.create().unwrap().id;
    let settings = ModelSettings::default();
    let model = store
        .remember_model(Path::new("/synthetic/model.gguf"), "model.gguf", settings)
        .unwrap();
    let turn = store
        .begin_chat_attempt(
            id,
            TurnAction::Send("Write a long response.".into()),
            Some("model.gguf"),
            model.id,
            settings,
        )
        .unwrap();
    let assistant = turn.assistant.id;
    let stats = synthetic_replay_stats();
    store
        .finish(
            assistant,
            "Initial",
            GenerationState::Truncated,
            Some(&stats),
        )
        .unwrap();
    for index in 0..3 {
        assert_eq!(
            store
                .continuation_remaining(id, assistant, model.id)
                .unwrap(),
            3 - index
        );
        let plan = store
            .begin_continuation(id, assistant, model.id, &Cancellation::default())
            .unwrap();
        let content = plan.turn.assistant.content + " next";
        store
            .finish(
                assistant,
                &content,
                GenerationState::Truncated,
                Some(&stats),
            )
            .unwrap();
    }
    assert!(
        store
            .begin_continuation(id, assistant, model.id, &Cancellation::default())
            .is_err()
    );
    let saved = store.get(id).unwrap();
    assert_eq!(saved.messages.len(), 2);
    assert_eq!(saved.messages[1].content, "Initial next next next");
    let later = store
        .begin(id, TurnAction::Send("New turn.".into()))
        .unwrap();
    finish_complete(&mut store, later.assistant.id, "Done.");
    assert!(
        store
            .continuation_remaining(id, assistant, model.id)
            .is_err()
    );
}

fn bundled_settings() -> ModelSettings {
    let mut settings = ModelSettings::default();
    settings.model.context_length = 4096;
    settings.model.threads = 4;
    settings
}

#[test]
fn bundled_model_is_the_only_choice_on_a_fresh_profile() {
    let mut store = in_memory();
    let path = Path::new("/opt/app/lib/models/bundled.gguf");
    let first = store
        .register_bundled_model("bundled-v1", path, "bundled.gguf", bundled_settings())
        .unwrap();
    assert!(first.only_model);
    assert_eq!(first.reference.settings.model.context_length, 4096);
    // Repeated discovery is idempotent and keeps later user settings.
    let mut changed = first.reference.settings;
    changed.max_tokens = 99;
    store
        .save_model_settings(first.reference.id, changed)
        .unwrap();
    let again = store
        .register_bundled_model("bundled-v1", path, "bundled.gguf", bundled_settings())
        .unwrap();
    assert_eq!(again.reference.id, first.reference.id);
    assert_eq!(again.reference.settings.max_tokens, 99);
    assert_eq!(store.recent_models().unwrap().len(), 1);
    // A relocated resource updates the same row.
    let moved = store
        .register_bundled_model(
            "bundled-v1",
            Path::new("/tmp/.mount_x/usr/lib/models/bundled.gguf"),
            "bundled.gguf",
            bundled_settings(),
        )
        .unwrap();
    assert_eq!(moved.reference.id, first.reference.id);
    assert_eq!(moved.reference.settings.max_tokens, 99);
    assert_eq!(
        store.model(first.reference.id).unwrap().path,
        Path::new("/tmp/.mount_x/usr/lib/models/bundled.gguf")
    );
}

#[test]
fn bundled_model_preserves_existing_models_order_and_settings() {
    let mut store = in_memory();
    let custom = ModelSettings {
        max_tokens: 256,
        enable_thinking: true,
        ..ModelSettings::default()
    };
    let qwen = store
        .remember_model(
            Path::new("/models/Qwen3.5-4B.gguf"),
            "Qwen3.5-4B.gguf",
            custom,
        )
        .unwrap();
    store.mark_model_used(qwen.id).unwrap();
    let bundled = store
        .register_bundled_model(
            "bundled-v1",
            Path::new("/usr/lib/app/models/bundled.gguf"),
            "bundled.gguf",
            bundled_settings(),
        )
        .unwrap();
    assert!(!bundled.only_model);
    let models = store.recent_models().unwrap();
    assert_eq!(
        models.iter().map(|model| model.id).collect::<Vec<_>>(),
        [qwen.id, bundled.reference.id]
    );
    let saved = store.model(qwen.id).unwrap().reference.settings;
    assert_eq!(saved.max_tokens, 256);
    assert!(saved.enable_thinking);

    // A manual import of the same file is reused, not duplicated.
    let manual = store
        .remember_model(
            Path::new("/models/bundled-copy.gguf"),
            "bundled.gguf",
            custom,
        )
        .unwrap();
    let merged = store
        .register_bundled_model(
            "bundled-v1",
            Path::new("/models/bundled-copy.gguf"),
            "bundled.gguf",
            bundled_settings(),
        )
        .unwrap();
    assert_eq!(merged.reference.id, manual.id);
    assert_eq!(merged.reference.settings.max_tokens, 256);
    assert!(
        store
            .register_bundled_model("", Path::new("/x.gguf"), "x", bundled_settings())
            .is_err()
    );
}

#[test]
fn v4_profile_migrates_to_bundled_schema_without_changing_models() {
    let database = TestDatabase::new();
    {
        let manual = Connection::open(database.path()).unwrap();
        manual
            .execute_batch(include_str!("../migrations/001_chat.sql"))
            .unwrap();
        manual
            .execute_batch(include_str!("../migrations/002_agent.sql"))
            .unwrap();
        manual
            .execute_batch(include_str!(
                "../migrations/003_conversation_agent_turns.sql"
            ))
            .unwrap();
        manual
            .execute_batch(include_str!("../migrations/004_agent_turn_status.sql"))
            .unwrap();
        manual
            .execute_batch(include_str!("../migrations/004_chat_continuation.sql"))
            .unwrap();
        manual.pragma_update(None, "user_version", 4).unwrap();
        let settings = serde_json::to_string(&ModelSettings::default()).unwrap();
        manual
            .execute(
                "INSERT INTO models (path, file_name, settings, last_used_at) VALUES (?1, ?2, ?3, 5)",
                params!["/models/old.gguf", "old.gguf", settings],
            )
            .unwrap();
    }
    let mut store = database.open();
    let version: i64 = store
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let models = store.recent_models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].file_name, "old.gguf");
    assert_eq!(models[0].settings, ModelSettings::default());
    assert_eq!(
        store.preferences().unwrap().defaults,
        ModelSettings::default()
    );
    let bundled = store
        .register_bundled_model(
            "bundled-v1",
            Path::new("/usr/lib/app/models/bundled.gguf"),
            "bundled.gguf",
            bundled_settings(),
        )
        .unwrap();
    assert!(!bundled.only_model);
    assert_eq!(store.recent_models().unwrap()[0].file_name, "old.gguf");
    let integrity: String = store
        .connection
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[path = "knowledge_tests.rs"]
mod knowledge_tests;

#[path = "semantic_tests.rs"]
mod semantic_tests;

#[path = "chat_context_tests.rs"]
mod chat_context_tests;

#[path = "relevance_tests.rs"]
mod relevance_tests;

#[path = "model_tests.rs"]
mod model_tests;

#[test]
fn schema_eleven_context_and_memory_reload_with_conservative_labels() {
    use agent_core::classification::{
        DataClasses, HISTORY, LIBRARY, MEMORY, MODEL_OUTPUT, Sensitivity, USER_INPUT,
    };

    let database = TestDatabase::new();
    let mut store = database.open();
    let conversation = store.create().unwrap().id;
    let turn = store
        .begin_chat_with_context(
            conversation,
            TurnAction::Send("legacy question".into()),
            None,
            Default::default(),
        )
        .unwrap();
    store
        .finish(
            turn.assistant.id,
            "legacy answer",
            GenerationState::Complete,
            None,
        )
        .unwrap();
    let memory = store.create_memory("legacy personal note").unwrap();
    // Recreate the immediately preceding schema with genuine pre-03-B records.
    store
        .connection
        .execute_batch(
            "DROP TABLE model_downloads;
         ALTER TABLE personal_memories DROP COLUMN provenance; ALTER TABLE personal_memories DROP COLUMN classification;
         DROP TABLE provider_usage; DROP TABLE intelligence_settings; DROP TABLE providers;
         ALTER TABLE models DROP COLUMN artifact_key; DROP TABLE intelligence_receipts;
         ALTER TABLE messages DROP COLUMN classification;
         PRAGMA user_version=11;",
        )
        .unwrap();
    drop(store);

    let mut store = database.open();
    let saved = store.get(conversation).unwrap();
    assert_eq!(saved.messages[0].content, "legacy question");
    assert_eq!(saved.messages[1].content, "legacy answer");
    for class in [HISTORY, USER_INPUT] {
        assert!(saved.messages[0].labels.contains(class));
    }
    for class in [HISTORY, LIBRARY, MEMORY, MODEL_OUTPUT] {
        assert!(saved.messages[1].labels.contains(class));
        assert_eq!(
            DataClasses::default().sensitivity(class.id),
            Some(Sensitivity::Personal)
        );
    }
    assert_eq!(store.list_memories("", None, 20).unwrap()[0].id, memory.id);
    assert!(
        store.list_memories("", None, 20).unwrap()[0]
            .labels()
            .contains(MEMORY)
    );
    assert!(
        store
            .intelligence_receipts(turn.assistant.id)
            .unwrap()
            .is_empty()
    );
    store
        .connection
        .execute(
            "UPDATE messages SET classification=?1 WHERE id=?2",
            params!["[\"unknown.class/1\"]", turn.assistant.id],
        )
        .unwrap();
    assert!(store.get(conversation).is_err());
    store.delete(conversation).unwrap();
}

#[test]
fn schema_fourteen_memories_upgrade_without_provenance_and_panel_memories_keep_theirs() {
    use agent_core::classification::{CLIPBOARD_TEXT, MEMORY, USER_INPUT};
    use agent_core::intelligence::{Receipt, SessionState};

    let database = TestDatabase::new();
    let mut store = database.open();
    let legacy = store.create_memory("legacy personal note").unwrap();
    store
        .connection
        .execute_batch(
            "DROP TABLE model_downloads;
             ALTER TABLE personal_memories DROP COLUMN provenance;
             ALTER TABLE personal_memories DROP COLUMN classification;
             PRAGMA user_version=14;",
        )
        .unwrap();
    drop(store);

    let mut store = database.open();
    let upgraded = &store.list_memories("", None, 10).unwrap()[0];
    assert_eq!(upgraded.id, legacy.id);
    assert!(upgraded.provenance.is_none());
    assert_eq!(upgraded.labels(), Labels::from_source(MEMORY));

    let mut output = Labels::from_source(USER_INPUT);
    output.insert(CLIPBOARD_TEXT);
    let receipt = Receipt {
        version: 1,
        capability: agent_core::capability::GENERATE,
        state: SessionState::Completed,
        elapsed_ms: 1,
        provider: None,
        local_authorization: None,
        used: Vec::new(),
        withheld: Vec::new(),
        output_labels: output,
        application_retention: agent_core::capability::Retention::Session,
        external: Vec::new(),
        usage: Vec::new(),
        usage_basis: None,
    };
    let saved = store.remember_glance("panel answer", &receipt).unwrap();
    drop(store);
    let mut store = database.open();
    let edited = store.edit_memory(saved.id, "panel answer, edited").unwrap();
    let provenance = edited.provenance.as_ref().unwrap();
    assert_eq!(provenance["source"], "contextual_panel");
    assert_eq!(provenance["authorization"], "explicit_user_remember");
    // Editing keeps the origin labels: an edit never downgrades sensitivity.
    assert!(edited.labels().contains(CLIPBOARD_TEXT));
    assert!(edited.labels().contains(MEMORY));
}

#[test]
fn sqlite_build_includes_sqlcipher_community_and_fts5() {
    let connection = Connection::open_in_memory().unwrap();
    let version: String = connection
        .pragma_query_value(None, "cipher_version", |row| row.get(0))
        .expect("SQLCipher must be linked");
    assert!(version.contains("community"), "{version}");
    connection
        .execute_batch("CREATE VIRTUAL TABLE search USING fts5(text);")
        .unwrap();
}
