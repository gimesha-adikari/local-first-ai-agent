//! In-memory, default-deny authorization for the trusted agent runtime.
//! This module never opens files, starts processes, or accepts model approvals.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::inference::Cancellation;

use super::{
    AgentError, AgentErrorCode, AgentResult, PermissionRequirement, TaskId, ToolArguments,
    ToolId, ToolRegistry, ValidatedCall, tool::valid_identifier,
};

const MAX_SCOPES: usize = 128;
const MAX_AUDIT_RECORDS: usize = 1024;
const MAX_SCOPE_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_TASK_LIFETIME: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeState {
    Active,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSummary {
    pub id: String,
    pub kind: ScopeKind,
    pub state: ScopeState,
    pub remaining_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialReason {
    TaskInactive,
    TaskExpired,
    TaskCancelled,
    UnknownTool,
    ScopeMissing,
    ScopeExpired,
    ScopeRevoked,
    ScopeKindMismatch,
    ApprovalRequired,
    ApprovalExpired,
    OperationMismatch,
    UserRejected,
    AuditUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    ScopeGranted,
    ScopeRevoked,
    TaskStarted,
    TaskClosed,
    Allowed,
    ConfirmationRequested,
    Confirmed,
    Denied,
    AuthorizationConsumed,
    AuthorizationDiscarded,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionAudit {
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub task_id: Option<TaskId>,
    pub operation_id: Option<String>,
    pub tool: Option<ToolId>,
    pub scope_id: Option<String>,
    pub action: AuditAction,
    pub reason: Option<DenialReason>,
}

/// Audit records contain metadata only, never arguments, paths, document
/// contents, application command lines, or generated text.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionAuditBatch {
    pub records: Vec<PermissionAudit>,
    pub omitted: u64,
}

#[derive(Debug, Clone)]
struct Lease {
    deadline: Instant,
    revoked: Arc<AtomicBool>,
}

impl Lease {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }

    fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    fn state(&self, now: Instant) -> ScopeState {
        if self.revoked.load(Ordering::Acquire) {
            ScopeState::Revoked
        } else if now >= self.deadline {
            ScopeState::Expired
        } else {
            ScopeState::Active
        }
    }
}

#[derive(Debug)]
struct Grant {
    kind: ScopeKind,
    lease: Lease,
}

#[derive(Debug)]
struct ActiveTask {
    id: TaskId,
    lease: Lease,
    cancellation: Cancellation,
}

impl ActiveTask {
    fn check(&self, now: Instant) -> Result<(), DenialReason> {
        if self.cancellation.is_cancelled() {
            return Err(DenialReason::TaskCancelled);
        }
        match self.lease.state(now) {
            ScopeState::Active => Ok(()),
            ScopeState::Expired => Err(DenialReason::TaskExpired),
            ScopeState::Revoked => Err(DenialReason::TaskInactive),
        }
    }
}

#[derive(Debug)]
struct Operation {
    id: String,
    task_id: TaskId,
    call: ValidatedCall,
    deadline: Instant,
    confirmed: bool,
}

/// For display by trusted host UI only. Approvals are answered using the
/// operation ID, not by submitting replacement arguments.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub operation_id: String,
    pub task_id: TaskId,
    pub call: ValidatedCall,
    pub expires_in_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum AuthorizationDecision {
    Allow { operation_id: String },
    Confirm { request: ApprovalRequest },
    Deny { reason: DenialReason },
}

/// Single-use authorization extracted from the engine by the trusted runtime.
/// Not cloneable or deserializable. The executor must consume this object
/// exactly once and check liveness before and during native operations.
#[derive(Debug)]
pub struct AuthorizedCall {
    operation_id: String,
    task_id: TaskId,
    call: ValidatedCall,
    task_lease: Lease,
    dispatch_deadline: Instant,
    cancellation: Cancellation,
    scope_lease: Option<Lease>,
}

impl AuthorizedCall {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub fn call(&self) -> &ValidatedCall {
        &self.call
    }

    /// Recheck immediately before dispatch, including the original approval
    /// deadline. Once dispatched, ongoing work checks task/scope liveness.
    pub fn check_dispatch(&self) -> AgentResult<()> {
        self.check_live()?;
        if Instant::now() >= self.dispatch_deadline {
            return Err(denied());
        }
        Ok(())
    }

    /// Cooperative check: revocation cannot undo a side effect that has
    /// already happened, or interrupt an uninterruptible native syscall.
    pub fn check_live(&self) -> AgentResult<()> {
        let now = Instant::now();
        if self.cancellation.is_cancelled() {
            return Err(AgentError::new(AgentErrorCode::Cancelled, "The task was cancelled."));
        }
        match self.task_lease.state(now) {
            ScopeState::Active => {}
            ScopeState::Expired => {
                return Err(AgentError::new(AgentErrorCode::TimedOut, "The task deadline expired."));
            }
            ScopeState::Revoked => return Err(denied()),
        }
        if self.scope_lease.as_ref().is_some_and(|lease| lease.state(now) != ScopeState::Active) {
            return Err(denied());
        }
        Ok(())
    }
}

/// One application session, one active task, at most one pending operation.
/// Only trusted Rust host code may create grants or respond to confirmations.
/// The host must supply a fresh unpredictable session namespace on startup,
/// bind scope IDs to immutable native resource handles, and never expose
/// these mutating methods to model-controlled input.
#[derive(Debug)]
pub struct PermissionEngine {
    namespace: String,
    next_id: u64,
    started: Instant,
    scopes: BTreeMap<String, Grant>,
    active: Option<ActiveTask>,
    last_task_id: u64,
    pending: Option<Operation>,
    audit_sequence: u64,
    audit: Vec<PermissionAudit>,
    omitted: u64,
}

impl PermissionEngine {
    pub fn new(namespace: &str) -> AgentResult<Self> {
        if namespace.len() > 32 || !valid_identifier(namespace) {
            return Err(AgentError::new(AgentErrorCode::InvalidArguments, "Invalid permission session namespace."));
        }
        Ok(Self {
            namespace: namespace.into(),
            next_id: 0,
            started: Instant::now(),
            scopes: BTreeMap::new(),
            active: None,
            last_task_id: 0,
            pending: None,
            audit_sequence: 0,
            audit: Vec::new(),
            omitted: 0,
        })
    }

    fn allocate(&mut self, kind: &str) -> AgentResult<String> {
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            AgentError::new(AgentErrorCode::BudgetExhausted, "Permission identifiers are exhausted.")
        })?;
        Ok(format!("{}_{}_{}", self.namespace, kind, self.next_id))
    }

    fn audit_available(&self) -> bool {
        self.audit.len() < MAX_AUDIT_RECORDS && self.audit_sequence < u64::MAX
    }

    fn require_audit(&self) -> AgentResult<()> {
        if self.audit_available() {
            Ok(())
        } else {
            Err(AgentError::new(AgentErrorCode::StorageFailed, "Permission audit buffer must be drained."))
        }
    }

    fn record(
        &mut self,
        task_id: Option<TaskId>,
        operation_id: Option<&str>,
        tool: Option<ToolId>,
        scope_id: Option<&str>,
        action: AuditAction,
        reason: Option<DenialReason>,
    ) {
        if !self.audit_available() {
            self.omitted = self.omitted.saturating_add(1);
            return;
        }
        self.audit_sequence += 1;
        self.audit.push(PermissionAudit {
            sequence: self.audit_sequence,
            elapsed_ms: millis(self.started.elapsed()),
            task_id,
            operation_id: operation_id.map(str::to_owned),
            tool,
            scope_id: scope_id.map(str::to_owned),
            action,
            reason,
        });
    }

    fn record_operation(&mut self, op: &Operation, action: AuditAction, reason: Option<DenialReason>) {
        self.record(Some(op.task_id), Some(&op.id), Some(op.call.tool()), op.call.scope_id(), action, reason);
    }

    /// The host must persist drained records before dispatching any tool.
    /// A persistence failure must stop the task rather than silently proceed.
    /// Revocation and cancellation remain effective even when the buffer fills.
    pub fn drain_audit(&mut self) -> PermissionAuditBatch {
        PermissionAuditBatch {
            records: std::mem::take(&mut self.audit),
            omitted: std::mem::take(&mut self.omitted),
        }
    }

    /// Call only after explicit native picker selection and successful secure
    /// handle acquisition. Register the native handle under the returned ID
    /// before making the scope visible to any model or UI.
    pub fn grant_scope(&mut self, kind: ScopeKind, lifetime: Duration) -> AgentResult<ScopeSummary> {
        self.require_audit()?;
        if lifetime.is_zero() || lifetime > MAX_SCOPE_LIFETIME {
            return Err(AgentError::new(AgentErrorCode::InvalidLimits, "Invalid resource grant lifetime."));
        }
        if self.scopes.len() >= MAX_SCOPES {
            return Err(AgentError::new(AgentErrorCode::BudgetExhausted, "The session resource grant limit is reached."));
        }
        let now = Instant::now();
        let id = self.allocate("scope")?;
        let lease = Lease::new(now + lifetime);
        self.scopes.insert(id.clone(), Grant { kind, lease });
        self.record(None, None, None, Some(&id), AuditAction::ScopeGranted, None);
        Ok(ScopeSummary { id, kind, state: ScopeState::Active, remaining_ms: millis(lifetime) })
    }

    pub fn scopes(&self) -> Vec<ScopeSummary> {
        let now = Instant::now();
        self.scopes.iter().map(|(id, grant)| {
            let state = grant.lease.state(now);
            ScopeSummary {
                id: id.clone(),
                kind: grant.kind,
                state,
                remaining_ms: if state == ScopeState::Active {
                    millis(grant.lease.deadline.saturating_duration_since(now))
                } else {
                    0
                },
            }
        }).collect()
    }

    /// Retain revoked IDs for the session; never rebind an old scope ID.
    pub fn revoke_scope(&mut self, id: &str) -> bool {
        let Some(grant) = self.scopes.get(id) else { return false };
        if grant.lease.state(Instant::now()) == ScopeState::Revoked {
            return false;
        }
        grant.lease.revoke();
        self.record(None, None, None, Some(id), AuditAction::ScopeRevoked, None);
        true
    }

    /// IDs must be monotonically allocated by the trusted host/store. An
    /// old task cannot be reopened to reuse approvals.
    pub fn begin_task(&mut self, id: TaskId, lifetime: Duration, cancellation: Cancellation) -> AgentResult<()> {
        self.require_audit()?;
        if self.active.is_some() || self.pending.is_some() {
            return Err(AgentError::new(AgentErrorCode::Busy, "An agent task is already active."));
        }
        if id.get() <= self.last_task_id || lifetime.is_zero() || lifetime > MAX_TASK_LIFETIME {
            return Err(AgentError::new(AgentErrorCode::InvalidLimits, "Invalid task identity or lifetime."));
        }
        if cancellation.is_cancelled() {
            return Err(AgentError::new(AgentErrorCode::Cancelled, "The task was cancelled."));
        }
        self.last_task_id = id.get();
        self.active = Some(ActiveTask { id, lease: Lease::new(Instant::now() + lifetime), cancellation });
        self.record(Some(id), None, None, None, AuditAction::TaskStarted, None);
        Ok(())
    }

    /// End a task even if the audit buffer is full. This invalidates any
    /// outstanding permits, but does not undo previously dispatched actions.
    pub fn end_task(&mut self, id: TaskId) -> bool {
        if self.active.as_ref().is_none_or(|task| task.id != id) {
            return false;
        }
        if let Some(task) = self.active.take() {
            task.lease.revoke();
        }
        if let Some(op) = self.pending.take() {
            self.record_operation(&op, AuditAction::AuthorizationDiscarded, None);
        }
        self.record(Some(id), None, None, None, AuditAction::TaskClosed, None);
        true
    }

    fn active_task(&self, id: TaskId, now: Instant) -> Result<&ActiveTask, DenialReason> {
        let task = self.active.as_ref().filter(|task| task.id == id).ok_or(DenialReason::TaskInactive)?;
        task.check(now)?;
        Ok(task)
    }

    fn scope_for(&self, call: &ValidatedCall, permission: PermissionRequirement, now: Instant) -> Result<Option<&Grant>, DenialReason> {
        match permission {
            PermissionRequirement::Automatic | PermissionRequirement::ConfirmEveryLaunch => Ok(None),
            PermissionRequirement::DirectoryRead | PermissionRequirement::ResourceRead => {
                let id = call.scope_id().ok_or(DenialReason::ScopeMissing)?;
                let grant = self.scopes.get(id).ok_or(DenialReason::ScopeMissing)?;
                match grant.lease.state(now) {
                    ScopeState::Active => {}
                    ScopeState::Expired => return Err(DenialReason::ScopeExpired),
                    ScopeState::Revoked => return Err(DenialReason::ScopeRevoked),
                }
                let compatible = match (grant.kind, call.arguments()) {
                    (ScopeKind::Directory, ToolArguments::FileSearch(_)) => true,
                    (ScopeKind::Directory, ToolArguments::FileRead(args)) => args.relative_path.is_some(),
                    (ScopeKind::File, ToolArguments::FileRead(args)) => args.relative_path.is_none(),
                    _ => false,
                };
                if compatible {
                    Ok(Some(grant))
                } else {
                    Err(DenialReason::ScopeKindMismatch)
                }
            }
        }
    }

    fn check_call(
        &self, registry: &ToolRegistry, task_id: TaskId, call: &ValidatedCall, now: Instant,
    ) -> Result<PermissionRequirement, DenialReason> {
        self.active_task(task_id, now)?;
        let permission = registry.get(call.tool()).ok_or(DenialReason::UnknownTool)?.permission;
        self.scope_for(call, permission, now)?;
        Ok(permission)
    }

    /// Create one exact operation. Missing scope is a denial, not a request
    /// for unrestricted access. A separate trusted picker flow can grant a
    /// scope, after which the runtime may request a new validated operation.
    pub fn authorize(
        &mut self, registry: &ToolRegistry, task_id: TaskId, call: &ValidatedCall, wait: Duration,
    ) -> AgentResult<AuthorizationDecision> {
        self.require_audit()?;
        if self.pending.is_some() {
            return Err(AgentError::new(AgentErrorCode::Busy, "An authorization is already pending."));
        }
        if wait.is_zero() || wait > MAX_TASK_LIFETIME {
            return Err(AgentError::new(AgentErrorCode::InvalidLimits, "Invalid authorization lifetime."));
        }
        let now = Instant::now();
        let permission = match self.check_call(registry, task_id, call, now) {
            Ok(permission) => permission,
            Err(reason) => {
                self.record(Some(task_id), None, Some(call.tool()), call.scope_id(), AuditAction::Denied, Some(reason));
                return Ok(AuthorizationDecision::Deny { reason });
            }
        };
        let task_deadline = self.active_task(task_id, now).map_err(|_| denied())?.lease.deadline;
        let deadline = (now + wait).min(task_deadline);
        let id = self.allocate("op")?;
        let confirmed = permission != PermissionRequirement::ConfirmEveryLaunch;
        let op = Operation { id: id.clone(), task_id, call: call.clone(), deadline, confirmed };
        let decision = if confirmed {
            self.record_operation(&op, AuditAction::Allowed, None);
            AuthorizationDecision::Allow { operation_id: id }
        } else {
            self.record_operation(&op, AuditAction::ConfirmationRequested, None);
            AuthorizationDecision::Confirm {
                request: ApprovalRequest {
                    operation_id: id,
                    task_id,
                    call: call.clone(),
                    expires_in_ms: millis(deadline.saturating_duration_since(now)),
                },
            }
        };
        self.pending = Some(op);
        Ok(decision)
    }

    /// Accept an explicit human response to the exact pending operation.
    /// No replacement arguments or model-provided approval are accepted.
    pub fn respond(
        &mut self, task_id: TaskId, operation_id: &str, approve: bool,
    ) -> AgentResult<AuthorizationDecision> {
        let op = self.pending.as_ref().ok_or_else(denied)?;
        if op.task_id != task_id || op.id != operation_id || op.confirmed {
            return Err(denied());
        }
        let now = Instant::now();
        let reason = if now >= op.deadline {
            Some(DenialReason::ApprovalExpired)
        } else if let Err(reason) = self.active_task(task_id, now) {
            Some(reason)
        } else if !approve {
            Some(DenialReason::UserRejected)
        } else if !self.audit_available() {
            Some(DenialReason::AuditUnavailable)
        } else {
            None
        };
        let mut op = self.pending.take().ok_or_else(denied)?;
        if let Some(reason) = reason {
            self.record_operation(&op, AuditAction::Denied, Some(reason));
            return Ok(AuthorizationDecision::Deny { reason });
        }
        op.confirmed = true;
        self.record_operation(&op, AuditAction::Confirmed, None);
        let operation_id = op.id.clone();
        self.pending = Some(op);
        Ok(AuthorizationDecision::Allow { operation_id })
    }

    /// Discard a pending authorization on cancellation, timeout or abandoned
    /// UI flow. A mismatched ID cannot discard somebody else's operation.
    pub fn discard(&mut self, task_id: TaskId, operation_id: &str) -> bool {
        if self.pending.as_ref().is_none_or(|op| op.task_id != task_id || op.id != operation_id) {
            return false;
        }
        if let Some(op) = self.pending.take() {
            self.record_operation(&op, AuditAction::AuthorizationDiscarded, None);
        }
        true
    }

    /// Atomically consume the authorization under the host's policy lock.
    /// Recheck registration, task, grant, exact arguments and expiration.
    /// Even a substituted call consumes the matching ticket and fails closed.
    pub fn consume(
        &mut self, registry: &ToolRegistry, task_id: TaskId, operation_id: &str, call: &ValidatedCall,
    ) -> AgentResult<AuthorizedCall> {
        self.require_audit()?;
        let op = self.pending.as_ref().ok_or_else(denied)?;
        if op.id != operation_id || op.task_id != task_id {
            return Err(denied());
        }
        let op = self.pending.take().ok_or_else(denied)?;
        let now = Instant::now();
        let permission = match self.check_call(registry, task_id, call, now) {
            Ok(permission) => permission,
            Err(reason) => {
                self.record_operation(&op, AuditAction::Denied, Some(reason));
                return Err(denied());
            }
        };
        let reason = if op.call != *call {
            Some(DenialReason::OperationMismatch)
        } else if now >= op.deadline {
            Some(DenialReason::ApprovalExpired)
        } else if !op.confirmed {
            Some(DenialReason::ApprovalRequired)
        } else {
            None
        };
        if let Some(reason) = reason {
            self.record_operation(&op, AuditAction::Denied, Some(reason));
            return Err(denied());
        }
        let task = self.active_task(task_id, now).map_err(|_| denied())?;
        let scope_lease = self.scope_for(call, permission, now).map_err(|_| denied())?.map(|grant| grant.lease.clone());
        let permit = AuthorizedCall {
            operation_id: op.id.clone(),
            task_id,
            call: op.call.clone(),
            task_lease: task.lease.clone(),
            dispatch_deadline: op.deadline,
            cancellation: task.cancellation.clone(),
            scope_lease,
        };
        self.record_operation(&op, AuditAction::AuthorizationConsumed, None);
        Ok(permit)
    }
}

impl Drop for PermissionEngine {
    fn drop(&mut self) {
        if let Some(task) = &self.active {
            task.lease.revoke();
        }
        for grant in self.scopes.values() {
            grant.lease.revoke();
        }
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn denied() -> AgentError {
    AgentError::new(AgentErrorCode::PermissionDenied, "The operation is not authorized.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_grant_and_approval_are_rejected_without_sleep() {
        let registry = ToolRegistry::mvp().unwrap();
        let mut policy = PermissionEngine::new("test_session").unwrap();
        let task = TaskId::new(1).unwrap();
        policy.begin_task(task, Duration::from_secs(30), Cancellation::default()).unwrap();
        let grant = policy.grant_scope(ScopeKind::File, Duration::from_secs(30)).unwrap();
        policy.scopes.get_mut(&grant.id).unwrap().lease.deadline = Instant::now();
        let args = serde_json::json!({"scope_id": grant.id}).to_string();
        let read = registry.validate(ToolId::FileRead, &args).unwrap();
        assert!(matches!(
            policy.authorize(&registry, task, &read, Duration::from_secs(10)).unwrap(),
            AuthorizationDecision::Deny { reason: DenialReason::ScopeExpired }
        ));
        let launch = registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_1"}"#).unwrap();
        let AuthorizationDecision::Confirm { request } = policy.authorize(&registry, task, &launch, Duration::from_secs(10)).unwrap() else {
            panic!("expected confirmation");
        };
        policy.pending.as_mut().unwrap().deadline = Instant::now();
        assert!(matches!(
            policy.respond(task, &request.operation_id, true).unwrap(),
            AuthorizationDecision::Deny { reason: DenialReason::ApprovalExpired }
        ));
        assert!(policy.consume(&registry, task, &request.operation_id, &launch).is_err());
    }

    #[test]
    fn full_audit_blocks_new_authority_but_not_revocation() {
        let mut policy = PermissionEngine::new("test_session").unwrap();
        let grant = policy.grant_scope(ScopeKind::File, Duration::from_secs(30)).unwrap();
        while policy.audit_available() {
            policy.record(None, None, None, None, AuditAction::Denied, Some(DenialReason::TaskInactive));
        }
        assert!(policy.grant_scope(ScopeKind::File, Duration::from_secs(30)).is_err());
        assert!(policy.revoke_scope(&grant.id));
        assert_eq!(policy.scopes()[0].state, ScopeState::Revoked);
        assert_eq!(policy.drain_audit().omitted, 1);
    }
}
