//! Bounded, synchronous orchestration for a dedicated host worker.
//! No Tauri, SQLite, llama.cpp or native platform dependencies.

use std::{sync::{Mutex, MutexGuard}, time::Duration};
use serde::Serialize;
use crate::inference::{Cancellation, FinishReason};
use super::{
    AgentBudget, AgentDecision, AgentError, AgentErrorCode, AgentLimits, AgentResult, AgentTask,
    ApprovalRequest, AuthorizationDecision, AuthorizedCall, BudgetUsage, DecisionRequest,
    DecisionResponse, DecisionStep, DenialReason, PermissionAuditBatch, PermissionEngine,
    TaskId, TaskState, TaskTransition, ToolId, ToolObservation, ToolOutcome, ToolRegistry,
    ValidatedCall, parse_decision,
};

#[derive(Debug)]
pub struct TaskRequest {
    pub id: TaskId,
    pub prompt: String,
    pub limits: AgentLimits,
    pub decision_tokens: u32,
    pub cancellation: Cancellation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    AllowOnce,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    NotDispatched,
    Succeeded,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRecord {
    pub operation_id: String,
    pub tool: ToolId,
    pub status: ExecutionStatus,
    pub error: Option<AgentErrorCode>,
}

/// Persistent events are metadata only. The host may store the user prompt
/// separately with explicit UI disclosure, but never log raw observations.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeEvent {
    Started,
    Transition { transition: TaskTransition },
    Proposed { tool: ToolId },
    RejectedProposal { code: AgentErrorCode },
    DispatchIntent { operation_id: String, tool: ToolId },
    Execution { record: ExecutionRecord },
    Finished { state: TaskState, usage: BudgetUsage, error: Option<AgentErrorCode> },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReport {
    pub task_id: TaskId,
    pub state: TaskState,
    pub usage: BudgetUsage,
    /// Model-written text, not authoritative evidence of native success.
    pub answer: Option<String>,
    pub error: Option<AgentError>,
    pub executions: Vec<ExecutionRecord>,
    /// A journal failure makes the result unsuitable as durable evidence.
    pub journal_incomplete: bool,
}

/// Trusted host adapters. Run synchronously on a dedicated task worker.
/// decide/approve must cooperate with cancellation and return only after
/// their work stops. No policy lock is held during model, UI or native work.
/// Persistence callbacks must commit before returning Ok, and never call
/// back into policy while flushing audit. No callback may panic.
/// An approval must come from the human-facing trusted UI, not the model.
/// Resolve and display the actual catalog target for apps.launch before
/// returning AllowOnce; if it is missing/changed, deny instead.
pub trait RuntimeHost {
    fn decide(&mut self, request: DecisionRequest, cancellation: Cancellation)
        -> crate::inference::Result<DecisionResponse>;
    fn approve(&mut self, request: &ApprovalRequest, cancellation: Cancellation, wait: Duration)
        -> AgentResult<ApprovalChoice>;
    fn execute(&mut self, registry: &ToolRegistry, permit: AuthorizedCall)
        -> AgentResult<ToolObservation>;
    fn persist_audit(&mut self, namespace: &str, batch: &PermissionAuditBatch) -> AgentResult<()>;
    fn record(&mut self, id: TaskId, event: &RuntimeEvent) -> AgentResult<()>;

    /// Optional transient UI data. Never persist full arguments or document
    /// contents in the audit or automatically navigate rendered output.
    fn proposed(&mut self, _id: TaskId, _call: &ValidatedCall) {}
    fn observed(&mut self, _id: TaskId, _record: &ExecutionRecord, _observation: &ToolObservation) {}
}

fn storage_error() -> AgentError {
    AgentError::new(AgentErrorCode::StorageFailed, "The agent journal could not be persisted.")
}

fn denied() -> AgentError {
    AgentError::new(AgentErrorCode::PermissionDenied, "The operation was denied. Select an authorized resource or approve the exact action.")
}

fn policy_lock(policy: &Mutex<PermissionEngine>) -> AgentResult<MutexGuard<'_, PermissionEngine>> {
    policy.lock().map_err(|_| storage_error())
}

/// Close the policy lease even if an adapter unwinds. Unflushed records
/// remain in the engine; restart recovery must mark unfinished work.
struct PolicyTask<'a> {
    policy: &'a Mutex<PermissionEngine>,
    id: TaskId,
    closed: bool,
}

impl PolicyTask<'_> {
    fn close(&mut self) -> AgentResult<()> {
        let mut policy = policy_lock(self.policy)?;
        if !policy.end_task(self.id) {
            return Err(storage_error());
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for PolicyTask<'_> {
    fn drop(&mut self) {
        if !self.closed {
            // Recover a poisoned lock for cleanup only, never for new work.
            let mut policy = self.policy.lock().unwrap_or_else(|error| error.into_inner());
            policy.end_task(self.id);
        }
    }
}

struct Run<'a, H> {
    registry: &'a ToolRegistry,
    policy: &'a Mutex<PermissionEngine>,
    host: &'a mut H,
    task: AgentTask,
    budget: AgentBudget,
    cancellation: Cancellation,
    executions: Vec<ExecutionRecord>,
    journal_incomplete: bool,
    launch_attempted: bool,
}

/// Run one task. The host owns the surrounding operation reservation and
/// must exclude chat/model changes for the entire task, including approvals.
/// Initial failures return Err; once the policy task is opened, return a
/// terminal report preserving any known side effects even on journal error.
pub fn run_task<H: RuntimeHost>(
    registry: &ToolRegistry,
    policy: &Mutex<PermissionEngine>,
    host: &mut H,
    request: TaskRequest,
) -> AgentResult<TaskReport> {
    let budget = AgentBudget::new(request.limits, request.cancellation.clone())?;
    let scopes = policy_lock(policy)?.scopes();
    let mut decision = DecisionRequest::new(&request.prompt, registry, &scopes, request.decision_tokens)?;
    {
        let mut engine = policy_lock(policy)?;
        engine.flush_audit(|namespace, batch| host.persist_audit(namespace, batch))?;
        engine.begin_task(request.id, budget.remaining_time()?, request.cancellation.clone())?;
    }
    let mut lease = PolicyTask { policy, id: request.id, closed: false };
    let mut run = Run {
        registry, policy, host, task: AgentTask::new(request.id), budget,
        cancellation: request.cancellation, executions: Vec::new(),
        journal_incomplete: false, launch_attempted: false,
    };
    let mut result = run.record(RuntimeEvent::Started)
        .and_then(|()| run.flush())
        .and_then(|()| run.transition(TaskState::Planning))
        .and_then(|()| run.loop_task(&mut decision, request.decision_tokens));

    if lease.close().is_err() {
        run.journal_incomplete = true;
        result = Err(storage_error());
    }
    if let Err(error) = run.flush() {
        result = Err(error);
    }

    let (answer, mut error, state) = match result {
        Ok(answer) => (Some(answer), None, TaskState::Completed),
        Err(error) => {
            let state = match error.code {
                AgentErrorCode::Cancelled => TaskState::Cancelled,
                AgentErrorCode::TimedOut => TaskState::TimedOut,
                AgentErrorCode::PermissionDenied if matches!(
                    run.task.state(), TaskState::Validating | TaskState::AwaitingAuthorization | TaskState::Authorized
                ) => TaskState::Denied,
                _ => TaskState::Failed,
            };
            (None, Some(error), state)
        }
    };
    if run.transition(state).is_err() {
        run.journal_incomplete = true;
        error = Some(storage_error());
    }
    let final_state = run.task.state();
    let usage = run.budget.usage();
    if run.record(RuntimeEvent::Finished {
        state: final_state, usage, error: error.as_ref().map(|e| e.code),
    }).is_err() {
        error = Some(storage_error());
    }
    Ok(TaskReport {
        task_id: request.id, state: final_state, usage, answer, error,
        executions: run.executions, journal_incomplete: run.journal_incomplete,
    })
}

impl<H: RuntimeHost> Run<'_, H> {
    fn record(&mut self, event: RuntimeEvent) -> AgentResult<()> {
        self.host.record(self.task.id(), &event).map_err(|_| {
            self.journal_incomplete = true;
            storage_error()
        })
    }

    fn flush(&mut self) -> AgentResult<()> {
        let result = match policy_lock(self.policy) {
            Ok(mut policy) => policy.flush_audit(|namespace, batch| self.host.persist_audit(namespace, batch)),
            Err(error) => Err(error),
        };
        result.map_err(|_| {
            self.journal_incomplete = true;
            storage_error()
        })
    }

    fn transition(&mut self, state: TaskState) -> AgentResult<()> {
        let transition = self.task.transition(state)?;
        self.record(RuntimeEvent::Transition { transition })
    }

    fn loop_task(&mut self, request: &mut DecisionRequest, per_call: u32) -> AgentResult<String> {
        loop {
            let allowed = self.budget.begin_model(per_call)?;
            request.set_max_tokens(allowed)?;
            let remaining = match self.budget.remaining_time() {
                Ok(remaining) => remaining,
                Err(error) => {
                    self.budget.finish_model(Some(0))?;
                    return Err(error);
                }
            };
            let cancellation = self.cancellation.with_timeout(remaining);
            let response = self.host.decide(request.clone(), cancellation);
            // Always settle the reservation, even on cancellation or error.
            self.budget.finish_model(response.as_ref().ok().map(|r| r.stats.generated_tokens))?;
            self.budget.remaining_time()?;
            let response = response.map_err(|_| AgentError::new(AgentErrorCode::ModelFailed, "The local model could not produce an agent decision."))?;
            if response.stats.reason == FinishReason::Cancelled {
                return Err(AgentError::new(AgentErrorCode::Cancelled, "The model decision was cancelled."));
            }
            match parse_decision(self.registry, &response) {
                Ok(AgentDecision::Final(answer)) => return Ok(answer),
                Ok(AgentDecision::Tool(call)) => {
                    self.transition(TaskState::Validating)?;
                    self.record(RuntimeEvent::Proposed { tool: call.tool() })?;
                    self.host.proposed(self.task.id(), &call);
                    let observation = self.dispatch(&call)?;
                    // Keep the actual result; no model text is evidence.
                    request.push(DecisionStep::Observation { call, observation })?;
                    self.transition(TaskState::Planning)?;
                }
                Err(error) => {
                    self.record(RuntimeEvent::RejectedProposal { code: error.code })?;
                    self.budget.record_retry()?;
                    request.push(DecisionStep::InvalidProposal)?;
                }
            }
        }
    }

    fn deny_reason(reason: DenialReason) -> AgentError {
        match reason {
            DenialReason::TaskCancelled => AgentError::new(AgentErrorCode::Cancelled, "The task was cancelled."),
            DenialReason::TaskExpired | DenialReason::ApprovalExpired => AgentError::new(AgentErrorCode::TimedOut, "The authorization deadline expired."),
            DenialReason::AuditUnavailable => storage_error(),
            _ => denied(),
        }
    }

    fn dispatch(&mut self, call: &ValidatedCall) -> AgentResult<ToolObservation> {
        let id = self.task.id();
        let tool = call.tool();
        // Conservative MVP: only one launch attempt per task, even if the
        // model proposes a different ID. Never replay an uncertain side effect.
        if tool == ToolId::AppLaunch && self.launch_attempted {
            return Err(AgentError::new(AgentErrorCode::PermissionDenied, "Only one application launch attempt is allowed per task."));
        }
        // Charge an attempt before requesting permission, not only on success.
        self.budget.begin_tool()?;
        let wait = self.budget.approval_wait()?;
        let decision = policy_lock(self.policy)?.authorize(self.registry, id, call, wait)?;
        self.flush()?;
        let operation_id = match decision {
            AuthorizationDecision::Allow { operation_id } => operation_id,
            AuthorizationDecision::Deny { reason } => return Err(Self::deny_reason(reason)),
            AuthorizationDecision::Confirm { request } => {
                self.transition(TaskState::AwaitingAuthorization)?;
                let wait = self.budget.approval_wait()?.min(Duration::from_millis(request.expires_in_ms));
                if wait.is_zero() {
                    return Err(AgentError::new(AgentErrorCode::TimedOut, "The authorization deadline expired."));
                }
                let choice = self.host.approve(&request, self.cancellation.with_timeout(wait), wait)?;
                self.budget.remaining_time()?;
                let decision = policy_lock(self.policy)?.respond(
                    id, &request.operation_id, choice == ApprovalChoice::AllowOnce,
                )?;
                self.flush()?;
                match decision {
                    AuthorizationDecision::Allow { operation_id } => operation_id,
                    AuthorizationDecision::Deny { reason } => return Err(Self::deny_reason(reason)),
                    AuthorizationDecision::Confirm { .. } => return Err(denied()),
                }
            }
        };
        self.transition(TaskState::Authorized)?;
        self.budget.remaining_time()?;
        let permit = policy_lock(self.policy)?.consume(self.registry, id, &operation_id, call)?;
        // Persist consumed authorization and dispatch intent before any side
        // effect. If persistence fails, drop the permit without dispatching.
        self.flush()?;
        self.transition(TaskState::Executing)?;
        self.record(RuntimeEvent::DispatchIntent { operation_id: operation_id.clone(), tool })?;
        if let Err(error) = self.budget.remaining_time().and_then(|_| permit.check_dispatch()) {
            self.record_execution(operation_id, tool, ExecutionStatus::NotDispatched, Some(error.code))?;
            return Err(error);
        }
        if tool == ToolId::AppLaunch {
            self.launch_attempted = true;
        }
        let output = self.host.execute(self.registry, permit);
        // Record actual or uncertain outcomes before checking cancellation,
        // timeout or further inference. Never erase a known launch dispatch.
        let observation = match output {
            Ok(observation) if observation.matches_call(call) => observation,
            Ok(_) => {
                let error = AgentError::new(AgentErrorCode::ToolFailed, "The tool result did not match its request.");
                self.record_execution(operation_id, tool, failure_status(tool), Some(error.code))?;
                return Err(error);
            }
            Err(error) => {
                self.record_execution(operation_id, tool, failure_status(tool), Some(error.code))?;
                return Err(error);
            }
        };
        let (status, error) = match observation.outcome() {
            ToolOutcome::Success { .. } => (ExecutionStatus::Succeeded, None),
            ToolOutcome::Failure { error } => (failure_status(tool), Some(error.clone())),
        };
        self.record_execution(operation_id, tool, status, error.as_ref().map(|e| e.code))?;
        if let Some(record) = self.executions.last() {
            self.host.observed(id, record, &observation);
        }
        self.transition(TaskState::Observing)?;
        if let Some(error) = error {
            return Err(error);
        }
        self.budget.remaining_time()?;
        Ok(observation)
    }

    fn record_execution(&mut self, operation_id: String, tool: ToolId, status: ExecutionStatus, error: Option<AgentErrorCode>) -> AgentResult<()> {
        let record = ExecutionRecord { operation_id, tool, status, error };
        // Retain known results in the returned report even if persistence fails.
        self.executions.push(record.clone());
        self.record(RuntimeEvent::Execution { record })
    }
}

fn failure_status(tool: ToolId) -> ExecutionStatus {
    if tool == ToolId::AppLaunch { ExecutionStatus::Uncertain } else { ExecutionStatus::Failed }
}
