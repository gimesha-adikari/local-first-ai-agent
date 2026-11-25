//! Platform-independent agent domain. No model or tool execution is performed here.

mod budget;
mod decision;
mod error;
mod policy;
mod registry;
mod runtime;
mod store;
mod task;
mod tool;

pub use budget::{AgentBudget, AgentLimits, BudgetUsage};
pub use error::{AgentError, AgentErrorCode, AgentResult};
pub use task::{AgentTask, TaskId, TaskState, TaskTransition};

pub use registry::ToolRegistry;
pub use tool::{
    AppEntry, ApplicationArgs, CalculatorArgs, DeviceInfo, FileReadArgs, FileSearchArgs,
    MAX_OBSERVATION_BYTES, MAX_PDF_BYTES, MAX_PDF_PAGES, MAX_PDF_RANGE_PAGES, MAX_PDF_TEXT_BYTES,
    MAX_PROPOSAL_BYTES, PdfPageRange, PdfPageText, PermissionRequirement, Risk, ToolArguments,
    ToolDefinition, ToolId, ToolObservation, ToolOutcome, ToolOutput, ValidatedCall,
};

pub use policy::{
    ApprovalRequest, AuditAction, AuthorizationDecision, AuthorizedCall, DenialReason,
    PermissionAudit, PermissionAuditBatch, PermissionEngine, ScopeKind, ScopeState, ScopeSummary,
};

pub use decision::{
    AgentDecision, DecisionRequest, DecisionResponse, DecisionStep, DecisionStyle, parse_decision,
};

pub use runtime::{
    ApprovalChoice, ExecutionRecord, ExecutionStatus, RuntimeEvent, RuntimeHost, TaskReport,
    TaskRequest, run_task,
};

pub use store::{
    AgentStore, StoredAudit, StoredExecution, StoredTask, StoredTaskDetail, StoredTaskEvent,
};
