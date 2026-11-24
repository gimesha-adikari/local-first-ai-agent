//! Platform-independent agent domain. No model or tool execution is performed here.

mod budget;
mod error;
mod task;

pub use budget::{AgentBudget, AgentLimits, BudgetUsage};
pub use error::{AgentError, AgentErrorCode, AgentResult};
pub use task::{AgentTask, TaskId, TaskState, TaskTransition};
