use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    InvalidLimits,
    InvalidTransition,
    BudgetExhausted,
    TimedOut,
    Cancelled,
    InvalidProposal,
    UnknownTool,
    InvalidArguments,
    PermissionDenied,
    ScopeKindMismatch,
    ApprovalDeclined,
    ApprovalExpired,
    ToolFailed,
    PdfEncrypted,
    PdfNoText,
    PdfInvalid,
    PdfUnsupported,
    PdfLimit,
    PdfUnavailable,
    KnowledgeLimit,
    KnowledgeNotFound,
    EmbeddingUnavailable,
    EmbeddingIncompatible,
    EmbeddingIndexUnavailable,
    KnowledgeRevisionChanged,
    ModelFailed,
    ModelContextLimit,
    DecisionOutputLimit,
    StorageFailed,
    UnsupportedPlatform,
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentError {
    pub code: AgentErrorCode,
    pub message: &'static str,
}

impl AgentError {
    /// Use fixed, non-sensitive messages at the runtime and IPC boundaries.
    pub const fn new(code: AgentErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for AgentError {}

pub type AgentResult<T> = std::result::Result<T, AgentError>;
