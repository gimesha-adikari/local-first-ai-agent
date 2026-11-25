use std::{collections::VecDeque, sync::Mutex, time::Duration};

use agent_core::{
    agent::{
        AgentError, AgentErrorCode, AgentLimits, AgentResult, ApprovalChoice, ApprovalRequest,
        AuditAction, AuthorizedCall, DecisionRequest, DecisionResponse, ExecutionStatus,
        PermissionAuditBatch, PermissionEngine, RuntimeEvent, RuntimeHost, TaskId, TaskRequest,
        TaskState, ToolArguments, ToolObservation, ToolOutput, ToolRegistry, run_task,
    },
    inference::{
        Cancellation, ErrorCode, FinishReason, GenerationStats, InferenceError,
        Result as InferenceResult,
    },
};

const CALCULATE: &str =
    r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2+2"}}"#;
const ANOTHER_CALCULATION: &str =
    r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"3+3"}}"#;
const LAUNCH: &str = r#"{"kind":"tool_call","tool":"apps.launch","arguments":{"app_id":"app_1"}}"#;
const FINAL: &str = r#"{"kind":"final","answer":"Done"}"#;

struct Host {
    responses: VecDeque<&'static str>,
    trace: Vec<&'static str>,
    choice: ApprovalChoice,
    approval_error: Option<AgentError>,
    fail_intent: bool,
    fail_outcome: bool,
    fail_consumed_audit: bool,
    uncertain_launch: bool,
    cancel_after_execute: bool,
    cancellation: Cancellation,
    dispatches: usize,
    approvals: usize,
    saw_observation: bool,
}

impl Host {
    fn new(responses: &[&'static str]) -> Self {
        Self {
            responses: responses.iter().copied().collect(),
            trace: Vec::new(),
            choice: ApprovalChoice::AllowOnce,
            approval_error: None,
            fail_intent: false,
            fail_outcome: false,
            fail_consumed_audit: false,
            uncertain_launch: false,
            cancel_after_execute: false,
            cancellation: Cancellation::default(),
            dispatches: 0,
            approvals: 0,
            saw_observation: false,
        }
    }
}

fn storage_error() -> AgentError {
    AgentError::new(AgentErrorCode::StorageFailed, "Fixture write failed.")
}

impl RuntimeHost for Host {
    fn decide(
        &mut self,
        request: DecisionRequest,
        _: Cancellation,
    ) -> InferenceResult<DecisionResponse> {
        let generation = request.generation_request()?;
        self.saw_observation |= generation.prompt.contains("untrusted_observation");
        let text = self
            .responses
            .pop_front()
            .expect("unexpected model invocation");
        if text == "fixture context error" {
            return Err(InferenceError::new(
                ErrorCode::ContextLimit,
                "private fixture cause",
            ));
        }
        Ok(DecisionResponse {
            text: text.into(),
            stats: GenerationStats {
                reason: if text == "fixture output limit" {
                    FinishReason::TokenLimit
                } else {
                    FinishReason::EndOfGeneration
                },
                prompt_tokens: 32,
                prompt_tokens_per_second: None,
                generated_tokens: 10,
                time_to_first_token_ms: None,
                elapsed_ms: 1,
                tokens_per_second: 10.0,
                excluded_turns: 0,
                output_budget: None,
                assistant_replay: None,
            },
        })
    }

    fn approve(
        &mut self,
        _: &ApprovalRequest,
        _: Cancellation,
        _: Duration,
    ) -> AgentResult<ApprovalChoice> {
        self.approvals += 1;
        self.approval_error.clone().map_or(Ok(self.choice), Err)
    }

    fn execute(
        &mut self,
        _: &ToolRegistry,
        permit: AuthorizedCall,
    ) -> AgentResult<ToolObservation> {
        permit.check_dispatch()?;
        assert_eq!(self.trace.last(), Some(&"intent"));
        assert!(self.trace.contains(&"consumed"));
        self.trace.push("execute");
        self.dispatches += 1;
        let call = permit.call();
        let output = match call.arguments() {
            ToolArguments::Calculate(_) => ToolOutput::Calculate { value: 4.0 },
            ToolArguments::AppLaunch(args) => {
                if self.uncertain_launch {
                    return Err(AgentError::new(
                        AgentErrorCode::ToolFailed,
                        "Fixture launch uncertain.",
                    ));
                }
                ToolOutput::AppLaunch {
                    app_id: args.app_id.clone(),
                    dispatched: true,
                }
            }
            _ => panic!("unexpected tool"),
        };
        if self.cancel_after_execute {
            self.cancellation.cancel();
        }
        ToolObservation::success(call, output)
    }

    fn persist_audit(&mut self, _: &str, batch: &PermissionAuditBatch) -> AgentResult<()> {
        if batch
            .records
            .iter()
            .any(|record| record.action == AuditAction::AuthorizationConsumed)
        {
            if self.fail_consumed_audit {
                return Err(storage_error());
            }
            self.trace.push("consumed");
        }
        Ok(())
    }

    fn record(&mut self, _: TaskId, event: &RuntimeEvent) -> AgentResult<()> {
        match event {
            RuntimeEvent::DispatchIntent { .. } => {
                if self.fail_intent {
                    return Err(storage_error());
                }
                self.trace.push("intent");
            }
            RuntimeEvent::Execution { .. } => {
                if self.fail_outcome {
                    return Err(storage_error());
                }
                self.trace.push("outcome");
            }
            _ => {}
        }
        Ok(())
    }
}

fn run(host: &mut Host, limits: AgentLimits) -> agent_core::agent::TaskReport {
    let registry = ToolRegistry::mvp().unwrap();
    let policy = Mutex::new(PermissionEngine::new("runtime_test").unwrap());
    let request = TaskRequest {
        id: TaskId::new(1).unwrap(),
        prompt: "Fixture task".into(),
        limits,
        decision_tokens: 128,
        cancellation: host.cancellation.clone(),
    };
    run_task(&registry, &policy, host, request).unwrap()
}

#[test]
fn executes_then_observes_then_finishes_with_durable_ordering() {
    let mut host = Host::new(&[CALCULATE, FINAL]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(report.answer.as_deref(), Some("Done"));
    assert_eq!(report.usage.model_calls, 2);
    assert_eq!(report.usage.generated_tokens, 20);
    assert_eq!(report.usage.tool_calls, 1);
    assert_eq!(host.trace, ["consumed", "intent", "execute", "outcome"]);
    assert!(host.saw_observation);
    assert_eq!(report.executions[0].status, ExecutionStatus::Succeeded);
}

#[test]
fn malformed_proposals_are_bounded_and_never_dispatched() {
    let mut host = Host::new(&["invalid", "invalid", "invalid"]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Failed);
    assert_eq!(report.usage.retries, 2);
    assert_eq!(report.usage.model_calls, 3);
    assert_eq!(host.dispatches, 0);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::InvalidProposal);
}

#[test]
fn nested_launch_prose_never_dispatches_even_after_an_actual_native_result() {
    const NESTED: &str =
        r#"{"kind":"final","answer":"{'tool':'apps.launch','arguments':{'app_id':'app_1'}}"}"#;
    let mut host = Host::new(&[CALCULATE, NESTED, NESTED, NESTED]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Failed);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::InvalidProposal);
    assert_eq!(report.usage.retries, 2);
    assert_eq!(host.dispatches, 1);
    assert_eq!(report.executions[0].status, ExecutionStatus::Succeeded);
    assert!(report.answer.is_none());
    let mut host = Host::new(&[NESTED, CALCULATE, FINAL]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(report.usage.retries, 1);
    assert_eq!(host.dispatches, 1);
}

#[test]
fn tool_budget_stops_distinct_proposals() {
    let mut host = Host::new(&[CALCULATE, ANOTHER_CALCULATION]);
    let report = run(
        &mut host,
        AgentLimits {
            tool_calls: 1,
            ..AgentLimits::default()
        },
    );
    assert_eq!(report.state, TaskState::Failed);
    assert_eq!(report.usage.tool_calls, 1);
    assert_eq!(host.dispatches, 1);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::BudgetExhausted);
}

#[test]
fn blocks_identical_successful_reads_with_a_bounded_correction() {
    let mut host = Host::new(&[CALCULATE, CALCULATE, FINAL]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(report.usage.retries, 1);
    assert_eq!(report.usage.tool_calls, 1);
    assert_eq!(host.dispatches, 1);

    let mut host = Host::new(&[CALCULATE, CALCULATE, CALCULATE, CALCULATE]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Failed);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::BudgetExhausted);
    assert_eq!(report.usage.retries, 2);
    assert_eq!(host.dispatches, 1);
}

#[test]
fn preserves_distinct_calls_and_reads_after_other_work() {
    let mut host = Host::new(&[CALCULATE, ANOTHER_CALCULATION, CALCULATE, FINAL]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(report.usage.retries, 0);
    assert_eq!(host.dispatches, 3);
}

#[test]
fn context_failure_keeps_successful_execution_and_explains_recovery() {
    let mut host = Host::new(&[CALCULATE, "fixture context error"]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Failed);
    assert!(report.answer.is_none());
    assert_eq!(report.executions[0].status, ExecutionStatus::Succeeded);
    assert_eq!(host.dispatches, 1);
    let error = report.error.unwrap();
    assert_eq!(error.code, AgentErrorCode::ModelContextLimit);
    assert!(error.message.contains("larger context"));
    assert!(!error.message.contains("private fixture cause"));
}

#[test]
fn output_limit_is_bounded_and_recovery_does_not_repeat_successful_calls() {
    let mut host = Host::new(&[
        CALCULATE,
        ANOTHER_CALCULATION,
        "fixture output limit",
        CALCULATE,
        FINAL,
    ]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(report.usage.retries, 2);
    assert_eq!(host.dispatches, 2);

    let mut host = Host::new(&[
        CALCULATE,
        "fixture output limit",
        "fixture output limit",
        "fixture output limit",
    ]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Failed);
    assert_eq!(
        report.error.unwrap().code,
        AgentErrorCode::DecisionOutputLimit
    );
    assert_eq!(report.usage.retries, 2);
    assert_eq!(host.dispatches, 1);
}

#[test]
fn denial_and_missing_scope_never_dispatch() {
    let mut host = Host::new(&[LAUNCH]);
    host.choice = ApprovalChoice::Deny;
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Denied);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::ApprovalDeclined);
    assert_eq!(host.approvals, 1);
    assert_eq!(host.dispatches, 0);

    let mut host = Host::new(&[LAUNCH]);
    host.approval_error = Some(AgentError::new(
        AgentErrorCode::ApprovalExpired,
        "The approval window closed before the application was authorized.",
    ));
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::TimedOut);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::ApprovalExpired);
    assert_eq!(host.approvals, 1);
    assert_eq!(host.dispatches, 0);

    let mut host = Host::new(&[
        r#"{"kind":"tool_call","tool":"files.read","arguments":{"scope_id":"missing"}}"#,
    ]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Denied);
    assert_eq!(report.error.unwrap().code, AgentErrorCode::PermissionDenied);
    assert_eq!(host.approvals, 0);
    assert_eq!(host.dispatches, 0);
}

#[test]
fn audit_and_intent_failure_prevent_dispatch() {
    for fail_audit in [false, true] {
        let mut host = Host::new(&[CALCULATE]);
        host.fail_consumed_audit = fail_audit;
        host.fail_intent = !fail_audit;
        let report = run(&mut host, AgentLimits::default());
        assert_eq!(report.state, TaskState::Failed);
        assert!(report.journal_incomplete);
        assert_eq!(host.dispatches, 0);
    }
}

#[test]
fn launch_cancellation_preserves_dispatch_and_uncertainty_is_not_retried() {
    let mut host = Host::new(&[LAUNCH]);
    host.cancel_after_execute = true;
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Cancelled);
    assert_eq!(report.executions[0].status, ExecutionStatus::Succeeded);
    assert_eq!(host.dispatches, 1);

    let mut host = Host::new(&[LAUNCH]);
    host.uncertain_launch = true;
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Failed);
    assert_eq!(report.executions[0].status, ExecutionStatus::Uncertain);
    assert_eq!(host.dispatches, 1);
}

#[test]
fn repeated_launch_is_blocked_and_journal_failure_preserves_result() {
    let mut host = Host::new(&[LAUNCH, LAUNCH]);
    let report = run(&mut host, AgentLimits::default());
    assert_eq!(report.state, TaskState::Denied);
    assert_eq!(host.approvals, 1);
    assert_eq!(host.dispatches, 1);

    let mut host = Host::new(&[LAUNCH]);
    host.fail_outcome = true;
    let report = run(&mut host, AgentLimits::default());
    assert!(report.journal_incomplete);
    assert_eq!(report.executions[0].status, ExecutionStatus::Succeeded);
    assert_eq!(host.dispatches, 1);
}

#[test]
fn cancellation_children_expire_without_cancelling_parent() {
    let cancellation = Cancellation::default();
    let child = cancellation.with_timeout(Duration::ZERO);
    assert!(child.is_cancelled());
    assert!(!cancellation.is_cancelled());
    let child = cancellation.with_timeout(Duration::from_secs(60));
    cancellation.cancel();
    assert!(child.is_cancelled());
}

#[test]
fn failed_audit_flush_retains_records() {
    let mut policy = PermissionEngine::new("audit_test").unwrap();
    policy
        .grant_scope(agent_core::agent::ScopeKind::File, Duration::from_secs(30))
        .unwrap();
    assert!(policy.flush_audit(|_, _| Err(storage_error())).is_err());
    let mut seen = 0;
    policy
        .flush_audit(|_, batch| {
            seen = batch.records.len();
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, 1);
    policy
        .flush_audit(|_, _| panic!("audit should be empty"))
        .unwrap();
}
