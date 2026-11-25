use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use super::{AgentError, AgentErrorCode, AgentResult};

/// Assigned by the trusted host, never by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(NonZeroU64);

impl TaskId {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Created,
    Planning,
    Validating,
    AwaitingAuthorization,
    Authorized,
    Executing,
    Observing,
    Completed,
    Denied,
    Cancelled,
    Failed,
    TimedOut,
    Interrupted,
}

impl TaskState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Denied
                | Self::Cancelled
                | Self::Failed
                | Self::TimedOut
                | Self::Interrupted
        )
    }

    fn permits(self, next: Self) -> bool {
        use TaskState::*;
        if self.is_terminal() {
            return false;
        }
        if matches!(next, Cancelled | Failed | TimedOut | Interrupted) {
            return true;
        }
        matches!(
            (self, next),
            (Created, Planning)
                | (Planning, Validating | Completed)
                | (
                    Validating,
                    AwaitingAuthorization | Authorized | Denied | Planning
                )
                | (AwaitingAuthorization, Authorized | Denied)
                | (Authorized, Executing | Denied)
                | (Executing, Observing)
                | (Observing, Planning)
        )
    }
}

/// An ordered record of a successful transition, not a permission grant or
/// evidence of a tool side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskTransition {
    pub task_id: TaskId,
    pub sequence: u32,
    pub from: TaskState,
    pub to: TaskState,
}

/// Owned by the trusted runtime. State cannot be assigned by a model or IPC
/// payload. The runtime must still verify policy, results and budgets before
/// requesting any transition.
#[derive(Debug)]
pub struct AgentTask {
    id: TaskId,
    state: TaskState,
    sequence: u32,
}

impl AgentTask {
    pub const fn new(id: TaskId) -> Self {
        Self {
            id,
            state: TaskState::Created,
            sequence: 0,
        }
    }

    pub const fn id(&self) -> TaskId {
        self.id
    }

    pub const fn state(&self) -> TaskState {
        self.state
    }

    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    pub fn transition(&mut self, next: TaskState) -> AgentResult<TaskTransition> {
        if !self.state.permits(next) {
            return Err(AgentError::new(
                AgentErrorCode::InvalidTransition,
                "The requested agent task transition is not allowed.",
            ));
        }
        let sequence = self.sequence.checked_add(1).ok_or_else(|| {
            AgentError::new(
                AgentErrorCode::BudgetExhausted,
                "The agent transition sequence is exhausted.",
            )
        })?;
        let event = TaskTransition {
            task_id: self.id,
            sequence,
            from: self.state,
            to: next,
        };
        self.state = next;
        self.sequence = sequence;
        Ok(event)
    }
}
