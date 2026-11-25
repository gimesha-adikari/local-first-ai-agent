use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::inference::Cancellation;

use super::{AgentError, AgentErrorCode, AgentResult};

const DEFAULT_TASK_TIMEOUT_SECONDS: u32 = 300;
pub(super) const MAX_TASK_LIFETIME: Duration = Duration::from_secs(600);
pub(super) const MAX_APPROVAL_LIFETIME: Duration = Duration::from_secs(60);

/// Conservative MVP ceilings. Callers may configure smaller limits but may
/// not increase these hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLimits {
    pub tool_calls: u32,
    pub model_calls: u32,
    pub retries: u32,
    pub generated_tokens: u32,
    pub task_timeout_ms: u64,
    pub approval_timeout_ms: u64,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            tool_calls: 5,
            model_calls: 8,
            retries: 2,
            generated_tokens: 2048,
            task_timeout_ms: u64::from(DEFAULT_TASK_TIMEOUT_SECONDS) * 1_000,
            approval_timeout_ms: MAX_APPROVAL_LIFETIME.as_millis() as u64,
        }
    }
}

impl AgentLimits {
    pub fn with_task_timeout_seconds(seconds: u32) -> AgentResult<Self> {
        let task_timeout_ms = u64::from(seconds) * 1_000;
        let limits = Self {
            task_timeout_ms,
            approval_timeout_ms: (MAX_APPROVAL_LIFETIME.as_millis() as u64).min(task_timeout_ms),
            ..Self::default()
        };
        limits.validate()?;
        Ok(limits)
    }

    pub fn validate(self) -> AgentResult<()> {
        let max = Self::default();
        if self.tool_calls == 0
            || self.tool_calls > max.tool_calls
            || self.model_calls == 0
            || self.model_calls > max.model_calls
            || self.retries > max.retries
            || self.generated_tokens == 0
            || self.generated_tokens > max.generated_tokens
            || self.task_timeout_ms == 0
            || Duration::from_millis(self.task_timeout_ms) > MAX_TASK_LIFETIME
            || self.approval_timeout_ms == 0
            || self.approval_timeout_ms > self.task_timeout_ms
            || self.approval_timeout_ms > max.approval_timeout_ms
        {
            return Err(AgentError::new(
                AgentErrorCode::InvalidLimits,
                "Agent limits must remain within the MVP ceilings.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetUsage {
    pub tool_calls: u32,
    pub model_calls: u32,
    pub retries: u32,
    pub generated_tokens: u32,
}

/// A single task's monotonic budget. The runtime owns this value and checks
/// it before dispatch and during interruptible operations. It does not spawn
/// timers or forcibly interrupt native calls.
#[derive(Debug)]
pub struct AgentBudget {
    limits: AgentLimits,
    usage: BudgetUsage,
    started: Instant,
    cancellation: Cancellation,
    pending_model_tokens: Option<u32>,
}

impl AgentBudget {
    pub fn new(limits: AgentLimits, cancellation: Cancellation) -> AgentResult<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            usage: BudgetUsage::default(),
            started: Instant::now(),
            cancellation,
            pending_model_tokens: None,
        })
    }

    pub const fn usage(&self) -> BudgetUsage {
        self.usage
    }

    pub const fn limits(&self) -> AgentLimits {
        self.limits
    }

    pub fn remaining_time(&self) -> AgentResult<Duration> {
        if self.cancellation.is_cancelled() {
            return Err(AgentError::new(
                AgentErrorCode::Cancelled,
                "The task was cancelled.",
            ));
        }
        Duration::from_millis(self.limits.task_timeout_ms)
            .checked_sub(self.started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| AgentError::new(AgentErrorCode::TimedOut, "The task deadline expired."))
    }

    /// Approval waits share the overall task deadline; they do not reset it.
    pub fn approval_wait(&self) -> AgentResult<Duration> {
        Ok(self
            .remaining_time()?
            .min(Duration::from_millis(self.limits.approval_timeout_ms)))
    }

    /// Reserve an invocation and return its maximum allowed output. The
    /// caller must also apply the model's actual context and provider limits.
    pub fn begin_model(&mut self, requested: u32) -> AgentResult<u32> {
        self.remaining_time()?;
        if self.pending_model_tokens.is_some() {
            return Err(AgentError::new(
                AgentErrorCode::Busy,
                "A model invocation is pending.",
            ));
        }
        let available = self.limits.generated_tokens - self.usage.generated_tokens;
        let allowed = requested.min(available);
        if allowed == 0 || self.usage.model_calls >= self.limits.model_calls {
            return Err(exhausted());
        }
        self.usage.model_calls += 1;
        self.pending_model_tokens = Some(allowed);
        Ok(allowed)
    }

    /// Settle exactly one invocation. On failure with unknown token usage,
    /// pass None to charge the entire reservation rather than guessing.
    /// Accounting must finish even when a task is cancelled or timed out.
    pub fn finish_model(&mut self, generated: Option<u32>) -> AgentResult<()> {
        let reserved = self.pending_model_tokens.take().ok_or_else(|| {
            AgentError::new(
                AgentErrorCode::InvalidTransition,
                "No model invocation is pending.",
            )
        })?;
        let actual = generated.unwrap_or(reserved);
        self.usage.generated_tokens += actual.min(reserved);
        if actual > reserved {
            self.usage.generated_tokens = self.limits.generated_tokens;
            return Err(exhausted());
        }
        Ok(())
    }

    pub fn begin_tool(&mut self) -> AgentResult<()> {
        self.remaining_time()?;
        if self.pending_model_tokens.is_some() {
            return Err(AgentError::new(
                AgentErrorCode::Busy,
                "A model invocation is pending.",
            ));
        }
        if self.usage.tool_calls >= self.limits.tool_calls {
            return Err(exhausted());
        }
        self.usage.tool_calls += 1;
        Ok(())
    }

    pub fn record_retry(&mut self) -> AgentResult<()> {
        self.remaining_time()?;
        if self.pending_model_tokens.is_some() {
            return Err(AgentError::new(
                AgentErrorCode::Busy,
                "A model invocation is pending.",
            ));
        }
        if self.usage.retries >= self.limits.retries {
            return Err(exhausted());
        }
        self.usage.retries += 1;
        Ok(())
    }
}

fn exhausted() -> AgentError {
    AgentError::new(
        AgentErrorCode::BudgetExhausted,
        "The agent execution budget is exhausted.",
    )
}
