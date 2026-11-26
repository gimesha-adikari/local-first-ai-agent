//! Trusted Tauri bridge for the local agent runtime.
//! Native work runs on a dedicated task thread. The existing inference
//! worker remains the sole owner of the model and serializes agent decisions
//! with ordinary chat generation.
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    pin::Pin,
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use agent_core::{
    agent::{
        AgentError, AgentErrorCode, AgentLimits, AgentResult, AgentStore, ApprovalChoice,
        ApprovalRequest, BudgetUsage, ExecutionRecord, ExecutionStatus, PermissionAuditBatch,
        PermissionEngine, RuntimeEvent, RuntimeHost, ScopeKind, ScopeState, ScopeSummary,
        StoredAudit, StoredExecution, StoredTask, StoredTaskDetail, StoredTaskEvent, TaskId,
        TaskRequest, TaskState, ToolArguments, ToolId, ToolObservation, ToolRegistry, run_task,
    },
    chat::ConversationId,
    inference::{Cancellation, ErrorCode, InferenceError},
};
use agent_tools::{NativeTools, supported_registry};
use inference::{InferenceWorker, Pending};
use serde::Serialize;
use serde_json::Value;
use storage::SqliteStore;
use tauri::{AppHandle, Emitter, State};

use crate::commands::Desktop;

#[path = "agent_conversation.rs"]
pub(crate) mod agent_conversation;
use agent_conversation::{
    AgentTurnIdentity, CachedResult, CachedTurn, PreviousTurns, SessionResults, apply_context,
    context_unavailable, emit_turn,
};
use storage::AgentTurnLink;

const AGENT_EVENT: &str = "agent-event";
const SCOPE_LIFETIME: Duration = Duration::from_secs(60 * 60);
const APPROVAL_POLL: Duration = Duration::from_millis(100);

#[cfg(test)]
#[path = "agent_contract.rs"]
mod contract;

fn storage_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::StorageFailed,
        "The local agent journal is unavailable.",
    )
}
fn busy_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::Busy,
        "Another chat or agent operation is active.",
    )
}
fn invalid_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::InvalidArguments,
        "The agent request is invalid.",
    )
}
fn cancelled_error() -> AgentError {
    AgentError::new(AgentErrorCode::Cancelled, "The agent task was cancelled.")
}
fn unavailable_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        "The requested local capability is unavailable.",
    )
}
fn map_inference(_: InferenceError) -> AgentError {
    unavailable_error()
}
fn map_turn_error(error: InferenceError) -> AgentError {
    match error.code {
        ErrorCode::Busy => busy_error(),
        ErrorCode::NotFound => AgentError::new(
            AgentErrorCode::InvalidArguments,
            "The conversation no longer exists.",
        ),
        ErrorCode::InvalidConfig => invalid_error(),
        _ => storage_error(),
    }
}
fn parse_task_id(value: &str) -> AgentResult<TaskId> {
    value
        .parse::<u64>()
        .ok()
        .and_then(TaskId::new)
        .ok_or_else(invalid_error)
}
fn emit(app: &AppHandle, event: AgentEvent) -> bool {
    app.emit(AGENT_EVENT, event).is_ok()
}

#[cfg(unix)]
fn session_namespace() -> AgentResult<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| storage_error())?;
    let mut value = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").map_err(|_| storage_error())?;
    }
    Ok(value)
}
#[cfg(not(unix))]
fn session_namespace() -> AgentResult<String> {
    Err(AgentError::new(
        AgentErrorCode::UnsupportedPlatform,
        "A secure permission session is unavailable on this platform.",
    ))
}

struct ActiveAgent {
    id: TaskId,
    cancellation: Cancellation,
    approvals: HashMap<String, PendingApproval>,
    turn: Option<AgentTurnLink>,
    started_at: Option<i64>,
    task_timeout_ms: u64,
    tool: Option<ToolId>,
}

struct PendingApproval {
    sender: SyncSender<ApprovalChoice>,
    deadline: Instant,
    target: Option<ApprovalTarget>,
    tool: ToolId,
}

/// One ephemeral permission namespace and one native resource catalog per
/// application session. No scopes or approvals are restored from SQLite.
pub struct AgentSession {
    pub(crate) policy: Mutex<PermissionEngine>,
    pub(crate) registry: ToolRegistry,
    pub(crate) tools: NativeTools,
    active: Mutex<Option<ActiveAgent>>,
    task_thread: Mutex<Option<JoinHandle<()>>>,
    grant_refs: Mutex<HashMap<String, String>>,
    grant_namespace: String,
    latest_results: Mutex<Option<(TaskId, Vec<NativeResult>)>>,
    conversation_results: Mutex<SessionResults>,
}

impl AgentSession {
    pub fn new() -> AgentResult<Self> {
        let namespace = session_namespace()?;
        Ok(Self {
            policy: Mutex::new(PermissionEngine::new(&namespace)?),
            registry: supported_registry()?,
            tools: NativeTools::new(),
            active: Mutex::new(None),
            task_thread: Mutex::new(None),
            grant_refs: Mutex::new(HashMap::new()),
            grant_namespace: session_namespace()?,
            latest_results: Mutex::new(None),
            conversation_results: Mutex::new(SessionResults::default()),
        })
    }

    fn activate(
        &self,
        id: TaskId,
        cancellation: Cancellation,
        turn: Option<AgentTurnLink>,
        task_timeout_ms: u64,
    ) -> AgentResult<()> {
        let mut active = self.active.lock().map_err(|_| storage_error())?;
        if active.is_some() {
            return Err(busy_error());
        }
        *active = Some(ActiveAgent {
            id,
            cancellation,
            approvals: HashMap::new(),
            turn,
            started_at: None,
            task_timeout_ms,
            tool: None,
        });
        Ok(())
    }

    pub(crate) fn forget_conversation(&self, id: ConversationId) {
        if let Ok(mut results) = self.conversation_results.lock() {
            results.forget_conversation(id);
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.lock().map_or(true, |active| active.is_some())
    }

    fn record_progress(&self, id: TaskId, event: &RuntimeEvent) -> AgentResult<()> {
        let mut active = self.active.lock().map_err(|_| storage_error())?;
        let Some(task) = active.as_mut().filter(|task| task.id == id) else {
            return Ok(());
        };
        match event {
            RuntimeEvent::Started => {
                task.started_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok());
            }
            RuntimeEvent::Proposed { tool } => task.tool = Some(*tool),
            RuntimeEvent::Transition { transition }
                if transition.to == TaskState::Planning || transition.to.is_terminal() =>
            {
                task.tool = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn add_approval(
        &self,
        task_id: TaskId,
        operation_id: &str,
        sender: SyncSender<ApprovalChoice>,
        deadline: Instant,
        target: Option<ApprovalTarget>,
        tool: ToolId,
    ) -> AgentResult<()> {
        let mut active = self.active.lock().map_err(|_| storage_error())?;
        let task = active
            .as_mut()
            .filter(|task| task.id == task_id)
            .ok_or_else(cancelled_error)?;
        if task.approvals.contains_key(operation_id) {
            return Err(busy_error());
        }
        task.approvals.insert(
            operation_id.to_owned(),
            PendingApproval {
                sender,
                deadline,
                target,
                tool,
            },
        );
        Ok(())
    }

    fn remove_approval(&self, task_id: TaskId, operation_id: &str) {
        if let Ok(mut active) = self.active.lock()
            && let Some(task) = active.as_mut().filter(|task| task.id == task_id)
        {
            task.approvals.remove(operation_id);
        }
    }

    fn respond_approval(
        &self,
        task_id: TaskId,
        operation_id: &str,
        approve: bool,
    ) -> AgentResult<()> {
        let sender = {
            let mut active = self.active.lock().map_err(|_| storage_error())?;
            let task = active
                .as_mut()
                .filter(|task| task.id == task_id)
                .ok_or_else(cancelled_error)?;
            let pending = task.approvals.get(operation_id).ok_or_else(|| {
                AgentError::new(
                    AgentErrorCode::PermissionDenied,
                    "This approval is no longer pending.",
                )
            })?;
            if Instant::now() >= pending.deadline {
                return Err(AgentError::new(
                    AgentErrorCode::ApprovalExpired,
                    "The approval window closed before the application was authorized.",
                ));
            }
            task.approvals
                .remove(operation_id)
                .map(|approval| approval.sender)
                .ok_or_else(cancelled_error)?
        };
        sender
            .send(if approve {
                ApprovalChoice::AllowOnce
            } else {
                ApprovalChoice::Deny
            })
            .map_err(|_| cancelled_error())
    }

    fn cancel(&self, task_id: TaskId) -> AgentResult<bool> {
        let senders = {
            let mut active = self.active.lock().map_err(|_| storage_error())?;
            let Some(task) = active.as_mut().filter(|task| task.id == task_id) else {
                return Ok(false);
            };
            task.cancellation.cancel();
            std::mem::take(&mut task.approvals)
                .into_values()
                .map(|approval| approval.sender)
                .collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.send(ApprovalChoice::Deny);
        }
        Ok(true)
    }

    fn clear_active(&self, task_id: TaskId) {
        let senders = {
            let Ok(mut active) = self.active.lock() else {
                return;
            };
            if active.as_ref().is_none_or(|task| task.id != task_id) {
                return;
            }
            let Some(task) = active.take() else { return };
            task.cancellation.cancel();
            task.approvals
                .into_values()
                .map(|approval| approval.sender)
                .collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.send(ApprovalChoice::Deny);
        }
    }

    fn join_previous(&self) {
        let handle = self
            .task_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(handle) = handle
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
    }

    pub(crate) fn shutdown(&self, worker: &InferenceWorker) {
        let senders = {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(task) = active.as_mut() {
                task.cancellation.cancel();
                std::mem::take(&mut task.approvals)
                    .into_values()
                    .map(|approval| approval.sender)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        for sender in senders {
            let _ = sender.send(ApprovalChoice::Deny);
        }
        let _ = worker.cancel();
        self.join_previous();
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalTarget {
    pub display_name: String,
    pub desktop_file: String,
    pub executable: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    Started {
        task_id: String,
        task_timeout_ms: u64,
    },
    StateChanged {
        task_id: String,
        state: TaskState,
    },
    ToolProposed {
        task_id: String,
        tool: ToolId,
    },
    ProposalRejected {
        task_id: String,
        code: AgentErrorCode,
    },
    DispatchStarted {
        task_id: String,
        operation_id: String,
        tool: ToolId,
    },
    ApprovalRequired {
        task_id: String,
        operation_id: String,
        tool: ToolId,
        expires_in_ms: u64,
        target: Option<ApprovalTarget>,
    },
    Execution {
        task_id: String,
        record: ExecutionRecord,
    },
    Observation {
        task_id: String,
        record: ExecutionRecord,
        observation: ToolObservation,
    },
    Finished {
        task_id: String,
        state: TaskState,
        usage: BudgetUsage,
        answer: Option<String>,
        error: Option<AgentError>,
        executions: Vec<ExecutionRecord>,
        native_results: Vec<NativeResult>,
        journal_incomplete: bool,
    },
    Failed {
        task_id: String,
        error: AgentError,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeResult {
    pub record: ExecutionRecord,
    pub observation: ToolObservation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentScopeDisplay {
    id: String,
    kind: ScopeKind,
    display_name: String,
    access: &'static str,
    state: ScopeState,
    remaining_ms: u64,
}

impl AgentScopeDisplay {
    fn new(scope: ScopeSummary, id: String) -> Self {
        Self {
            id,
            kind: scope.kind,
            display_name: scope.display_name.unwrap_or_else(|| match scope.kind {
                ScopeKind::File => "Selected file".into(),
                ScopeKind::Directory => "Selected folder".into(),
            }),
            access: "read_only",
            state: scope.state,
            remaining_ms: scope.remaining_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskSummary {
    pub id: String,
    pub state: TaskState,
    pub created_at: i64,
    pub updated_at: i64,
    pub finalized: bool,
    pub usage: Option<Value>,
    pub error: Option<String>,
    pub error_message: Option<&'static str>,
    pub conversation_id: Option<ConversationId>,
    pub turn_id: Option<String>,
    pub excluded_turns: Option<usize>,
}

impl From<StoredTask> for AgentTaskSummary {
    fn from(task: StoredTask) -> Self {
        let (error, error_message) = saved_task_error(task.state, task.error);
        Self {
            id: task.id.get().to_string(),
            state: task.state,
            created_at: task.created_at,
            updated_at: task.updated_at,
            finalized: task.finalized,
            usage: task.usage,
            error,
            error_message,
            conversation_id: task.conversation_id,
            turn_id: task.turn_id.map(|id| id.to_string()),
            excluded_turns: task.excluded_turns,
        }
    }
}

/// Restore user-facing copy from fixed codes only. Unknown stored strings
/// are never displayed or copied through as an error code.
fn saved_task_error(
    state: TaskState,
    code: Option<String>,
) -> (Option<String>, Option<&'static str>) {
    use AgentErrorCode::*;
    let Some(code) = code else {
        let message = match state {
            TaskState::Interrupted => "This Agent task was interrupted before it finished.",
            TaskState::Cancelled => "The Agent task was cancelled.",
            TaskState::Denied => "Access for this operation was denied.",
            TaskState::Failed => "The Agent task could not finish.",
            TaskState::TimedOut => "The Agent task ran out of time.",
            _ => return (None, None),
        };
        return (None, Some(message));
    };
    let parsed = serde_json::from_value::<AgentErrorCode>(Value::String(code.clone()));
    let Ok(parsed) = parsed else {
        return (
            Some("unknown".into()),
            Some("The Agent task could not finish."),
        );
    };
    let message = match parsed {
        InvalidLimits => "The Agent task limits are invalid.",
        InvalidTransition => "The Agent task could not advance to its next state.",
        BudgetExhausted => "The Agent task reached its limit for decisions or operations.",
        TimedOut => "The Agent task ran out of time.",
        Cancelled => "The Agent task was cancelled.",
        InvalidProposal => {
            "The model returned an invalid Agent decision. Review any native results before retrying."
        }
        UnknownTool => "The requested tool is unavailable.",
        InvalidArguments => "The proposed tool arguments were invalid.",
        PermissionDenied => {
            "Access was denied. Select the resource or approve the exact action if needed."
        }
        ScopeKindMismatch => "The tool arguments did not match the selected file or folder.",
        ApprovalDeclined => "You declined the application launch.",
        ApprovalExpired => "The application approval expired.",
        ToolFailed => "A local tool could not complete the operation. Review the execution record.",
        ModelFailed => "The local model could not finish the Agent decision.",
        ModelContextLimit => "The task and tool context do not fit the loaded model's context.",
        DecisionOutputLimit => "The model ran out of tokens for its Agent decision.",
        StorageFailed => "The local Agent journal is unavailable.",
        UnsupportedPlatform => "This local capability is unavailable on this platform.",
        Busy => "Another chat or Agent operation is active.",
        PdfEncrypted => "This PDF is encrypted or password-protected.",
        PdfNoText => "The requested PDF pages have no extractable text layer.",
        PdfInvalid => "The selected PDF is invalid or corrupted.",
        PdfUnsupported => "The PDF uses an unsupported feature.",
        PdfLimit => "The PDF exceeded the limits for local extraction.",
        PdfUnavailable => "Isolated PDF extraction is unavailable on this system.",
        KnowledgeLimit => "The local knowledge limit was reached.",
        KnowledgeNotFound => "The local knowledge document was not found.",
        EmbeddingUnavailable => "The local embedding model is unavailable.",
        EmbeddingIncompatible => "The embedding model or vectors are incompatible.",
        EmbeddingIndexUnavailable => "The embedding index is unavailable.",
        KnowledgeRevisionChanged => "The local knowledge document changed.",
    };
    (Some(code), Some(message))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskEvent {
    pub sequence: i64,
    pub created_at: i64,
    pub event: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskDetail {
    pub task: AgentTaskSummary,
    pub events: Vec<AgentTaskEvent>,
    pub executions: Vec<StoredExecution>,
    pub audit: Vec<StoredAudit>,
    pub native_results: Vec<NativeResult>,
}

fn lossless_task_id(value: &mut Value) {
    if let Some(id) = value
        .get_mut("transition")
        .and_then(|transition| transition.get_mut("taskId"))
        && let Some(number) = id.as_u64()
    {
        *id = Value::String(number.to_string());
    }
    if let Some(id) = value.get_mut("taskId")
        && let Some(number) = id.as_u64()
    {
        *id = Value::String(number.to_string());
    }
}

impl From<StoredTaskDetail> for AgentTaskDetail {
    fn from(detail: StoredTaskDetail) -> Self {
        let events = detail
            .events
            .into_iter()
            .map(|event: StoredTaskEvent| {
                let mut payload = event.event;
                lossless_task_id(&mut payload);
                AgentTaskEvent {
                    sequence: event.sequence,
                    created_at: event.created_at,
                    event: payload,
                }
            })
            .collect();
        let audit = detail
            .audit
            .into_iter()
            .map(|mut item: StoredAudit| {
                lossless_task_id(&mut item.record);
                if let Some(record) = item.record.as_object_mut() {
                    record.remove("scopeId");
                }
                item
            })
            .collect();
        Self {
            task: detail.task.into(),
            events,
            executions: detail.executions,
            audit,
            native_results: Vec::new(),
        }
    }
}

struct ThreadWake(thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn wait_for<T>(pending: Pending<T>) -> std::result::Result<T, InferenceError> {
    let mut future = Box::pin(pending);
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match Pin::as_mut(&mut future).poll(&mut context) {
            Poll::Ready(Ok(result)) => return result,
            Poll::Ready(Err(_)) => {
                return Err(InferenceError::new(
                    ErrorCode::WorkerStopped,
                    "The inference worker stopped.",
                ));
            }
            Poll::Pending => thread::park(),
        }
    }
}

struct DesktopAgentHost {
    app: AppHandle,
    worker: Arc<InferenceWorker>,
    database: Arc<Mutex<SqliteStore>>,
    session: Arc<AgentSession>,
    task_timeout_ms: u64,
    native_results: Vec<NativeResult>,
    turn: Option<AgentTurnLink>,
    history: PreviousTurns,
    omitted_by_storage: usize,
    excluded_turns: usize,
    proposed_scope: Option<String>,
    cached_results: Vec<CachedResult>,
}

impl DesktopAgentHost {
    fn emit(&self, event: AgentEvent) -> bool {
        emit_turn(&self.app, self.turn, self.excluded_turns, event)
    }
}

impl RuntimeHost for DesktopAgentHost {
    fn decide(
        &mut self,
        mut request: agent_core::agent::DecisionRequest,
        cancellation: Cancellation,
    ) -> agent_core::inference::Result<agent_core::agent::DecisionResponse> {
        if let Some(turn) = self.turn {
            let scopes = self
                .session
                .policy
                .lock()
                .map_err(|_| context_unavailable())?
                .scopes();
            let cache = self
                .session
                .conversation_results
                .lock()
                .map_err(|_| context_unavailable())?;
            apply_context(&mut request, turn, &self.history, &scopes, &cache);
        }
        let pending = self.worker.decide(request, cancellation)?;
        let response = wait_for(pending)?;
        let excluded = self
            .omitted_by_storage
            .saturating_add(response.stats.excluded_turns);
        if excluded > self.excluded_turns {
            if let Some(turn) = self.turn {
                self.database
                    .lock()
                    .map_err(|_| context_unavailable())?
                    .record_agent_excluded_turns(turn.task_id, excluded)
                    .map_err(|_| context_unavailable())?;
            }
            self.excluded_turns = excluded;
        }
        Ok(response)
    }

    fn approve(
        &mut self,
        request: &ApprovalRequest,
        cancellation: Cancellation,
        wait: Duration,
    ) -> AgentResult<ApprovalChoice> {
        let deadline = Instant::now() + wait;
        #[cfg(target_os = "linux")]
        let target = {
            let app_id = match request.call.arguments() {
                ToolArguments::AppLaunch(args) => &args.app_id,
                _ => {
                    return Err(AgentError::new(
                        AgentErrorCode::PermissionDenied,
                        "The requested action has no trusted approval target.",
                    ));
                }
            };
            let target = self.session.tools.applications().approval_target(app_id)?;
            Some(ApprovalTarget {
                display_name: target.display_name,
                desktop_file: target.desktop_path.to_string_lossy().into_owned(),
                executable: target.executable.to_string_lossy().into_owned(),
            })
        };
        #[cfg(not(target_os = "linux"))]
        let target: Option<ApprovalTarget> = None;

        let (sender, receiver) = mpsc::sync_channel(1);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || cancellation.is_deadline_elapsed() {
            return Err(AgentError::new(
                AgentErrorCode::ApprovalExpired,
                "The approval window closed before the application was authorized.",
            ));
        }
        self.session.add_approval(
            request.task_id,
            &request.operation_id,
            sender,
            deadline,
            target.clone(),
            request.call.tool(),
        )?;
        if !self.emit(AgentEvent::ApprovalRequired {
            task_id: request.task_id.get().to_string(),
            operation_id: request.operation_id.clone(),
            tool: request.call.tool(),
            expires_in_ms: remaining.as_millis() as u64,
            target,
        }) {
            self.session
                .remove_approval(request.task_id, &request.operation_id);
            return Err(AgentError::new(
                AgentErrorCode::ToolFailed,
                "The approval request could not be delivered to the desktop.",
            ));
        }

        let choice = loop {
            if cancellation.is_explicitly_cancelled() {
                self.session
                    .remove_approval(request.task_id, &request.operation_id);
                return Err(cancelled_error());
            }
            if cancellation.is_deadline_elapsed() {
                self.session
                    .remove_approval(request.task_id, &request.operation_id);
                return Err(AgentError::new(
                    AgentErrorCode::ApprovalExpired,
                    "The approval window closed before the application was authorized.",
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.session
                    .remove_approval(request.task_id, &request.operation_id);
                return Err(AgentError::new(
                    AgentErrorCode::ApprovalExpired,
                    "The approval window closed before the application was authorized.",
                ));
            }
            match receiver.recv_timeout(remaining.min(APPROVAL_POLL)) {
                Ok(choice) => break choice,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.session
                        .remove_approval(request.task_id, &request.operation_id);
                    return Err(cancelled_error());
                }
            }
        };
        self.session
            .remove_approval(request.task_id, &request.operation_id);
        Ok(choice)
    }

    fn execute(
        &mut self,
        registry: &ToolRegistry,
        permit: agent_core::agent::AuthorizedCall,
    ) -> AgentResult<ToolObservation> {
        self.session.tools.execute(registry, permit)
    }

    fn persist_audit(&mut self, namespace: &str, batch: &PermissionAuditBatch) -> AgentResult<()> {
        let mut database = self.database.lock().map_err(|_| storage_error())?;
        database.persist_permission_audit(namespace, batch)
    }

    fn record(&mut self, id: TaskId, event: &RuntimeEvent) -> AgentResult<()> {
        {
            let mut database = self.database.lock().map_err(|_| storage_error())?;
            database.record_agent_event(id, event)?;
        }
        self.session.record_progress(id, event)?;
        let task_id = id.get().to_string();
        match event {
            RuntimeEvent::Started => {
                self.emit(AgentEvent::Started {
                    task_id,
                    task_timeout_ms: self.task_timeout_ms,
                });
            }
            RuntimeEvent::Transition { transition } => {
                self.emit(AgentEvent::StateChanged {
                    task_id,
                    state: transition.to,
                });
            }
            RuntimeEvent::Proposed { .. } => {}
            RuntimeEvent::RejectedProposal { code } => {
                self.emit(AgentEvent::ProposalRejected {
                    task_id,
                    code: *code,
                });
            }
            RuntimeEvent::DispatchIntent { operation_id, tool } => {
                self.emit(AgentEvent::DispatchStarted {
                    task_id,
                    operation_id: operation_id.clone(),
                    tool: *tool,
                });
            }
            RuntimeEvent::Execution { record } => {
                self.emit(AgentEvent::Execution {
                    task_id,
                    record: record.clone(),
                });
            }
            RuntimeEvent::Finished { .. } => {}
        }
        Ok(())
    }

    fn finish(
        &mut self,
        id: TaskId,
        event: &RuntimeEvent,
        answer: Option<&str>,
    ) -> AgentResult<()> {
        let answer = answer.map(|answer| redact_scope_ids(answer.to_owned(), &self.session));
        self.database
            .lock()
            .map_err(|_| storage_error())?
            .record_agent_event_with_answer(id, event, answer.as_deref())
    }

    fn proposed(&mut self, id: TaskId, call: &agent_core::agent::ValidatedCall) {
        self.proposed_scope = call.scope_id().map(str::to_owned);
        let _ = self.emit(AgentEvent::ToolProposed {
            task_id: id.get().to_string(),
            tool: call.tool(),
        });
    }

    fn observed(&mut self, id: TaskId, record: &ExecutionRecord, observation: &ToolObservation) {
        if record.status == ExecutionStatus::Succeeded {
            let native = NativeResult {
                record: record.clone(),
                observation: observation.clone(),
            };
            if self.turn.is_some() {
                self.cached_results.push(CachedResult {
                    native: native.clone(),
                    scope_id: self.proposed_scope.clone(),
                });
            }
            self.native_results.push(native);
        }
        let _ = self.emit(AgentEvent::Observation {
            task_id: id.get().to_string(),
            record: record.clone(),
            observation: observation.clone(),
        });
    }
}

struct TaskCleanup {
    id: TaskId,
    database: Arc<Mutex<SqliteStore>>,
    session: Arc<AgentSession>,
}

impl Drop for TaskCleanup {
    fn drop(&mut self) {
        if let Ok(mut policy) = self.session.policy.lock() {
            let _ = policy.flush_audit(|namespace, batch| {
                let mut database = self.database.lock().map_err(|_| storage_error())?;
                database.persist_permission_audit(namespace, batch)
            });
            #[cfg(target_os = "linux")]
            let _ = self.session.tools.filesystem().prune(&policy);
        }
        if let Ok(mut database) = self.database.lock() {
            let _ = database.interrupt_agent_task(self.id);
        }
        self.session.clear_active(self.id);
    }
}

#[tauri::command]
pub fn start_agent_task(
    app: AppHandle,
    state: State<'_, Desktop>,
    prompt: String,
    task_timeout_seconds: Option<u32>,
) -> AgentResult<String> {
    start_task(app, state, prompt, task_timeout_seconds, None).map(|(id, _)| id.get().to_string())
}

#[tauri::command]
pub fn start_agent_turn(
    app: AppHandle,
    state: State<'_, Desktop>,
    conversation_id: ConversationId,
    prompt: String,
    task_timeout_seconds: Option<u32>,
) -> AgentResult<AgentTurnIdentity> {
    let (_, turn) = start_task(
        app,
        state,
        prompt,
        task_timeout_seconds,
        Some(conversation_id),
    )?;
    turn.map(Into::into).ok_or_else(storage_error)
}

fn start_task(
    app: AppHandle,
    state: State<'_, Desktop>,
    prompt: String,
    task_timeout_seconds: Option<u32>,
    conversation: Option<ConversationId>,
) -> AgentResult<(TaskId, Option<AgentTurnLink>)> {
    if prompt.trim().is_empty() || prompt.len() > 8192 || prompt.contains('\0') {
        return Err(invalid_error());
    }
    let limits = task_timeout_seconds
        .map(AgentLimits::with_task_timeout_seconds)
        .transpose()?
        .unwrap_or_default();
    let operation = state.engine.reserve().map_err(|_| busy_error())?;
    state.agent.join_previous();
    let status = state.engine.worker().status().map_err(map_inference)?;
    let model = status
        .model
        .filter(|model| model.supports_thinking && status.phase == inference::Phase::Ready)
        .ok_or_else(|| {
            AgentError::new(
                AgentErrorCode::ModelFailed,
                "Load a supported local model before starting agent mode.",
            )
        })?;
    let (id, turn, history, omitted_by_storage) = {
        let mut database = state.engine.store().lock().map_err(|_| storage_error())?;
        if let Some(conversation) = conversation {
            let (history, omitted) = database
                .agent_context(conversation)
                .map_err(map_inference)?;
            let turn = database
                .begin_agent_turn(
                    conversation,
                    &prompt,
                    &model.file_name,
                    limits.task_timeout_ms,
                )
                .map_err(map_turn_error)?;
            (turn.task_id, Some(turn), history, omitted)
        } else {
            (database.allocate_agent_task()?, None, Vec::new(), 0)
        }
    };
    let cancellation = Cancellation::default();
    if let Err(error) = state
        .agent
        .activate(id, cancellation.clone(), turn, limits.task_timeout_ms)
    {
        if let Ok(mut database) = state.engine.store().lock() {
            let _ = database.interrupt_agent_task(id);
        }
        return Err(error);
    }

    let session = Arc::clone(&state.agent);
    let worker = Arc::clone(state.engine.worker());
    let database = Arc::clone(state.engine.store());
    let task_prompt = prompt;
    let operation_guard = operation;
    let app_handle = app.clone();
    // Hold the slot lock across spawn and handle publication. A very short
    // task can finish and release the operation lease before spawn returns;
    // publishing under this lock prevents the next task from missing its join handle.
    let mut task_thread = state
        .agent
        .task_thread
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let spawned = thread::Builder::new()
        .name("local-agent-task".into())
        .spawn(move || {
            let cleanup = TaskCleanup {
                id,
                database: Arc::clone(&database),
                session: Arc::clone(&session),
            };
            let mut host = DesktopAgentHost {
                app: app_handle.clone(),
                worker,
                database,
                session: Arc::clone(&session),
                task_timeout_ms: limits.task_timeout_ms,
                native_results: Vec::new(),
                turn,
                history,
                omitted_by_storage,
                excluded_turns: omitted_by_storage,
                proposed_scope: None,
                cached_results: Vec::new(),
            };
            let request = TaskRequest {
                id,
                prompt: task_prompt,
                limits,
                decision_tokens: 512,
                cancellation,
            };
            let report = run_task(&session.registry, &session.policy, &mut host, request);
            if let Some(link) = turn {
                if let Ok(mut cache) = session.conversation_results.lock() {
                    cache.remember(CachedTurn {
                        link,
                        results: std::mem::take(&mut host.cached_results),
                    });
                }
            } else if let Ok(mut latest) = session.latest_results.lock() {
                *latest = Some((id, host.native_results.clone()));
            }
            let event = match report {
                Ok(report) => AgentEvent::Finished {
                    task_id: report.task_id.get().to_string(),
                    state: report.state,
                    usage: report.usage,
                    answer: report
                        .answer
                        .map(|answer| redact_scope_ids(answer, &session)),
                    error: report.error,
                    executions: report.executions,
                    native_results: host.native_results,
                    journal_incomplete: report.journal_incomplete,
                },
                Err(error) => AgentEvent::Failed {
                    task_id: id.get().to_string(),
                    error,
                },
            };
            // The terminal event enables the composer in the WebView.
            drop(cleanup);
            drop(operation_guard);
            emit_turn(&app_handle, turn, host.excluded_turns, event);
        });
    match spawned {
        Ok(handle) => *task_thread = Some(handle),
        Err(_) => {
            drop(task_thread);
            state.agent.clear_active(id);
            if let Ok(mut database) = state.engine.store().lock() {
                let _ = database.interrupt_agent_task(id);
            }
            return Err(AgentError::new(
                AgentErrorCode::StorageFailed,
                "The agent worker could not be started.",
            ));
        }
    }
    Ok((id, turn))
}

#[tauri::command]
pub fn cancel_agent_task(state: State<'_, Desktop>, task_id: String) -> AgentResult<bool> {
    let id = parse_task_id(&task_id)?;
    // The decision worker already shares this task's cancellation flag. An
    // unscoped worker cancel could race with the next task or a chat request.
    state.agent.cancel(id)
}

#[tauri::command]
pub fn respond_agent_approval(
    state: State<'_, Desktop>,
    task_id: String,
    operation_id: String,
    approve: bool,
) -> AgentResult<()> {
    let id = parse_task_id(&task_id)?;
    if operation_id.is_empty() || operation_id.len() > 64 {
        return Err(invalid_error());
    }
    state.agent.respond_approval(id, &operation_id, approve)
}

#[tauri::command]
pub fn list_agent_tasks(
    state: State<'_, Desktop>,
    before: Option<String>,
    limit: Option<u32>,
) -> AgentResult<Vec<AgentTaskSummary>> {
    let before = before.as_deref().map(parse_task_id).transpose()?;
    let database = state.engine.store().lock().map_err(|_| storage_error())?;
    database
        .list_agent_tasks(before, limit.unwrap_or(30))
        .map(|tasks| tasks.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub fn get_agent_task(state: State<'_, Desktop>, task_id: String) -> AgentResult<AgentTaskDetail> {
    let id = parse_task_id(&task_id)?;
    let mut detail: AgentTaskDetail = state
        .engine
        .store()
        .lock()
        .map_err(|_| storage_error())?
        .agent_task(id)?
        .into();
    if let Some((_, observations)) = state
        .agent
        .latest_results
        .lock()
        .map_err(|_| storage_error())?
        .as_ref()
        .filter(|(task, _)| *task == id)
    {
        detail.native_results = observations.clone();
    }
    if detail.native_results.is_empty() {
        let scopes = state
            .agent
            .policy
            .lock()
            .map_err(|_| storage_error())?
            .scopes();
        detail.native_results = state
            .agent
            .conversation_results
            .lock()
            .map_err(|_| storage_error())?
            .results(id, &scopes);
    }
    Ok(detail)
}

fn redact_scope_ids(mut answer: String, session: &AgentSession) -> String {
    let references = session
        .grant_refs
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut ids: Vec<_> = references.values().collect();
    ids.sort_unstable_by_key(|id| std::cmp::Reverse(id.len()));
    for id in ids {
        answer = answer.replace(id, "selected resource");
    }
    answer
}

#[cfg(target_os = "linux")]
fn grant_selected(
    state: &Desktop,
    path: &std::path::Path,
    kind: ScopeKind,
) -> AgentResult<AgentScopeDisplay> {
    let mut policy = state.agent.policy.lock().map_err(|_| storage_error())?;
    let mut references = state.agent.grant_refs.lock().map_err(|_| storage_error())?;
    let scope =
        state
            .agent
            .tools
            .filesystem()
            .grant_selected(&mut policy, path, kind, SCOPE_LIFETIME)?;
    let persisted = policy.flush_audit(|namespace, batch| {
        let mut database = state.engine.store().lock().map_err(|_| storage_error())?;
        database.persist_permission_audit(namespace, batch)
    });
    if let Err(error) = persisted {
        let _ = state
            .agent
            .tools
            .filesystem()
            .revoke(&mut policy, &scope.id);
        let _ = policy.flush_audit(|namespace, batch| {
            let mut database = state.engine.store().lock().map_err(|_| storage_error())?;
            database.persist_permission_audit(namespace, batch)
        });
        return Err(error);
    }
    let id = format!(
        "grant-{}-{}",
        state.agent.grant_namespace,
        references.len() + 1
    );
    references.insert(id.clone(), scope.id.clone());
    Ok(AgentScopeDisplay::new(scope, id))
}

#[tauri::command]
pub async fn choose_agent_file(
    state: State<'_, Desktop>,
) -> AgentResult<Option<AgentScopeDisplay>> {
    let _operation = state.engine.reserve().map_err(|_| busy_error())?;
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Grant read access to one file")
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    #[cfg(target_os = "linux")]
    {
        let path = file.path().to_path_buf();
        grant_selected(&state, &path, ScopeKind::File).map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(AgentError::new(
            AgentErrorCode::UnsupportedPlatform,
            "Scoped file access is supported only on Linux.",
        ))
    }
}

#[tauri::command]
pub async fn choose_agent_directory(
    state: State<'_, Desktop>,
) -> AgentResult<Option<AgentScopeDisplay>> {
    let _operation = state.engine.reserve().map_err(|_| busy_error())?;
    let Some(directory) = rfd::AsyncFileDialog::new()
        .set_title("Grant read access to one directory")
        .pick_folder()
        .await
    else {
        return Ok(None);
    };
    #[cfg(target_os = "linux")]
    {
        let path = directory.path().to_path_buf();
        grant_selected(&state, &path, ScopeKind::Directory).map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = directory;
        Err(AgentError::new(
            AgentErrorCode::UnsupportedPlatform,
            "Scoped directory access is supported only on Linux.",
        ))
    }
}

#[tauri::command]
pub fn list_agent_scopes(state: State<'_, Desktop>) -> AgentResult<Vec<AgentScopeDisplay>> {
    let policy = state.agent.policy.lock().map_err(|_| storage_error())?;
    #[cfg(target_os = "linux")]
    state.agent.tools.filesystem().prune(&policy)?;
    let references = state.agent.grant_refs.lock().map_err(|_| storage_error())?;
    Ok(policy
        .scopes()
        .into_iter()
        .filter_map(|scope| {
            let (view_id, _) = references
                .iter()
                .find(|(_, internal)| **internal == scope.id)?;
            Some(AgentScopeDisplay::new(scope, view_id.clone()))
        })
        .collect())
}

/// Revocation deliberately does not reserve the global operation gate:
/// the user must be able to revoke a resource while a task is using it.
#[tauri::command]
pub fn revoke_agent_scope(state: State<'_, Desktop>, scope_id: String) -> AgentResult<bool> {
    if scope_id.is_empty() || scope_id.len() > 64 {
        return Err(invalid_error());
    }
    let mut policy = state.agent.policy.lock().map_err(|_| storage_error())?;
    let references = state.agent.grant_refs.lock().map_err(|_| storage_error())?;
    let Some(internal_id) = references.get(&scope_id) else {
        return Ok(false);
    };
    #[cfg(target_os = "linux")]
    let revoked = state
        .agent
        .tools
        .filesystem()
        .revoke(&mut policy, internal_id)?;
    #[cfg(not(target_os = "linux"))]
    let revoked = policy.revoke_scope(internal_id);
    if revoked {
        state
            .agent
            .conversation_results
            .lock()
            .map_err(|_| storage_error())?
            .revoke(internal_id);
        policy.flush_audit(|namespace, batch| {
            let mut database = state.engine.store().lock().map_err(|_| storage_error())?;
            database.persist_permission_audit(namespace, batch)
        })?;
    }
    Ok(revoked)
}
