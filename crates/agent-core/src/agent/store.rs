//! Metadata-only durable task history contract.
use serde::Serialize;
use serde_json::Value;
use super::{AgentResult, PermissionAuditBatch, RuntimeEvent, TaskId, TaskState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTask {
    pub id: TaskId,
    pub state: TaskState,
    pub created_at: i64,
    pub updated_at: i64,
    pub finalized: bool,
    pub usage: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTaskEvent {
    pub sequence: i64,
    pub created_at: i64,
    pub event: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredExecution {
    pub operation_id: String,
    pub tool: String,
    /// pending is an unresolved intent; uncertain is its recovered status.
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAudit {
    pub namespace: String,
    pub sequence: i64,
    pub created_at: i64,
    pub record: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTaskDetail {
    pub task: StoredTask,
    pub events: Vec<StoredTaskEvent>,
    pub executions: Vec<StoredExecution>,
    pub audit: Vec<StoredAudit>,
}

/// Called only by trusted host code. Allocate the ID before opening the
/// policy task. If startup fails, interrupt the allocated row explicitly.
/// A successful record/persist return must mean the transaction committed.
pub trait AgentStore {
    fn allocate_agent_task(&mut self) -> AgentResult<TaskId>;
    fn record_agent_event(&mut self, id: TaskId, event: &RuntimeEvent) -> AgentResult<()>;
    fn persist_permission_audit(&mut self, namespace: &str, batch: &PermissionAuditBatch) -> AgentResult<()>;
    fn interrupt_agent_task(&mut self, id: TaskId) -> AgentResult<()>;
    fn list_agent_tasks(&self, before: Option<TaskId>, limit: u32) -> AgentResult<Vec<StoredTask>>;
    fn agent_task(&self, id: TaskId) -> AgentResult<StoredTaskDetail>;
}
