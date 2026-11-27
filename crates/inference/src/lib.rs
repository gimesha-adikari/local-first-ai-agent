//! Dedicated local inference worker and optional llama.cpp provider.

mod adapter;
#[cfg(feature = "llama")]
mod backend;
#[cfg(feature = "llama")]
pub mod embedding;
#[cfg(any(feature = "llama", test))]
mod gguf;
#[cfg(feature = "llama")]
mod llama;
pub use adapter::LocalInference;
mod worker;

#[cfg(feature = "llama")]
pub use gguf::trained_context;
#[cfg(feature = "llama")]
pub use llama::LlamaProvider;
pub use worker::{InferenceWorker, Pending, Phase, Status};
