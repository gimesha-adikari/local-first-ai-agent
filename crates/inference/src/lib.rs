//! Dedicated local inference worker and optional llama.cpp provider.

#[cfg(any(feature = "llama", test))]
mod gguf;
#[cfg(feature = "llama")]
mod llama;
mod worker;

#[cfg(feature = "llama")]
pub use gguf::trained_context;
#[cfg(feature = "llama")]
pub use llama::LlamaProvider;
pub use worker::{InferenceWorker, Pending, Phase, Status};
