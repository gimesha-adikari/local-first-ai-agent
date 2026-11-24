use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

use agent_core::{chat::Appearance, inference::FinishReason};

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

    fn open(&self) -> SqliteStore {
        SqliteStore::open(&self.path()).unwrap()
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn in_memory() -> SqliteStore {
    SqliteStore::open(Path::new(":memory:")).unwrap()
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
         INSERT INTO models VALUES ('keep this');
         PRAGMA user_version = 7;",
        )
        .unwrap();
    drop(manual);

    let error = SqliteStore::open(&database.path()).err().unwrap();
    assert_eq!(error.code, ErrorCode::Storage);
    assert!(error.message.contains("unsupported schema version"));
    let manual = Connection::open(database.path()).unwrap();
    let version: i64 = manual
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    manual.pragma_update(None, "user_version", 0).unwrap();
    drop(manual);

    let error = SqliteStore::open(&database.path()).err().unwrap();
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
    let second_open = SqliteStore::open(&database.path()).err().unwrap();
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
