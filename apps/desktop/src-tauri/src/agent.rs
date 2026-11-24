//! Trusted Tauri bridge for the local agent runtime.
//! Native work runs on a dedicated task thread. The existing inference
//! worker remains the sole owner of the model and serializes agent decisions
//! with ordinary chat generation.
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    pin::Pin,
    sync::{Arc, Mutex, mpsc::{self, RecvTimeoutError, SyncSender}},
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use agent_core::{
    agent::{
        AgentError, AgentErrorCode, AgentLimits, AgentResult, AgentStore, ApprovalChoice,
        ApprovalRequest, BudgetUsage, ExecutionRecord, PermissionAuditBatch, PermissionEngine,
        RuntimeEvent, RuntimeHost, ScopeKind, ScopeSummary, StoredAudit, StoredExecution,
        StoredTask, StoredTaskDetail, StoredTaskEvent, TaskId, TaskReport, TaskRequest,
        TaskState, ToolArguments, ToolId, ToolObservation, ToolRegistry, run_task,
    },
    inference::{Cancellation, ErrorCode, InferenceError},
};
use agent_tools::{NativeTools, supported_registry};
use inference::{InferenceWorker, Pending};
use serde::Serialize;
use serde_json::Value;
use storage::SqliteStore;
use tauri::{AppHandle, Emitter, State};

use crate::commands::Desktop;

const AGENT_EVENT: &str = "agent-event";
const SCOPE_LIFETIME: Duration = Duration::from_secs(60 * 60);
const APPROVAL_POLL: Duration = Duration::from_millis(100);

fn storage_error() -> AgentError {
    AgentError::new(AgentErrorCode::StorageFailed, "The local agent journal is unavailable.")
}
fn busy_error() -> AgentError {
    AgentError::new(AgentErrorCode::Busy, "Another chat or agent operation is active.")
}
fn invalid_error() -> AgentError {
    AgentError::new(AgentErrorCode::InvalidArguments, "The agent request is invalid.")
}
fn cancelled_error() -> AgentError {
    AgentError::new(AgentErrorCode::Cancelled, "The agent task was cancelled.")
}
fn unavailable_error() -> AgentError {
    AgentError::new(AgentErrorCode::ToolFailed, "The requested local capability is unavailable.")
}
fn map_inference(_: InferenceError) -> AgentError {
    unavailable_error()
}
fn parse_task_id(value: &str) -> AgentResult<TaskId> {
    value.parse::<u64>().ok().and_then(TaskId::new).ok_or_else(invalid_error)
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
    approvals: HashMap<String, SyncSender<ApprovalChoice>>,
}

/// One ephemeral permission namespace and one native resource catalog per
/// application session. No scopes or approvals are restored from SQLite.
pub struct AgentSession {
    pub(crate) policy: Mutex<PermissionEngine>,
    pub(crate) registry: ToolRegistry,
    pub(crate) tools: NativeTools,
    active: Mutex<Option<ActiveAgent>>,
    task_thread: Mutex<Option<JoinHandle<()>>>,
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
        })
    }

    fn activate(&self, id: TaskId, cancellation: Cancellation) -> AgentResult<()> {
        let mut active = self.active.lock().map_err(|_| storage_error())?;
        if active.is_some() {
            return Err(busy_error());
        }
        *active = Some(ActiveAgent { id, cancellation, approvals: HashMap::new() });
        Ok(())
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.lock().map_or(true, |active| active.is_some())
    }

    fn add_approval(
        &self,
        task_id: TaskId,
        operation_id: &str,
        sender: SyncSender<ApprovalChoice>,
    ) -> AgentResult<()> {
        let mut active = self.active.lock().map_err(|_| storage_error())?;
        let task = active.as_mut().filter(|task| task.id == task_id).ok_or_else(cancelled_error)?;
        if task.approvals.contains_key(operation_id) {
            return Err(busy_error());
        }
        task.approvals.insert(operation_id.to_owned(), sender);
        Ok(())
    }

    fn remove_approval(&self, task_id: TaskId, operation_id: &str) {
        if let Ok(mut active) = self.active.lock()
            && let Some(task) = active.as_mut().filter(|task| task.id == task_id)
        {
            task.approvals.remove(operation_id);
        }
    }

    fn respond_approval(&self, task_id: TaskId, operation_id: &str, approve: bool) -> AgentResult<()> {
        let sender = {
            let mut active = self.active.lock().map_err(|_| storage_error())?;
            let task = active.as_mut().filter(|task| task.id == task_id).ok_or_else(cancelled_error)?;
            task.approvals.remove(operation_id).ok_or_else(|| {
                AgentError::new(AgentErrorCode::PermissionDenied, "This approval is no longer pending.")
            })?
        };
        sender.send(if approve { ApprovalChoice::AllowOnce } else { ApprovalChoice::Deny })
            .map_err(|_| cancelled_error())
    }

    fn cancel(&self, task_id: TaskId) -> AgentResult<bool> {
        let senders = {
            let mut active = self.active.lock().map_err(|_| storage_error())?;
            let Some(task) = active.as_mut().filter(|task| task.id == task_id) else {
                return Ok(false);
            };
            task.cancellation.cancel();
            std::mem::take(&mut task.approvals).into_values().collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.send(ApprovalChoice::Deny);
        }
        Ok(true)
    }

    fn clear_active(&self, task_id: TaskId) {
        let senders = {
            let Ok(mut active) = self.active.lock() else { return };
            if active.as_ref().is_none_or(|task| task.id != task_id) {
                return;
            }
            let Some(task) = active.take() else { return };
            task.cancellation.cancel();
            task.approvals.into_values().collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.send(ApprovalChoice::Deny);
        }
    }

    fn join_previous(&self) {
        let handle = self.task_thread.lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(handle) = handle
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
    }

    fn store_thread(&self, handle: JoinHandle<()>) {
        let mut slot = self.task_thread.lock().unwrap_or_else(|error| error.into_inner());
        *slot = Some(handle);
    }

    pub(crate) fn shutdown(&self, worker: &InferenceWorker) {
        let senders = {
            let mut active = self.active.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(task) = active.as_mut() {
                task.cancellation.cancel();
                std::mem::take(&mut task.approvals).into_values().collect::<Vec<_>>()
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    Started { task_id: String },
    StateChanged { task_id: String, state: TaskState },
    ToolProposed { task_id: String, tool: ToolId },
    ProposalRejected { task_id: String, code: AgentErrorCode },
    DispatchStarted { task_id: String, operation_id: String, tool: ToolId },
    ApprovalRequired {
        task_id: String,
        operation_id: String,
        tool: ToolId,
        expires_in_ms: u64,
        target: Option<ApprovalTarget>,
    },
    Execution { task_id: String, record: ExecutionRecord },
    Observation { task_id: String, record: ExecutionRecord, observation: ToolObservation },
    Finished {
        task_id: String,
        state: TaskState,
        usage: BudgetUsage,
        answer: Option<String>,
        error: Option<AgentError>,
        executions: Vec<ExecutionRecord>,
        journal_incomplete: bool,
    },
    Failed { task_id: String, error: AgentError },
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
}

impl From<StoredTask> for AgentTaskSummary {
    fn from(task: StoredTask) -> Self {
        Self {
            id: task.id.get().to_string(),
            state: task.state,
            created_at: task.created_at,
            updated_at: task.updated_at,
            finalized: task.finalized,
            usage: task.usage,
            error: task.error,
        }
    }
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
}

fn lossless_task_id(value: &mut Value) {
    if let Some(id) = value.get_mut("transition").and_then(|transition| transition.get_mut("taskId"))
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
        let events = detail.events.into_iter().map(|event: StoredTaskEvent| {
            let mut payload = event.event;
            lossless_task_id(&mut payload);
            AgentTaskEvent { sequence: event.sequence, created_at: event.created_at, event: payload }
        }).collect();
        let audit = detail.audit.into_iter().map(|mut item: StoredAudit| {
            lossless_task_id(&mut item.record);
            item
        }).collect();
        Self { task: detail.task.into(), events, executions: detail.executions, audit }
    }
}

struct ThreadWake(thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) { self.0.unpark(); }
    fn wake_by_ref(self: &Arc<Self>) { self.0.unpark(); }
}

fn wait_for<T>(pending: Pending<T>) -> std::result::Result<T, InferenceError> {
    let mut future = Box::pin(pending);
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match Pin::as_mut(&mut future).poll(&mut context) {
            Poll::Ready(Ok(result)) => return result,
            Poll::Ready(Err(_)) => {
                return Err(InferenceError::new(ErrorCode::WorkerStopped, "The inference worker stopped."));
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
}

impl RuntimeHost for DesktopAgentHost {
    fn decide(
        &mut self,
        request: agent_core::agent::DecisionRequest,
        cancellation: Cancellation,
    ) -> agent_core::inference::Result<agent_core::agent::DecisionResponse> {
        let pending = self.worker.decide(request, cancellation)?;
        wait_for(pending)
    }

    fn approve(
        &mut self,
        request: &ApprovalRequest,
        cancellation: Cancellation,
        wait: Duration,
    ) -> AgentResult<ApprovalChoice> {
        #[cfg(target_os = "linux")]
        let target = {
            let app_id = match request.call.arguments() {
                ToolArguments::AppLaunch(args) => &args.app_id,
                _ => return Err(AgentError::new(AgentErrorCode::PermissionDenied, "The requested action has no trusted approval target.")),
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
        self.session.add_approval(request.task_id, &request.operation_id, sender)?;
        if !emit(&self.app, AgentEvent::ApprovalRequired {
            task_id: request.task_id.get().to_string(),
            operation_id: request.operation_id.clone(),
            tool: request.call.tool(),
            expires_in_ms: request.expires_in_ms,
            target,
        }) {
            self.session.remove_approval(request.task_id, &request.operation_id);
            return Err(AgentError::new(AgentErrorCode::ToolFailed, "The approval request could not be delivered to the desktop."));
        }

        let deadline = Instant::now().checked_add(wait).unwrap_or_else(Instant::now);
        let choice = loop {
            if cancellation.is_cancelled() {
                self.session.remove_approval(request.task_id, &request.operation_id);
                return Err(cancelled_error());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.session.remove_approval(request.task_id, &request.operation_id);
                return Err(AgentError::new(AgentErrorCode::TimedOut, "The approval deadline expired."));
            }
            match receiver.recv_timeout(remaining.min(APPROVAL_POLL)) {
                Ok(choice) => break choice,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.session.remove_approval(request.task_id, &request.operation_id);
                    return Err(cancelled_error());
                }
            }
        };
        self.session.remove_approval(request.task_id, &request.operation_id);
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
        let task_id = id.get().to_string();
        match event {
            RuntimeEvent::Started => { emit(&self.app, AgentEvent::Started { task_id }); }
            RuntimeEvent::Transition { transition } => { emit(&self.app, AgentEvent::StateChanged {
                task_id, state: transition.to,
            }); }
            RuntimeEvent::Proposed { .. } => {},
            RuntimeEvent::RejectedProposal { code } => { emit(&self.app, AgentEvent::ProposalRejected {
                task_id, code: *code,
            }); }
            RuntimeEvent::DispatchIntent { operation_id, tool } => { emit(&self.app, AgentEvent::DispatchStarted {
                task_id, operation_id: operation_id.clone(), tool: *tool,
            }); }
            RuntimeEvent::Execution { record } => { emit(&self.app, AgentEvent::Execution {
                task_id, record: record.clone(),
            }); }
            RuntimeEvent::Finished { .. } => {}
        }
        Ok(())
    }

    fn proposed(&mut self, id: TaskId, call: &agent_core::agent::ValidatedCall) {
        let _ = emit(&self.app, AgentEvent::ToolProposed {
            task_id: id.get().to_string(), tool: call.tool(),
        });
    }

    fn observed(&mut self, id: TaskId, record: &ExecutionRecord, observation: &ToolObservation) {
        let _ = emit(&self.app, AgentEvent::Observation {
            task_id: id.get().to_string(), record: record.clone(), observation: observation.clone(),
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
pub fn start_agent_task(app: AppHandle, state: State<'_, Desktop>, prompt: String) -> AgentResult<String> {
    if prompt.trim().is_empty() || prompt.len() > 8192 || prompt.contains('\0') {
        return Err(invalid_error());
    }
    let operation = state.reserve().map_err(|_| busy_error())?;
    state.agent.join_previous();
    let status = state.worker.status().map_err(map_inference)?;
    if status.phase != inference::Phase::Ready
        || status.model.as_ref().is_none_or(|model| !model.supports_thinking)
    {
        return Err(AgentError::new(
            AgentErrorCode::ModelFailed,
            "Load a supported local model before starting agent mode.",
        ));
    }
    let id = {
        let mut database = state.database.lock().map_err(|_| storage_error())?;
        database.allocate_agent_task()?
    };
    let cancellation = Cancellation::default();
    if let Err(error) = state.agent.activate(id, cancellation.clone()) {
        if let Ok(mut database) = state.database.lock() {
            let _ = database.interrupt_agent_task(id);
        }
        return Err(error);
    }

    let session = Arc::clone(&state.agent);
    let worker = Arc::clone(&state.worker);
    let database = Arc::clone(&state.database);
    let task_prompt = prompt;
    let operation_guard = operation;
    let app_handle = app.clone();
    let spawned = thread::Builder::new().name("local-agent-task".into()).spawn(move || {
        let _operation = operation_guard;
        let _cleanup = TaskCleanup {
            id, database: Arc::clone(&database), session: Arc::clone(&session),
        };
        let mut host = DesktopAgentHost {
            app: app_handle.clone(), worker, database, session: Arc::clone(&session),
        };
        let request = TaskRequest {
            id, prompt: task_prompt, limits: AgentLimits::default(),
            decision_tokens: 512, cancellation,
        };
        match run_task(&session.registry, &session.policy, &mut host, request) {
            Ok(report) => { let _ = emit(&app_handle, AgentEvent::Finished {
                task_id: report.task_id.get().to_string(),
                state: report.state,
                usage: report.usage,
                answer: report.answer,
                error: report.error,
                executions: report.executions,
                journal_incomplete: report.journal_incomplete,
            }); }
            Err(error) => { let _ = emit(&app_handle, AgentEvent::Failed {
                task_id: id.get().to_string(), error,
            }); }
        }
    });
    match spawned {
        Ok(handle) => state.agent.store_thread(handle),
        Err(_) => {
            state.agent.clear_active(id);
            if let Ok(mut database) = state.database.lock() {
                let _ = database.interrupt_agent_task(id);
            }
            return Err(AgentError::new(AgentErrorCode::StorageFailed, "The agent worker could not be started."));
        }
    }
    Ok(id.get().to_string())
}

#[tauri::command]
pub fn cancel_agent_task(state: State<'_, Desktop>, task_id: String) -> AgentResult<bool> {
    let id = parse_task_id(&task_id)?;
    let cancelled = state.agent.cancel(id)?;
    if cancelled {
        let _ = state.worker.cancel();
    }
    Ok(cancelled)
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
    let mut database = state.database.lock().map_err(|_| storage_error())?;
    database.list_agent_tasks(before, limit.unwrap_or(30))
        .map(|tasks| tasks.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub fn get_agent_task(state: State<'_, Desktop>, task_id: String) -> AgentResult<AgentTaskDetail> {
    let id = parse_task_id(&task_id)?;
    let database = state.database.lock().map_err(|_| storage_error())?;
    database.agent_task(id).map(Into::into)
}

#[cfg(target_os = "linux")]
fn grant_selected(state: &Desktop, path: &std::path::Path, kind: ScopeKind) -> AgentResult<ScopeSummary> {
    let mut policy = state.agent.policy.lock().map_err(|_| storage_error())?;
    let scope = state.agent.tools.filesystem().grant_selected(&mut policy, path, kind, SCOPE_LIFETIME)?;
    let persisted = policy.flush_audit(|namespace, batch| {
        let mut database = state.database.lock().map_err(|_| storage_error())?;
        database.persist_permission_audit(namespace, batch)
    });
    if let Err(error) = persisted {
        let _ = state.agent.tools.filesystem().revoke(&mut policy, &scope.id);
        let _ = policy.flush_audit(|namespace, batch| {
            let mut database = state.database.lock().map_err(|_| storage_error())?;
            database.persist_permission_audit(namespace, batch)
        });
        return Err(error);
    }
    Ok(scope)
}

#[tauri::command]
pub async fn choose_agent_file(state: State<'_, Desktop>) -> AgentResult<Option<ScopeSummary>> {
    let _operation = state.reserve().map_err(|_| busy_error())?;
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Grant read access to one file")
        .pick_file().await else { return Ok(None) };
    #[cfg(target_os = "linux")]
    {
        let path = file.path().to_path_buf();
        grant_selected(&state, &path, ScopeKind::File).map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(AgentError::new(AgentErrorCode::UnsupportedPlatform, "Scoped file access is supported only on Linux."))
    }
}

#[tauri::command]
pub async fn choose_agent_directory(state: State<'_, Desktop>) -> AgentResult<Option<ScopeSummary>> {
    let _operation = state.reserve().map_err(|_| busy_error())?;
    let Some(directory) = rfd::AsyncFileDialog::new()
        .set_title("Grant read access to one directory")
        .pick_folder().await else { return Ok(None) };
    #[cfg(target_os = "linux")]
    {
        let path = directory.path().to_path_buf();
        grant_selected(&state, &path, ScopeKind::Directory).map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = directory;
        Err(AgentError::new(AgentErrorCode::UnsupportedPlatform, "Scoped directory access is supported only on Linux."))
    }
}

#[tauri::command]
pub fn list_agent_scopes(state: State<'_, Desktop>) -> AgentResult<Vec<ScopeSummary>> {
    let policy = state.agent.policy.lock().map_err(|_| storage_error())?;
    #[cfg(target_os = "linux")]
    state.agent.tools.filesystem().prune(&policy)?;
    Ok(policy.scopes())
}

/// Revocation deliberately does not reserve the global operation gate:
/// the user must be able to revoke a resource while a task is using it.
#[tauri::command]
pub fn revoke_agent_scope(state: State<'_, Desktop>, scope_id: String) -> AgentResult<bool> {
    if scope_id.is_empty() || scope_id.len() > 64 {
        return Err(invalid_error());
    }
    let mut policy = state.agent.policy.lock().map_err(|_| storage_error())?;
    #[cfg(target_os = "linux")]
    let revoked = state.agent.tools.filesystem().revoke(&mut policy, &scope_id)?;
    #[cfg(not(target_os = "linux"))]
    let revoked = policy.revoke_scope(&scope_id);
    if revoked {
        policy.flush_audit(|namespace, batch| {
            let mut database = state.database.lock().map_err(|_| storage_error())?;
            database.persist_permission_audit(namespace, batch)
        })?;
    }
    Ok(revoked)
}
