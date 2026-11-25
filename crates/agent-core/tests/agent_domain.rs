use std::time::Duration;

use agent_core::{
    agent::{AgentBudget, AgentErrorCode, AgentLimits, AgentTask, TaskId, TaskState},
    inference::Cancellation,
};

#[test]
fn task_lifecycle_rejects_skipped_and_terminal_transitions() {
    assert!(TaskId::new(0).is_none());
    let mut task = AgentTask::new(TaskId::new(1).unwrap());
    assert_eq!(
        task.transition(TaskState::Executing).unwrap_err().code,
        AgentErrorCode::InvalidTransition,
    );
    assert_eq!(task.sequence(), 0);
    for (index, state) in [
        TaskState::Planning,
        TaskState::Validating,
        TaskState::AwaitingAuthorization,
        TaskState::Authorized,
        TaskState::Executing,
        TaskState::Observing,
        TaskState::Planning,
        TaskState::Completed,
    ]
    .into_iter()
    .enumerate()
    {
        let event = task.transition(state).unwrap();
        assert_eq!(event.sequence, index as u32 + 1);
        assert_eq!(event.to, state);
        assert_eq!(event.task_id, task.id());
    }
    assert!(task.state().is_terminal());
    assert!(task.transition(TaskState::Planning).is_err());
}

#[test]
fn denied_and_cancelled_tasks_cannot_resume() {
    let mut task = AgentTask::new(TaskId::new(2).unwrap());
    task.transition(TaskState::Planning).unwrap();
    task.transition(TaskState::Validating).unwrap();
    task.transition(TaskState::AwaitingAuthorization).unwrap();
    task.transition(TaskState::Denied).unwrap();
    assert!(task.transition(TaskState::Authorized).is_err());

    let mut task = AgentTask::new(TaskId::new(3).unwrap());
    task.transition(TaskState::Cancelled).unwrap();
    assert!(task.transition(TaskState::Planning).is_err());
}

#[test]
fn budget_rejects_excess_and_charges_unknown_generation() {
    let limits = AgentLimits {
        tool_calls: 1,
        model_calls: 2,
        retries: 0,
        generated_tokens: 10,
        ..AgentLimits::default()
    };
    let cancellation = Cancellation::default();
    let mut budget = AgentBudget::new(limits, cancellation.clone()).unwrap();

    assert_eq!(budget.begin_model(8).unwrap(), 8);
    assert_eq!(budget.begin_tool().unwrap_err().code, AgentErrorCode::Busy);
    budget.finish_model(Some(6)).unwrap();
    assert_eq!(budget.begin_model(8).unwrap(), 4);
    budget.finish_model(None).unwrap();
    assert_eq!(budget.usage().generated_tokens, 10);
    assert_eq!(
        budget.begin_model(1).unwrap_err().code,
        AgentErrorCode::BudgetExhausted,
    );
    budget.begin_tool().unwrap();
    assert_eq!(
        budget.begin_tool().unwrap_err().code,
        AgentErrorCode::BudgetExhausted
    );
    assert_eq!(
        budget.record_retry().unwrap_err().code,
        AgentErrorCode::BudgetExhausted
    );
    cancellation.cancel();
    assert_eq!(
        budget.remaining_time().unwrap_err().code,
        AgentErrorCode::Cancelled
    );
}

#[test]
fn limits_cannot_exceed_mvp_ceilings() {
    let limits = AgentLimits {
        tool_calls: 6,
        ..AgentLimits::default()
    };
    assert_eq!(
        limits.validate().unwrap_err().code,
        AgentErrorCode::InvalidLimits
    );
}

#[test]
fn task_timeout_has_a_hard_ceiling_and_bounds_approval() {
    let defaults = AgentLimits::default();
    assert_eq!(defaults.task_timeout_ms, 300_000);
    assert_eq!(defaults.approval_timeout_ms, 60_000);

    let maximum = AgentLimits::with_task_timeout_seconds(600).unwrap();
    assert_eq!(maximum.task_timeout_ms, 600_000);
    assert_eq!(maximum.approval_timeout_ms, defaults.approval_timeout_ms);
    assert_eq!(maximum.tool_calls, defaults.tool_calls);
    assert_eq!(maximum.model_calls, defaults.model_calls);
    assert_eq!(maximum.retries, defaults.retries);
    assert_eq!(maximum.generated_tokens, defaults.generated_tokens);
    for invalid in [0, 601, u32::MAX] {
        assert_eq!(
            AgentLimits::with_task_timeout_seconds(invalid)
                .unwrap_err()
                .code,
            AgentErrorCode::InvalidLimits
        );
    }

    let short = AgentLimits::with_task_timeout_seconds(1).unwrap();
    let budget = AgentBudget::new(short, Cancellation::default()).unwrap();
    assert!(budget.approval_wait().unwrap() <= Duration::from_secs(1));
    let budget = AgentBudget::new(maximum, Cancellation::default()).unwrap();
    assert_eq!(budget.approval_wait().unwrap(), Duration::from_secs(60));
}
