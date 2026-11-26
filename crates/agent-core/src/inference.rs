//! Provider contract and data shared by the desktop host and inference worker.

use std::{
    fmt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Busy,
    InvalidConfig,
    InvalidPath,
    InvalidModel,
    NoSelection,
    NoModel,
    UnsupportedTemplate,
    ContextLimit,
    LoadFailed,
    GenerationFailed,
    StreamClosed,
    WorkerStopped,
    PickerFailed,
    Storage,
    NotFound,
    Cancelled,
    TimedOut,
    EmbeddingUnavailable,
    EmbeddingIncompatible,
    EmbeddingIndexUnavailable,
    MemoryLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InferenceError {
    pub code: ErrorCode,
    pub message: String,
}

impl InferenceError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for InferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InferenceError {}

pub type Result<T> = std::result::Result<T, InferenceError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub context_length: u32,
    pub threads: u32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            context_length: 2048,
            threads: 4,
        }
    }
}

impl ModelConfig {
    pub fn validate(self) -> Result<()> {
        if !(128..=32_768).contains(&self.context_length) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Context length must be between 128 and 32768 tokens.",
            ));
        }
        if !(1..=32).contains(&self.threads) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "CPU thread count must be between 1 and 32.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRequest {
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub seed: Option<u32>,
    #[serde(default)]
    pub enable_thinking: bool,
    #[serde(skip)]
    pub history: crate::chat::ChatHistory,
    #[serde(skip)]
    pub chat: Option<ChatGeneration>,
    #[serde(skip)]
    pub context: Option<crate::chat_context::ChatContext>,
    #[serde(skip)]
    pub context_observer: Option<crate::chat_context::ContextObserver>,
}

pub const MAX_CHAT_ATTEMPT_TOKENS: usize = 8192;

/// Opaque adapter-owned resume data. JSON is kept byte-for-byte compatible
/// with the existing local checkpoint format. Only the originating adapter
/// may interpret it, and it is never exposed through IPC.
#[derive(Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ResumeState(Box<serde_json::value::RawValue>);

impl std::fmt::Debug for ResumeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResumeState")
    }
}

impl ResumeState {
    pub fn encode(value: &impl Serialize) -> serde_json::Result<Self> {
        serde_json::value::to_raw_value(value).map(Self)
    }

    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_str(self.0.get())
    }
}

#[derive(Debug)]
pub struct ChatGeneration {
    pub adaptive: bool,
    pub replay: Option<ResumeState>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputBudget {
    pub requested: u32,
    pub effective: u32,
    pub context_length: u32,
}

impl GenerationRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            max_tokens: 768,
            temperature: 0.7,
            top_p: 0.8,
            top_k: 20,
            seed: None,
            enable_thinking: false,
            history: crate::chat::ChatHistory::default(),
            chat: None,
            context: None,
            context_observer: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.prompt.trim().is_empty()
            || self.prompt.len() > 64 * 1024
            || self.prompt.contains('\0')
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Enter a nonempty prompt of at most 64 KiB without NUL characters.",
            ));
        }
        if !(1..=4096).contains(&self.max_tokens)
            || !(0.0..=2.0).contains(&self.temperature)
            || !(0.0 < self.top_p && self.top_p <= 1.0)
            || !(1..=200).contains(&self.top_k)
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Use 1–4096 output tokens, temperature 0–2, top-p above 0 through 1, and top-k 1–200.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub file_name: String,
    pub name: String,
    pub architecture: String,
    pub parameters: u64,
    pub file_bytes: u64,
    pub trained_context: u32,
    pub supports_thinking: bool,
    /// Whether the loaded template passed a validated Agent decision format.
    /// This is independent of Thinking support.
    pub supports_agent: bool,
    pub config: ModelConfig,
    pub load_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    EndOfGeneration,
    TokenLimit,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationStats {
    pub reason: FinishReason,
    pub prompt_tokens: usize,
    pub prompt_tokens_per_second: Option<f64>,
    pub generated_tokens: u32,
    pub time_to_first_token_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub tokens_per_second: f64,
    pub excluded_turns: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_budget: Option<OutputBudget>,
    #[serde(skip)]
    pub assistant_replay: Option<ResumeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<crate::chat_context::ContextUsage>,
}

#[derive(Debug, Serialize)]
pub struct TextDelta {
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Cancellation {
    cancelled: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl Cancellation {
    /// A child shares explicit cancellation but adds a local monotonic deadline.
    /// Expiring the child does not change the parent's cancellation flag.
    pub fn with_timeout(&self, duration: Duration) -> Self {
        let deadline = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(Instant::now);
        Self {
            cancelled: self.cancelled.clone(),
            deadline: Some(
                self.deadline
                    .map_or(deadline, |parent| parent.min(deadline)),
            ),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Explicit user or host cancellation, excluding a local deadline.
    pub fn is_explicitly_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Whether this cancellation view deadline has elapsed.
    pub fn is_deadline_elapsed(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub fn is_cancelled(&self) -> bool {
        self.is_explicitly_cancelled() || self.is_deadline_elapsed()
    }
}

/// A worker owns the provider for its entire lifetime. A failed load leaves the
/// provider empty. An unsuccessful generation leaves the loaded model reusable.
pub trait InferenceProvider: 'static {
    fn load(&mut self, path: &Path, config: ModelConfig) -> Result<ModelInfo>;
    fn unload(&mut self);

    /// Separate, buffered agent decision path. Providers may opt out.
    fn decide(
        &mut self,
        _request: &crate::agent::DecisionRequest,
        _cancellation: &Cancellation,
    ) -> Result<crate::agent::DecisionResponse> {
        Err(InferenceError::new(
            ErrorCode::UnsupportedTemplate,
            "This provider does not support agent decisions.",
        ))
    }

    fn generate(
        &mut self,
        request: &GenerationRequest,
        cancellation: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats>;
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn distinguishes_explicit_cancellation_from_deadline_expiry() {
        let timed = Cancellation::default().with_timeout(Duration::ZERO);
        assert!(!timed.is_explicitly_cancelled());
        assert!(timed.is_deadline_elapsed());
        assert!(timed.is_cancelled());

        let parent = Cancellation::default();
        parent.cancel();
        let child = parent.with_timeout(Duration::from_secs(1));
        assert!(child.is_explicitly_cancelled());
        assert!(!child.is_deadline_elapsed());
        assert!(child.is_cancelled());
    }
}
