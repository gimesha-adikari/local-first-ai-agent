use std::{fs, sync::atomic::{AtomicU64, Ordering}, time::Duration};
use agent_core::{
    agent::{
        AgentStore, AgentTask, AuthorizationDecision, BudgetUsage, ExecutionRecord,
        ExecutionStatus, PermissionEngine, RuntimeEvent, TaskId, TaskState, ToolId, ToolRegistry,
    },
    inference::Cancellation,
};
use super::*;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("agent-journal-{}-{}-{}",
            std::process::id(), unix_ms().unwrap(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn path(&self) -> PathBuf { self.0.join("chat.sqlite3") }
    fn open(&self) -> SqliteStore { SqliteStore::open(&self.path()).unwrap() }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn transition(store: &mut SqliteStore, task: &mut AgentTask, state: TaskState) {
    let event = RuntimeEvent::Transition { transition: task.transition(state).unwrap() };
    store.record_agent_event(task.id(), &event).unwrap();
}

fn started(store: &mut SqliteStore) -> AgentTask {
    let id = store.allocate_agent_task().unwrap();
    store.record_agent_event(id, &RuntimeEvent::Started).unwrap();
    let mut task = AgentTask::new(id);
    transition(store, &mut task, TaskState::Planning);
    task
}

fn intent(store: &mut SqliteStore, task: &mut AgentTask, policy: &mut PermissionEngine) -> String {
    let registry = ToolRegistry::mvp().unwrap();
    let id = task.id();
    transition(store, task, TaskState::Validating);
    store.record_agent_event(id, &RuntimeEvent::Proposed { tool: ToolId::Calculate }).unwrap();
    let call = registry.validate(ToolId::Calculate, r#"{"expression":"2+2"}"#).unwrap();
    let AuthorizationDecision::Allow { operation_id } =
        policy.authorize(&registry, id, &call, Duration::from_secs(30)).unwrap()
    else { panic!("expected allow") };
    transition(store, task, TaskState::Authorized);
    let _permit = policy.consume(&registry, id, &operation_id, &call).unwrap();
    transition(store, task, TaskState::Executing);
    let event = RuntimeEvent::DispatchIntent { operation_id: operation_id.clone(), tool: ToolId::Calculate };
    assert!(store.record_agent_event(id, &event).is_err(), "must persist authorization first");
    policy.flush_audit(|namespace, batch| store.persist_permission_audit(namespace, batch)).unwrap();
    store.record_agent_event(id, &event).unwrap();
    assert!(store.record_agent_event(id, &event).is_err(), "intent cannot be replayed");
    operation_id
}

#[test]
fn migration_preserves_v1_chat_and_preferences() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.path()).unwrap();
    db.execute_batch(include_str!("../migrations/001_chat.sql")).unwrap();
    db.execute_batch("PRAGMA user_version = 1;
        INSERT INTO conversations(title, created_at, updated_at) VALUES ('Old chat', 1, 1);
        INSERT INTO messages(conversation_id, position, role, state, content, created_at)
        VALUES (1, 0, 'user', 'complete', 'Saved text', 1);").unwrap();
    let preferences = Preferences::default();
    db.execute("INSERT INTO application_preferences(id, preferences) VALUES (1, ?1)",
        [serde_json::to_string(&preferences).unwrap()]).unwrap();
    drop(db);
    let mut store = fixture.open();
    assert_eq!(store.get(1).unwrap().messages[0].content, "Saved text");
    assert_eq!(store.preferences().unwrap().defaults.model.threads, preferences.defaults.model.threads);
    let version: i64 = store.connection.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 2);
    assert!(store.allocate_agent_task().is_ok());
}

#[test]
fn failed_v2_migration_keeps_v1_version_and_chat() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.path()).unwrap();
    db.execute_batch(include_str!("../migrations/001_chat.sql")).unwrap();
    db.execute_batch("PRAGMA user_version = 1;
        INSERT INTO conversations(title, created_at, updated_at) VALUES ('Keep', 1, 1);
        CREATE TABLE agent_task_events (existing TEXT);").unwrap();
    drop(db);
    assert!(SqliteStore::open(&fixture.path()).is_err());
    let db = Connection::open(fixture.path()).unwrap();
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 1);
    let title: String = db.query_row("SELECT title FROM conversations", [], |r| r.get(0)).unwrap();
    assert_eq!(title, "Keep");
    let created: i64 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE name='agent_tasks'", [], |r| r.get(0)).unwrap();
    assert_eq!(created, 0);
}

#[test]
fn durable_intent_and_outcome_are_bound_and_finalized() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let mut task = started(&mut store);
    let id = task.id();
    let mut policy = PermissionEngine::new("journal_test").unwrap();
    policy.begin_task(id, Duration::from_secs(30), Cancellation::default()).unwrap();
    let operation_id = intent(&mut store, &mut task, &mut policy);
    let mut record = ExecutionRecord {
        operation_id, tool: ToolId::DeviceInfo, status: ExecutionStatus::Succeeded, error: None,
    };
    assert!(store.record_agent_event(id, &RuntimeEvent::Execution { record: record.clone() }).is_err());
    record.tool = ToolId::Calculate;
    store.record_agent_event(id, &RuntimeEvent::Execution { record: record.clone() }).unwrap();
    assert!(store.record_agent_event(id, &RuntimeEvent::Execution { record }).is_err());
    transition(&mut store, &mut task, TaskState::Observing);
    transition(&mut store, &mut task, TaskState::Planning);
    transition(&mut store, &mut task, TaskState::Completed);
    policy.end_task(id);
    policy.flush_audit(|namespace, batch| store.persist_permission_audit(namespace, batch)).unwrap();
    store.record_agent_event(id, &RuntimeEvent::Finished {
        state: TaskState::Completed, usage: BudgetUsage::default(), error: None,
    }).unwrap();
    let detail = store.agent_task(id).unwrap();
    assert!(detail.task.finalized);
    assert_eq!(detail.task.state, TaskState::Completed);
    assert_eq!(detail.executions[0].status, "succeeded");
    assert!(detail.events.windows(2).all(|p| p[0].sequence + 1 == p[1].sequence));
    assert!(!serde_json::to_string(&detail).unwrap().contains("2+2"));
    assert!(store.record_agent_event(id, &RuntimeEvent::Started).is_err());
}

#[test]
fn restart_marks_pending_intents_uncertain_without_replay() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut task = started(&mut store);
    let id = task.id();
    let mut policy = PermissionEngine::new("recovery_test").unwrap();
    policy.begin_task(id, Duration::from_secs(30), Cancellation::default()).unwrap();
    let operation_id = intent(&mut store, &mut task, &mut policy);
    store.record_agent_event(id, &RuntimeEvent::Execution { record: ExecutionRecord {
        operation_id, tool: ToolId::Calculate, status: ExecutionStatus::Succeeded, error: None,
    }}).unwrap();
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
    assert_eq!(detail.executions.iter().find(|e| e.operation_id == pending).unwrap().status, "uncertain");
    assert!(detail.executions.iter().any(|e| e.status == "succeeded"));
    assert_eq!(detail.events.last().unwrap().event["kind"], "recovered_interrupted");
    assert!(store.allocate_agent_task().unwrap().get() > id.get());
}

#[test]
fn audit_batch_retry_is_idempotent_and_conflicts_fail() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let mut policy = PermissionEngine::new("audit_test").unwrap();
    policy.grant_scope(agent_core::agent::ScopeKind::File, Duration::from_secs(30)).unwrap();
    let mut batch = policy.drain_audit();
    store.persist_permission_audit("audit_test", &batch).unwrap();
    store.persist_permission_audit("audit_test", &batch).unwrap();
    let count: i64 = store.connection.query_row("SELECT count(*) FROM permission_audit", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    batch.omitted = 1;
    assert!(store.persist_permission_audit("audit_test", &batch).is_err());
    batch.omitted = 0;
    batch.records[0].elapsed_ms += 1;
    assert!(store.persist_permission_audit("audit_test", &batch).is_err());
    policy.grant_scope(agent_core::agent::ScopeKind::Directory, Duration::from_secs(30)).unwrap();
    let mut batch = policy.drain_audit();
    batch.omitted = 2;
    store.persist_permission_audit("audit_test", &batch).unwrap();
    store.persist_permission_audit("audit_test", &batch).unwrap();
    let omissions: i64 = store.connection.query_row("SELECT sum(omitted) FROM permission_audit_batches", [], |r| r.get(0)).unwrap();
    assert_eq!(omissions, 2);
}

#[test]
fn history_pagination_and_cross_mode_exclusion() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let conversation = store.create().unwrap();
    let first = store.allocate_agent_task().unwrap();
    assert!(store.allocate_agent_task().is_err());
    assert!(store.begin(conversation.id, TurnAction::Send("Conflict".into())).is_err());
    store.interrupt_agent_task(first).unwrap();
    let chat = store.begin(conversation.id, TurnAction::Send("Hello".into())).unwrap();
    assert!(store.allocate_agent_task().is_err());
    store.finish(chat.assistant.id, "Hi", GenerationState::Complete, None).unwrap();
    let second = store.allocate_agent_task().unwrap();
    store.interrupt_agent_task(second).unwrap();
    assert_eq!(store.list_agent_tasks(None, 1).unwrap()[0].id, second);
    assert_eq!(store.list_agent_tasks(Some(second), 1).unwrap()[0].id, first);
    assert!(store.list_agent_tasks(Some(first), 1).unwrap().is_empty());
    assert!(store.list_agent_tasks(None, 101).is_err());
}

#[test]
fn rejects_skipped_and_cross_task_transitions() {
    let mut store = SqliteStore::open(Path::new(":memory:")).unwrap();
    let task = started(&mut store);
    let event = RuntimeEvent::Transition { transition: agent_core::agent::TaskTransition {
        task_id: task.id(), sequence: 2, from: TaskState::Planning, to: TaskState::Executing,
    }};
    assert!(store.record_agent_event(task.id(), &event).is_err());
    let event = RuntimeEvent::Transition { transition: agent_core::agent::TaskTransition {
        task_id: TaskId::new(task.id().get() + 1).unwrap(), sequence: 2,
        from: TaskState::Planning, to: TaskState::Validating,
    }};
    assert!(store.record_agent_event(task.id(), &event).is_err());
    assert_eq!(store.agent_task(task.id()).unwrap().task.state, TaskState::Planning);
}
