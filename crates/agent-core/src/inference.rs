//! Provider contract and data shared by the desktop host and inference worker.

use std::{
    fmt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
}

impl GenerationRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            max_tokens: 256,
            temperature: 0.7,
            top_p: 0.8,
            top_k: 20,
            seed: None,
            enable_thinking: false,
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
    pub config: ModelConfig,
    pub load_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    EndOfGeneration,
    TokenLimit,
    Cancelled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationStats {
    pub reason: FinishReason,
    pub prompt_tokens: usize,
    pub prompt_tokens_per_second: Option<f64>,
    pub generated_tokens: u32,
    pub time_to_first_token_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub tokens_per_second: f64,
}

#[derive(Debug, Serialize)]
pub struct TextDelta {
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A worker owns the provider for its entire lifetime. A failed load leaves the
/// provider empty. An unsuccessful generation leaves the loaded model reusable.
pub trait InferenceProvider: 'static {
    fn load(&mut self, path: &Path, config: ModelConfig) -> Result<ModelInfo>;
    fn unload(&mut self);
    fn generate(
        &mut self,
        request: &GenerationRequest,
        cancellation: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats>;
}
