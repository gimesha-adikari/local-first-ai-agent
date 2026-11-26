use std::sync::OnceLock;

use agent_core::inference::{ErrorCode, InferenceError, Result};
use llama_cpp_2::llama_backend::LlamaBackend;

/// llama.cpp has one process-wide backend. The chat and embedding models and
/// contexts remain separately owned by their workers.
pub(crate) fn cpu_backend() -> Result<&'static LlamaBackend> {
    static BACKEND: OnceLock<Result<LlamaBackend>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            let mut backend = LlamaBackend::init().map_err(|_| {
                InferenceError::new(
                    ErrorCode::LoadFailed,
                    "Could not initialize the llama.cpp CPU backend.",
                )
            })?;
            backend.void_logs();
            Ok(backend)
        })
        .as_ref()
        .map_err(Clone::clone)
}
