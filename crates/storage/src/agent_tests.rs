use super::*;
use agent_core::{
    agent::{
        AgentStore, AgentTask, AuthorizationDecision, BudgetUsage, ExecutionRecord,
        ExecutionStatus, PermissionEngine, RuntimeEvent, TaskId, TaskState, ToolId, ToolRegistry,
    },
    inference::Cancellation,
};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "agent-journal-{}-{}-{}",
            std::process::id(),
            unix_ms().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn path(&self) -> PathBuf {
        self.0.join("chat.sqlite3")
    }
    fn open(&self) -> SqliteStore {
        SqliteStore::open(&self.path()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn transition(store: &mut SqliteStore, task: &mut AgentTask, state: TaskState) {
    let event = RuntimeEvent::Transition {
        transition: task.transition(state).unwrap(),
    };
    store.record_agent_event(task.id(), &event).unwrap();
}

fn started(store: &mut SqliteStore) -> AgentTask {
    let id = store.allocate_agent_task().unwrap();
    store
        .record_agent_event(id, &RuntimeEvent::Started)
        .unwrap();
    let mut task = AgentTask::new(id);
    transition(store, &mut task, TaskState::Planning);
    task
}

fn intent(store: &mut SqliteStore, task: &mut AgentTask, policy: &mut PermissionEngine) -> String {
    let registry = ToolRegistry::mvp().unwrap();
    let id = task.id();
    transition(store, task, TaskState::Validating);
    store
        .record_agent_event(
            id,
            &RuntimeEvent::Proposed {
                tool: ToolId::Calculate,
            },
        )
        .unwrap();
    let call = registry
        .validate(ToolId::Calculate, r#"{"expression":"2+2"}"#)
        .unwrap();
    let AuthorizationDecision::Allow { operation_id } = policy
        .authorize(&registry, id, &call, Duration::from_secs(30))
        .unwrap()
    else {
        panic!("expected allow")
    };
    transition(store, task, TaskState::Authorized);
    let _permit = policy.consume(&registry, id, &operation_id, &call).unwrap();
    transition(store, task, TaskState::Executing);
    let event = RuntimeEvent::DispatchIntent {
        operation_id: operation_id.clone(),
        tool: ToolId::Calculate,
    };
    assert!(
        store.record_agent_event(id, &event).is_err(),
        "must persist authorization first"
    );
    policy
        .flush_audit(|namespace, batch| store.persist_permission_audit(namespace, batch))
        .unwrap();
    store.record_agent_event(id, &event).unwrap();
    assert!(
        store.record_agent_event(id, &event).is_err(),
        "intent cannot be replayed"
    );
    operation_id
}

#[test]
fn migration_preserves_v1_chat_and_preferences() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.path()).unwrap();
    db.execute_batch(include_str!("../migrations/001_chat.sql"))
        .unwrap();
    db.execute_batch(
        "PRAGMA user_version = 1;
        INSERT INTO conversations(title, created_at, updated_at) VALUES ('Old chat', 1, 1);
        INSERT INTO messages(conversation_id, position, role, state, content, created_at)
        VALUES (1, 0, 'user', 'complete', 'Saved text', 1);",
    )
    .unwrap();
    let preferences = Preferences::default();
    db.execute(
        "INSERT INTO application_preferences(id, preferences) VALUES (1, ?1)",
        [serde_json::to_string(&preferences).unwrap()],
    )
    .unwrap();
    drop(db);
    let mut store = fixture.open();
    assert_eq!(store.get(1).unwrap().messages[0].content, "Saved text");
    assert_eq!(
        store.preferences().unwrap().defaults.model.threads,
        preferences.defaults.model.threads
    );
    let version: i64 = store
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert!(store.allocate_agent_task().is_ok());
}

#[test]
fn failed_v2_migration_keeps_v1_version_and_chat() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.path()).unwrap();
    db.execute_batch(include_str!("../migrations/001_chat.sql"))
        .unwrap();
    db.execute_batch(
        "PRAGMA user_version = 1;
        INSERT INTO conversations(title, created_at, updated_at) VALUES ('Keep', 1, 1);
        CREATE TABLE agent_task_events (existing TEXT);",
    )
    .unwrap();
    drop(db);
    assert!(SqliteStore::open(&fixture.path()).is_err());
    let db = Connection::open(fixture.path()).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let title: String = db
        .query_row("SELECT title FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Keep");
    let created: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='agent_tasks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(created, 0);
}

#[test]
fn durable_intent_and_outcome_are_bound_and_finalized() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let mut task = started(&mut store);
    let id = task.id();
    let mut policy = PermissionEngine::new("journal_test").unwrap();
    policy
        .begin_task(id, Duration::from_secs(30), Cancellation::default())
        .unwrap();
    let operation_id = intent(&mut store, &mut task, &mut policy);
    let mut record = ExecutionRecord {
        operation_id,
        tool: ToolId::DeviceInfo,
        status: ExecutionStatus::Succeeded,
        error: None,
    };
    assert!(
        store
            .record_agent_event(
                id,
                &RuntimeEvent::Execution {
                    record: record.clone()
                }
            )
            .is_err()
    );
    record.tool = ToolId::Calculate;
    store
        .record_agent_event(
            id,
            &RuntimeEvent::Execution {
                record: record.clone(),
            },
        )
        .unwrap();
    assert!(
        store
            .record_agent_event(id, &RuntimeEvent::Execution { record })
            .is_err()
    );
    transition(&mut store, &mut task, TaskState::Observing);
    transition(&mut store, &mut task, TaskState::Planning);
    transition(&mut store, &mut task, TaskState::Completed);
    policy.end_task(id);
    policy
        .flush_audit(|namespace, batch| store.persist_permission_audit(namespace, batch))
        .unwrap();
    store
        .record_agent_event(
            id,
            &RuntimeEvent::Finished {
                state: TaskState::Completed,
                usage: BudgetUsage::default(),
                error: None,
            },
        )
        .unwrap();
    let detail = store.agent_task(id).unwrap();
    assert!(detail.task.finalized);
    assert_eq!(detail.task.state, TaskState::Completed);
    assert_eq!(detail.executions[0].status, "succeeded");
    assert!(
        detail
            .events
            .windows(2)
            .all(|p| p[0].sequence + 1 == p[1].sequence)
    );
    assert!(!serde_json::to_string(&detail).unwrap().contains("2+2"));
    assert!(
        store
            .record_agent_event(id, &RuntimeEvent::Started)
            .is_err()
    );
}

#[test]
fn restart_marks_pending_intents_uncertain_without_replay() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut task = started(&mut store);
    let id = task.id();
    let mut policy = PermissionEngine::new("recovery_test").unwrap();
    policy
        .begin_task(id, Duration::from_secs(30), Cancellation::default())
        .unwrap();
    let operation_id = intent(&mut store, &mut task, &mut policy);
    store
        .record_agent_event(
            id,
            &RuntimeEvent::Execution {
                record: ExecutionRecord {
                    operation_id,
                    tool: ToolId::Calculate,
                    status: ExecutionStatus::Succeeded,
                    error: None,
                },
            },
        )
        .unwrap();
    transition(&mut store, &mut task, TaskState::Observing);
    transition(&mut store, &mut task, TaskState::Planning);
    let pending = intent(&mut store, &mut task, &mut policy);
    assert!(SqliteStore::open(&fixture.path()).is_err());
    drop(store);
    drop(policy);
    let mut store = fixture.open();
    let detail = store.agent_task(id).unwrap();
    assert_eq!(detail.task.state, TaskState::Interrupted);
    assert!(detail.task.finalized);
    assert_eq!(
        detail
            .executions
            .iter()
            .find(|e| e.operation_id == pending)
            .unwrap()
            .status,
        "uncertain"
    );
    assert!(detail.executions.iter().any(|e| e.status == "succeeded"));
    assert_eq!(
        detail.events.last().unwrap().event["kind"],
        "recovered_interrupted"
    );
    assert!(store.allocate_agent_task().unwrap().get() > id.get());
}

#[test]
fn audit_batch_retry_is_idempotent_and_conflicts_fail() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let mut policy = PermissionEngine::new("audit_test").unwrap();
    policy
        .grant_scope(agent_core::agent::ScopeKind::File, Duration::from_secs(30))
        .unwrap();
    let mut batch = policy.drain_audit();
    store
        .persist_permission_audit("audit_test", &batch)
        .unwrap();
    store
        .persist_permission_audit("audit_test", &batch)
        .unwrap();
    let count: i64 = store
        .connection
        .query_row("SELECT count(*) FROM permission_audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    batch.omitted = 1;
    assert!(
        store
            .persist_permission_audit("audit_test", &batch)
            .is_err()
    );
    batch.omitted = 0;
    batch.records[0].elapsed_ms += 1;
    assert!(
        store
            .persist_permission_audit("audit_test", &batch)
            .is_err()
    );
    policy
        .grant_scope(
            agent_core::agent::ScopeKind::Directory,
            Duration::from_secs(30),
        )
        .unwrap();
    let mut batch = policy.drain_audit();
    batch.omitted = 2;
    store
        .persist_permission_audit("audit_test", &batch)
        .unwrap();
    store
        .persist_permission_audit("audit_test", &batch)
        .unwrap();
    let omissions: i64 = store
        .connection
        .query_row(
            "SELECT sum(omitted) FROM permission_audit_batches",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(omissions, 2);
}

#[test]
fn history_pagination_and_cross_mode_exclusion() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let conversation = store.create().unwrap();
    let first = store.allocate_agent_task().unwrap();
    assert!(store.allocate_agent_task().is_err());
    assert!(
        store
            .begin(conversation.id, TurnAction::Send("Conflict".into()))
            .is_err()
    );
    store.interrupt_agent_task(first).unwrap();
    let chat = store
        .begin(conversation.id, TurnAction::Send("Hello".into()))
        .unwrap();
    assert!(store.allocate_agent_task().is_err());
    store
        .finish(chat.assistant.id, "Hi", GenerationState::Complete, None)
        .unwrap();
    let second = store.allocate_agent_task().unwrap();
    store.interrupt_agent_task(second).unwrap();
    assert_eq!(store.list_agent_tasks(None, 1).unwrap()[0].id, second);
    assert_eq!(
        store.list_agent_tasks(Some(second), 1).unwrap()[0].id,
        first
    );
    assert!(store.list_agent_tasks(Some(first), 1).unwrap().is_empty());
    assert!(store.list_agent_tasks(None, 101).is_err());
}

#[test]
fn rejects_skipped_and_cross_task_transitions() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let task = started(&mut store);
    let event = RuntimeEvent::Transition {
        transition: agent_core::agent::TaskTransition {
            task_id: task.id(),
            sequence: 2,
            from: TaskState::Planning,
            to: TaskState::Executing,
        },
    };
    assert!(store.record_agent_event(task.id(), &event).is_err());
    let event = RuntimeEvent::Transition {
        transition: agent_core::agent::TaskTransition {
            task_id: TaskId::new(task.id().get() + 1).unwrap(),
            sequence: 2,
            from: TaskState::Planning,
            to: TaskState::Validating,
        },
    };
    assert!(store.record_agent_event(task.id(), &event).is_err());
    assert_eq!(
        store.agent_task(task.id()).unwrap().task.state,
        TaskState::Planning
    );
}

fn start_conversation_task(store: &mut SqliteStore, link: AgentTurnLink) -> AgentTask {
    store
        .record_agent_event(link.task_id, &RuntimeEvent::Started)
        .unwrap();
    let mut task = AgentTask::new(link.task_id);
    transition(store, &mut task, TaskState::Planning);
    task
}

fn finish_conversation_task(
    store: &mut SqliteStore,
    task: &mut AgentTask,
    state: TaskState,
    answer: Option<&str>,
) {
    transition(store, task, state);
    store
        .record_agent_event_with_answer(
            task.id(),
            &RuntimeEvent::Finished {
                state,
                usage: BudgetUsage::default(),
                error: None,
            },
            answer,
        )
        .unwrap();
}

#[test]
fn v2_migration_preserves_old_chat_and_unlinked_agent_history() {
    let fixture = Fixture::new();
    let connection = Connection::open(fixture.path()).unwrap();
    connection
        .execute_batch(include_str!("../migrations/001_chat.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/002_agent.sql"))
        .unwrap();
    connection.execute_batch(
        "PRAGMA user_version = 2;
         INSERT INTO conversations(id,title,created_at,updated_at) VALUES(42,'Version 2 chat',7,8);
         INSERT INTO messages(id,conversation_id,position,role,state,content,created_at)
            VALUES(71,42,0,'user','complete','Synthetic old question',7),
                  (72,42,1,'assistant','complete','Synthetic old reply',8);
         INSERT INTO agent_tasks(id,state,finalized,created_at,updated_at) VALUES(91,'completed',1,9,10);
         INSERT INTO agent_task_events(task_id,sequence,created_at,event) VALUES(91,1,10,'{}');
         INSERT INTO agent_executions(operation_id,task_id,tool,status,created_at,updated_at)
           VALUES('legacy_operation',91,'math.calculate','succeeded',9,10);"
    ).unwrap();
    drop(connection);
    let store = fixture.open();
    assert_eq!(
        store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let (chat, turns) = store.unified_conversation(42).unwrap();
    assert_eq!(chat.title, "Version 2 chat");
    assert_eq!(turns[0].user.id, 71);
    assert_eq!(turns[0].assistants[0].0.content, "Synthetic old reply");
    assert!(turns[0].assistants[0].1.is_none());
    assert!(turns[0].agent_task.is_none());
    let legacy = store.agent_task(TaskId::new(91).unwrap()).unwrap();
    assert_eq!(legacy.executions[0].operation_id, "legacy_operation");
    assert_eq!(legacy.events.len(), 1);
    assert_db_integrity(&store);
}

fn assert_db_integrity(store: &SqliteStore) {
    assert_eq!(
        store
            .connection
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
}

#[test]
fn v3_migration_preserves_linked_task_error_and_leaves_unknown_context_count_null() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.path()).unwrap();
    for migration in [
        include_str!("../migrations/001_chat.sql"),
        include_str!("../migrations/002_agent.sql"),
        include_str!("../migrations/003_conversation_agent_turns.sql"),
    ] {
        db.execute_batch(migration).unwrap();
    }
    db.execute_batch(
        "PRAGMA user_version = 3;
         INSERT INTO conversations(id,title,created_at,updated_at) VALUES(42,'Existing thread',7,8);
         INSERT INTO messages(id,conversation_id,position,role,state,content,created_at)
             VALUES(71,42,0,'user','complete','Synthetic Agent question',7),
                   (72,42,1,'assistant','failed','',8);
         INSERT INTO agent_tasks(id,state,finalized,created_at,updated_at,error)
             VALUES(91,'denied',1,7,8,'permission_denied'),(92,'completed',1,7,8,NULL);
         INSERT INTO conversation_agent_turns(task_id,conversation_id,user_message_id,
             assistant_message_id,task_timeout_ms) VALUES(91,42,71,72,240000);
         INSERT INTO agent_task_events(task_id,sequence,created_at,event) VALUES(91,1,8,'{}');",
    )
    .unwrap();
    drop(db);

    let store = fixture.open();
    let summaries = store.list_agent_tasks(None, 10).unwrap();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id.get(), 92);
    assert_eq!(summaries[0].conversation_id, None);
    assert_eq!(summaries[0].turn_id, None);
    assert_eq!(summaries[1].conversation_id, Some(42));
    assert_eq!(summaries[1].turn_id, Some(71));
    assert_eq!(summaries[1].excluded_turns, None);
    let (_, turns) = store.unified_conversation(42).unwrap();
    assert_eq!(turns[0].task_state, Some(TaskState::Denied));
    assert_eq!(turns[0].task_timeout_ms, Some(240_000));
    assert_eq!(turns[0].task_error.as_deref(), Some("permission_denied"));
    assert_eq!(turns[0].excluded_turns, None);
    assert_db_integrity(&store);
}

#[test]
fn omission_count_is_monotonic_and_available_after_reopening_the_conversation() {
    let fixture = Fixture::new();
    let id;
    let link;
    {
        let mut store = fixture.open();
        id = store.create().unwrap().id;
        for _ in 0..14 {
            let turn = store
                .begin(id, TurnAction::Send("Earlier synthetic turn".into()))
                .unwrap();
            store
                .finish(
                    turn.assistant.id,
                    "Saved answer",
                    GenerationState::Complete,
                    None,
                )
                .unwrap();
        }
        let (_, storage_omitted) = store.agent_context(id).unwrap();
        link = store
            .begin_agent_turn(id, "Agent follow-up", "model.gguf", 300_000)
            .unwrap();
        let task = store.agent_task(link.task_id).unwrap().task;
        assert_eq!(task.excluded_turns, Some(storage_omitted));
        assert_eq!(storage_omitted, 2);

        let mut runtime_task = start_conversation_task(&mut store, link);
        store.record_agent_excluded_turns(link.task_id, 6).unwrap();
        store.record_agent_excluded_turns(link.task_id, 4).unwrap();
        finish_conversation_task(
            &mut store,
            &mut runtime_task,
            TaskState::Completed,
            Some("Later answer"),
        );
        assert!(store.record_agent_excluded_turns(link.task_id, 7).is_err());
    }
    let mut store = fixture.open();
    let (_, turns) = store.unified_conversation(id).unwrap();
    assert_eq!(turns.len(), 15);
    assert_eq!(turns[0].excluded_turns, None);
    assert_eq!(turns[14].excluded_turns, Some(6));
    assert_eq!(turns[14].task_state, Some(TaskState::Completed));
    assert_eq!(
        store.agent_task(link.task_id).unwrap().task.excluded_turns,
        Some(6)
    );
    store.delete(id).unwrap();
    assert!(store.list_agent_tasks(None, 10).unwrap().is_empty());
    assert_db_integrity(&store);
}

#[test]
fn mixed_conversation_restores_order_and_keeps_task_identity_and_context_isolation() {
    let fixture = Fixture::new();
    let id;
    let links;
    {
        let mut store = fixture.open();
        id = store.create().unwrap().id;
        let other = store.create().unwrap().id;
        let chat = store
            .begin_chat(
                id,
                TurnAction::Send("Call the station Juniper.".into()),
                Some("local-chat.gguf"),
            )
            .unwrap();
        store
            .finish(
                chat.assistant.id,
                "I will.",
                GenerationState::Complete,
                None,
            )
            .unwrap();
        let mut collected = Vec::new();
        for (prompt, answer) in [
            ("Find the ledger files.", "alpha.txt and beta.txt"),
            ("Read the first file.", "The file identifies Juniper."),
            ("Summarize its main point.", "Juniper is the station name."),
        ] {
            let link = store
                .begin_agent_turn(id, prompt, "local-agent.gguf", 300_000)
                .unwrap();
            assert!(
                store
                    .begin(other, TurnAction::Send("Blocked".into()))
                    .is_err()
            );
            assert!(store.delete(id).is_err());
            let mut task = start_conversation_task(&mut store, link);
            finish_conversation_task(&mut store, &mut task, TaskState::Completed, Some(answer));
            collected.push(link);
        }
        links = collected;
        assert!(store.begin(id, TurnAction::Regenerate).is_err());
        let chat = store
            .begin_chat(
                id,
                TurnAction::Send("What is the station called?".into()),
                Some("local-chat.gguf"),
            )
            .unwrap();
        assert_eq!(chat.history.turns.len(), 4);
        assert_eq!(
            chat.history.turns[3].assistant,
            "Juniper is the station name."
        );
        store
            .finish(
                chat.assistant.id,
                "Juniper",
                GenerationState::Complete,
                None,
            )
            .unwrap();
        assert!(store.agent_context(other).unwrap().0.is_empty());
    }
    let store = fixture.open();
    let (conversation, turns) = store.unified_conversation(id).unwrap();
    assert_eq!(conversation.title, "Call the station Juniper.");
    assert_eq!(turns.len(), 5);
    for (turn, link) in turns[1..4].iter().zip(links) {
        assert_eq!(turn.user.id, link.turn_id);
        assert_eq!(turn.agent_task, Some(link.task_id));
        assert_eq!(turn.assistants[0].1.as_deref(), Some("local-agent.gguf"));
        assert_eq!(
            store.agent_task(link.task_id).unwrap().task.state,
            TaskState::Completed
        );
    }
    assert_eq!(turns[4].assistants[0].0.content, "Juniper");
    assert_eq!(store.list("alpha.txt").unwrap()[0].id, id);
    assert_db_integrity(&store);
}

#[test]
fn interruption_preserves_intent_then_deletion_removes_content_and_detaches_audit() {
    let fixture = Fixture::new();
    let conversation;
    let link;
    let operation;
    let mut policy = PermissionEngine::new("linked_recovery").unwrap();
    {
        let mut store = fixture.open();
        conversation = store.create().unwrap().id;
        link = store
            .begin_agent_turn(
                conversation,
                "Synthetic retained prompt",
                "model.gguf",
                300_000,
            )
            .unwrap();
        let mut task = start_conversation_task(&mut store, link);
        policy
            .begin_task(
                link.task_id,
                Duration::from_secs(30),
                Cancellation::default(),
            )
            .unwrap();
        operation = intent(&mut store, &mut task, &mut policy);
    }
    let mut store = fixture.open();
    let (_, turns) = store.unified_conversation(conversation).unwrap();
    assert_eq!(turns[0].assistants[0].0.state, GenerationState::Interrupted);
    assert!(turns[0].assistants[0].0.content.is_empty());
    let interrupted = store.agent_task(link.task_id).unwrap();
    assert_eq!(interrupted.executions[0].operation_id, operation);
    assert_eq!(interrupted.executions[0].status, "uncertain");
    store.delete(conversation).unwrap();
    assert!(store.get(conversation).is_err());
    assert!(store.agent_task(link.task_id).is_err());
    assert!(store.list("Synthetic retained prompt").unwrap().is_empty());
    let remaining: Vec<String> = store.connection.prepare(
        "SELECT record FROM permission_audit WHERE task_id IS NULL AND operation_id IS NULL ORDER BY sequence",
    ).unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    assert!(!remaining.is_empty());
    assert!(
        remaining
            .iter()
            .all(|r| !r.contains(&operation) && !r.contains("taskId"))
    );
    let mut next = PermissionEngine::new("fresh_audit_namespace").unwrap();
    let id = store.allocate_agent_task().unwrap();
    next.begin_task(id, Duration::from_secs(30), Cancellation::default())
        .unwrap();
    next.flush_audit(|namespace, batch| store.persist_permission_audit(namespace, batch))
        .unwrap();
    assert_db_integrity(&store);
}

#[test]
fn conversation_context_selects_only_the_latest_bounded_suffix() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let id = store.create().unwrap().id;
    for index in 0..15 {
        let turn = store
            .begin(id, TurnAction::Send(format!("Question {index}")))
            .unwrap();
        store
            .finish(
                turn.assistant.id,
                &format!("Answer {index}"),
                GenerationState::Complete,
                None,
            )
            .unwrap();
    }
    let (turns, omitted) = store.agent_context(id).unwrap();
    assert_eq!(omitted, 3);
    assert_eq!(turns.len(), 12);
    assert_eq!(turns[0].1.user, "Question 3");
    assert_eq!(turns[11].1.assistant, "Answer 14");
    assert_eq!(store.get(id).unwrap().messages.len(), 30);
}

#[test]
fn linked_finalization_rolls_back_the_journal_when_answer_persistence_fails() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let id = store.create().unwrap().id;
    let link = store
        .begin_agent_turn(id, "Synthetic prompt", "model.gguf", 300_000)
        .unwrap();
    let mut task = start_conversation_task(&mut store, link);
    transition(&mut store, &mut task, TaskState::Completed);
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_answer BEFORE UPDATE OF content ON messages
         WHEN NEW.role='assistant' BEGIN SELECT RAISE(ABORT,'synthetic write failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .record_agent_event_with_answer(
                link.task_id,
                &RuntimeEvent::Finished {
                    state: TaskState::Completed,
                    usage: BudgetUsage::default(),
                    error: None
                },
                Some("Must roll back")
            )
            .is_err()
    );
    assert!(!store.agent_task(link.task_id).unwrap().task.finalized);
    assert!(store.get(id).unwrap().messages[1].content.is_empty());
    store.interrupt_agent_task(link.task_id).unwrap();
    assert_eq!(
        store.get(id).unwrap().messages[1].state,
        GenerationState::Interrupted
    );
    assert_db_integrity(&store);
}
