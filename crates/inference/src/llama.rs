use std::{
    num::NonZeroU32,
    path::Path,
    time::{Duration, Instant},
};

use agent_core::inference::{
    Cancellation, ErrorCode, FinishReason, GenerationRequest, GenerationStats, InferenceError,
    InferenceProvider, ModelConfig, ModelInfo, Result, TextDelta,
};
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{LlamaModel, params::LlamaModelParams},
    sampling::LlamaSampler,
};
use minijinja::{Environment, context};

const PREFILL_BATCH: usize = 64;

/// Owns native resources on the inference worker. Fields drop in declaration order.
#[derive(Default)]
pub struct LlamaProvider {
    loaded: Option<LoadedModel>,
    backend: Option<LlamaBackend>,
}

struct LoadedModel {
    model: LlamaModel,
    config: ModelConfig,
    template: String,
}

impl InferenceProvider for LlamaProvider {
    fn load(&mut self, path: &Path, config: ModelConfig) -> Result<ModelInfo> {
        self.unload();
        config.validate()?;
        let file_bytes = crate::gguf::validate(path)?;
        let start = Instant::now();
        if self.backend.is_none() {
            let mut backend = LlamaBackend::init().map_err(|_| {
                InferenceError::new(
                    ErrorCode::LoadFailed,
                    "Could not initialize the llama.cpp CPU backend.",
                )
            })?;
            backend.void_logs();
            self.backend = Some(backend);
        }
        let backend = self.backend.as_ref().expect("backend initialized");
        let model = LlamaModel::load_from_file(
            backend,
            path,
            &LlamaModelParams::default().with_n_gpu_layers(0),
        )
        .map_err(|_| {
            InferenceError::new(
                ErrorCode::InvalidModel,
                "llama.cpp could not load this GGUF model.",
            )
        })?;
        let trained_context = model.n_ctx_train();
        if config.context_length > trained_context {
            return Err(InferenceError::new(
                ErrorCode::ContextLimit,
                format!("This model supports a context of {trained_context} tokens."),
            ));
        }
        let template = model
            .chat_template(None)
            .ok()
            .and_then(|template| template.to_string().ok())
            .ok_or_else(unsupported_template)?;
        let loaded = LoadedModel {
            model,
            config,
            template,
        };
        // Validate template compatibility before marking the model ready.
        loaded.render_prompt("Template check")?;
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let info = ModelInfo {
            name: loaded
                .model
                .meta_val_str("general.name")
                .unwrap_or_else(|_| file_name.clone()),
            architecture: loaded
                .model
                .meta_val_str("general.architecture")
                .unwrap_or_else(|_| "unknown".into()),
            parameters: loaded.model.n_params(),
            file_name,
            file_bytes,
            trained_context,
            config,
            load_ms: millis(start.elapsed()),
        };
        self.loaded = Some(loaded);
        Ok(info)
    }

    fn unload(&mut self) {
        self.loaded.take();
    }

    fn generate(
        &mut self,
        request: &GenerationRequest,
        cancellation: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats> {
        request.validate()?;
        let (Some(loaded), Some(backend)) = (&self.loaded, &self.backend) else {
            return Err(InferenceError::new(
                ErrorCode::NoModel,
                "Load a GGUF model first.",
            ));
        };
        loaded.generate(backend, request, cancellation, emit)
    }
}

fn unsupported_template() -> InferenceError {
    InferenceError::new(
        ErrorCode::UnsupportedTemplate,
        "The model needs a supported embedded chat template.",
    )
}

impl LoadedModel {
    fn render_prompt(&self, prompt: &str) -> Result<String> {
        let vocab = self.model.vocab();
        let special = |token: llama_cpp_2::token::LlamaToken| {
            if token.0 < 0 {
                String::new()
            } else {
                String::from_utf8_lossy(&vocab.token_to_piece(token, true, None)).into_owned()
            }
        };
        render_template(
            &self.template,
            prompt,
            &special(vocab.bos()),
            &special(vocab.eos()),
        )
    }

    fn generate(
        &self,
        backend: &LlamaBackend,
        request: &GenerationRequest,
        cancel: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats> {
        let started = Instant::now();
        let mut stats = GenerationStats {
            reason: FinishReason::Cancelled,
            prompt_tokens: 0,
            generated_tokens: 0,
            time_to_first_token_ms: None,
            elapsed_ms: 0,
            tokens_per_second: 0.0,
        };
        if cancel.is_cancelled() {
            return Ok(stats);
        }

        let prompt = self.render_prompt(&request.prompt)?;
        let vocab = self.model.vocab();
        let tokens = vocab.tokenize(prompt.as_bytes(), false, true);
        stats.prompt_tokens = tokens.len();
        if tokens.is_empty()
            || tokens.len() + request.max_tokens as usize > self.config.context_length as usize
        {
            return Err(InferenceError::new(
                ErrorCode::ContextLimit,
                "The formatted prompt and requested output exceed the configured context length.",
            ));
        }

        let threads = self.config.threads as i32;
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.config.context_length))
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_n_batch(PREFILL_BATCH as u32)
            .with_n_ubatch(PREFILL_BATCH as u32)
            .with_offload_kqv(false);
        let mut ctx = self
            .model
            .new_context(backend, params)
            .map_err(|_| generation_failed())?;
        let mut batch = LlamaBatch::new(PREFILL_BATCH, 1);
        for (chunk_index, chunk) in tokens.chunks(PREFILL_BATCH).enumerate() {
            if cancel.is_cancelled() {
                stats.elapsed_ms = millis(started.elapsed());
                return Ok(stats);
            }
            batch.clear();
            for (index, token) in chunk.iter().enumerate() {
                let pos = chunk_index * PREFILL_BATCH + index;
                batch
                    .add(*token, pos as i32, &[0], pos + 1 == tokens.len())
                    .map_err(|_| generation_failed())?;
            }
            ctx.decode(&mut batch).map_err(|_| generation_failed())?;
        }

        let mut sampler = if request.temperature == 0.0 {
            LlamaSampler::greedy()
        } else {
            LlamaSampler::chain_simple([
                LlamaSampler::top_k(request.top_k),
                LlamaSampler::top_p(request.top_p, 1),
                LlamaSampler::temp(request.temperature),
                LlamaSampler::dist(request.seed.unwrap_or(u32::MAX)),
            ])
        };

        let mut first = None;
        let mut pending = Vec::new();
        stats.reason = FinishReason::TokenLimit;
        for index in 0..request.max_tokens {
            if cancel.is_cancelled() {
                stats.reason = FinishReason::Cancelled;
                break;
            }
            let token = sampler.sample(&ctx, -1);
            if vocab.is_eog(token) {
                stats.reason = FinishReason::EndOfGeneration;
                break;
            }
            sampler.accept(token);
            first.get_or_insert_with(Instant::now);
            stats.generated_tokens += 1;
            pending.extend(vocab.token_to_piece(token, true, None));
            send_text(&mut pending, false, emit)?;
            if index + 1 < request.max_tokens && !cancel.is_cancelled() {
                batch.clear();
                batch
                    .add(token, (tokens.len() + index as usize) as i32, &[0], true)
                    .map_err(|_| generation_failed())?;
                ctx.decode(&mut batch).map_err(|_| generation_failed())?;
            }
        }
        send_text(&mut pending, true, emit)?;
        stats.elapsed_ms = millis(started.elapsed());
        if let Some(first) = first {
            stats.time_to_first_token_ms = Some(millis(first.duration_since(started)));
            stats.tokens_per_second = f64::from(stats.generated_tokens.saturating_sub(1))
                / first.elapsed().as_secs_f64().max(f64::EPSILON);
        }
        Ok(stats)
    }
}

fn render_template(template: &str, prompt: &str, bos: &str, eos: &str) -> Result<String> {
    let mut env = Environment::new();
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    env.set_fuel(Some(100_000));
    env.set_keep_trailing_newline(true);
    env.add_function(
        "raise_exception",
        |message: String| -> std::result::Result<(), minijinja::Error> {
            Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                message,
            ))
        },
    );
    env.render_str(
        template,
        context! {
            messages => [context! { role => "user", content => prompt }],
            add_generation_prompt => true,
            enable_thinking => false,
            bos_token => bos,
            eos_token => eos,
        },
    )
    .map_err(|_| unsupported_template())
}

fn generation_failed() -> InferenceError {
    InferenceError::new(
        ErrorCode::GenerationFailed,
        "llama.cpp could not evaluate the prompt or next token.",
    )
}

fn millis(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

fn send_text(
    pending: &mut Vec<u8>,
    final_chunk: bool,
    emit: &mut dyn FnMut(TextDelta) -> Result<()>,
) -> Result<()> {
    let mut consumed = 0;
    while let Err(error) = std::str::from_utf8(&pending[consumed..]) {
        consumed += error.valid_up_to();
        if let Some(length) = error.error_len() {
            consumed += length;
        } else {
            break;
        }
    }
    if final_chunk || std::str::from_utf8(&pending[consumed..]).is_ok() {
        consumed = pending.len();
    }
    if consumed > 0 {
        let text = String::from_utf8_lossy(&pending[..consumed]).into_owned();
        pending.drain(..consumed);
        emit(TextDelta { text })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_model_template_with_generation_prompt() {
        let template = "{{ bos_token }}{% for message in messages %}{{ message.role }}: {{ message.content.strip() }}{{ eos_token }}{% endfor %}{% if add_generation_prompt %}assistant: {% if not enable_thinking %}direct{% endif %}{% endif %}";
        assert_eq!(
            render_template(template, " Hi ", "<start>", "<end>").unwrap(),
            "<start>user: Hi<end>assistant: direct",
        );
        assert_eq!(
            render_template("{% invalid %}", "Hi", "", "")
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedTemplate,
        );
    }

    #[test]
    fn streams_complete_unicode_and_keeps_partial_sequences() {
        let mut chunks = Vec::new();
        let mut emit = |delta: TextDelta| {
            chunks.push(delta.text);
            Ok(())
        };
        let mut pending = vec![b'a', 0xf0, 0x9f];
        send_text(&mut pending, false, &mut emit).unwrap();
        pending.extend([0x98, 0x80]);
        send_text(&mut pending, false, &mut emit).unwrap();
        pending.extend([0xf0, 0x9f]);
        send_text(&mut pending, true, &mut emit).unwrap();
        assert_eq!(chunks, ["a", "😀", "�"]);
        assert!(pending.is_empty());
    }
}
