//! Platform-independent agent domain. No model or tool execution is performed here.

mod budget;
mod error;
mod task;
mod tool;
mod registry;
mod policy;
mod decision;
mod runtime;

pub use budget::{AgentBudget, AgentLimits, BudgetUsage};
pub use error::{AgentError, AgentErrorCode, AgentResult};
pub use task::{AgentTask, TaskId, TaskState, TaskTransition};

pub use registry::ToolRegistry;
pub use tool::{
    AppEntry, ApplicationArgs, CalculatorArgs, DeviceInfo, FileReadArgs, FileSearchArgs,
    PermissionRequirement, Risk, ToolArguments, ToolDefinition, ToolId, ToolObservation,
    ToolOutcome, ToolOutput, ValidatedCall, MAX_OBSERVATION_BYTES, MAX_PROPOSAL_BYTES,
};

pub use policy::{
    ApprovalRequest, AuditAction, AuthorizationDecision, AuthorizedCall, DenialReason,
    PermissionAudit, PermissionAuditBatch, PermissionEngine, ScopeKind, ScopeState, ScopeSummary,
};

pub use decision::{AgentDecision, DecisionRequest, DecisionResponse, DecisionStep, parse_decision};

pub use runtime::{run_task, ApprovalChoice, ExecutionRecord, ExecutionStatus, RuntimeEvent, RuntimeHost, TaskReport, TaskRequest};
