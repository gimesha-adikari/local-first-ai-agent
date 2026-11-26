use std::{
    num::NonZeroU32,
    path::Path,
    time::{Duration, Instant, UNIX_EPOCH},
};

use agent_core::agent::{DecisionRequest, ScopeKind, ToolId};
use agent_core::chat::{MessageRole, PromptMessage, fit_context};
use agent_core::inference::{
    AssistantReplay, Cancellation, ErrorCode, FinishReason, GenerationRequest, GenerationStats,
    InferenceError, InferenceProvider, MAX_CHAT_ATTEMPT_TOKENS, ModelConfig, ModelInfo,
    OutputBudget, Result, TextDelta,
};
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{LlamaModel, params::LlamaModelParams},
    sampling::LlamaSampler,
    token::LlamaToken,
};
use minijinja::{Environment, context};

const PREFILL_BATCH: usize = 64;
// The pinned binding does not expose native abort callbacks. Smaller Agent
// batches reduce the work between cancellation checks on the CPU worker.
const AGENT_PREFILL_BATCH: usize = 16;

// Limit agent decoding to the shape already accepted by the decision parser.
// The registry and permission engine still validate every proposed call.
const AGENT_DECISION_RULES: &str = r#"
root ::= ws "{" ws "\"kind\"" ws ":" ws (tool-call | final-answer) ws "}"
tool-call ::= "\"tool_call\"" ws "," ws "\"tool\"" ws ":" ws string ws "," ws "\"arguments\"" ws ":" ws object
final-answer ::= "\"final\"" ws "," ws "\"answer\"" ws ":" ws string
object ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}"
array ::= "[" ws (value (ws "," ws value)*)? ws "]"
value ::= object | array | string | number | "true" | "false" | "null"
string ::= "\"" ([^"\\\x00-\x1f] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F]{4}))* "\""
number ::= "-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? ([eE] [+-]? [0-9]+)?
"#;
// Qwen keeps free JSON whitespace. LFM2 is limited to single spaces so it
// cannot spend the decision budget on whitespace runs.
const STANDARD_WS: &str = "ws ::= [ \\t\\n\\r]*\n";
const COMPACT_WS: &str = "ws ::= \" \"?\n";

impl AgentFormat {
    /// Task 04-A: LFM2.5 produces valid single-step decisions, but multi-step
    /// scoped file workflows were not reliable, so Agent mode stays off.
    const fn released(self) -> bool {
        match self {
            Self::QwenChatMl => true,
            Self::Lfm2ChatMl => false,
        }
    }

    fn decision_style(self) -> agent_core::agent::DecisionStyle {
        match self {
            Self::QwenChatMl => agent_core::agent::DecisionStyle::Standard,
            Self::Lfm2ChatMl => agent_core::agent::DecisionStyle::Compact,
        }
    }

    /// LFM2 additionally restricts each call to a registered tool name and
    /// that tool's argument names (`{}` when it takes none). Values stay
    /// generic JSON and the registry still validates every argument.
    /// The released Qwen format constrains files.read against active grants.
    fn request_grammar(self, request: &DecisionRequest) -> String {
        let mut extra = String::new();
        let calls = request
            .tool_arguments()
            .enumerate()
            .filter_map(|(index, (tool, names))| {
                let arguments = if self == Self::QwenChatMl {
                    if tool == ToolId::FileRead {
                        extra.push_str(&read_rules(request)?);
                        "read-args".to_owned()
                    } else {
                        "object".to_owned()
                    }
                } else if names.is_empty() {
                    r#""{" ws "}""#.to_owned()
                } else {
                    let keys = names
                        .iter()
                        .filter(|name| name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                        .map(|name| format!(r#""\"{name}\"""#))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    extra.push_str(&format!(
                        "key-{index} ::= {keys}\nargs-{index} ::= \"{{\" ws (key-{index} ws \":\" ws value (ws \",\" ws key-{index} ws \":\" ws value)*)? ws \"}}\"\n"
                    ));
                    format!("args-{index}")
                };
                Some(format!(
                    r#""\"{}\"" ws "," ws "\"arguments\"" ws ":" ws {arguments}"#,
                    tool.as_str()
                ))
            })
            .collect::<Vec<_>>()
            .join(" | ");
        let rules = if calls.is_empty() {
            AGENT_DECISION_RULES.replace("(tool-call | final-answer)", "final-answer")
        } else {
            AGENT_DECISION_RULES.replace(
                r#""\"tool\"" ws ":" ws string ws "," ws "\"arguments\"" ws ":" ws object"#,
                &format!(r#""\"tool\"" ws ":" ws ({calls})"#),
            )
        };
        let ws = match self {
            Self::QwenChatMl => STANDARD_WS,
            Self::Lfm2ChatMl => COMPACT_WS,
        };
        format!("{rules}{extra}{ws}")
    }
}

fn read_rules(request: &DecisionRequest) -> Option<String> {
    let mut rules = String::new();
    let mut alternatives = Vec::new();
    for (kind, scope_rule, path) in [
        (ScopeKind::File, "file-scope", ""),
        (
            ScopeKind::Directory,
            "directory-scope",
            r#" ws "," ws "\"relative_path\"" ws ":" ws string"#,
        ),
    ] {
        let ids = request
            .scope_ids(kind)
            // DecisionRequest only exposes validated opaque identifiers.
            .map(|id| format!(r#""\"{id}\"""#))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }
        rules.push_str(&format!("{scope_rule} ::= {}\n", ids.join(" | ")));
        alternatives.push(format!("{scope_rule}{path}"));
    }
    if alternatives.is_empty() {
        return None;
    }
    rules.push_str(&format!(
        r#"read-args ::= "{{" ws "\"scope_id\"" ws ":" ws ({}) read-pages? ws "}}"
read-pages ::= ws "," ws "\"pdf_pages\"" ws ":" ws "{{" ws "\"first_page\"" ws ":" ws page-number ws "," ws "\"last_page\"" ws ":" ws page-number ws "}}"
page-number ::= [1-9] [0-9]*
"#,
        alternatives.join(" | ")
    ));
    Some(rules)
}

/// Owns the chat model on the dedicated inference worker.
#[derive(Default)]
pub struct LlamaProvider {
    loaded: Option<LoadedModel>,
    evaluate_unreleased_agent_formats: bool,
}

impl LlamaProvider {
    /// For opt-in real-model evaluation only. Also enables recognized Agent
    /// formats that are not yet released. The desktop app never uses this,
    /// and every decision is still validated, authorized and budgeted.
    pub fn for_agent_format_evaluation() -> Self {
        Self {
            evaluate_unreleased_agent_formats: true,
            ..Self::default()
        }
    }
}

struct LoadedModel {
    model: LlamaModel,
    config: ModelConfig,
    template: String,
    supports_thinking: bool,
    agent_format: Option<AgentFormat>,
    /// The recognized format is released (or under explicit evaluation).
    agent_enabled: bool,
    model_stamp: u64,
}

/// Prompt families whose actual constrained decisions were validated for the
/// Agent JSON protocol. Agent compatibility is independent of Thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentFormat {
    /// Qwen ChatML template with a switchable Thinking mode, used with Thinking off.
    QwenChatMl,
    /// LFM2 ChatML instruct template without a Thinking mode.
    Lfm2ChatMl,
}

const AGENT_PROBE_SYSTEM: &str = "Agent format check";
const AGENT_PROBE_USER: &str = "Template check";

/// Conservative compatibility gate, not a certification. `rendered` is the
/// direct-mode rendering of one system and one user probe message.
fn classify_agent_format(
    template: &str,
    architecture: &str,
    supports_thinking: bool,
    rendered: Option<&str>,
) -> Option<AgentFormat> {
    if !template.contains("<|im_start|>") || !template.contains("<|im_end|>") {
        return None;
    }
    if supports_thinking && template.contains("enable_thinking") {
        return Some(AgentFormat::QwenChatMl);
    }
    // LFM2 has no Thinking switch. Require the exact ChatML structure so the
    // template adds no hidden system text, tool list or reasoning prefix.
    let expected = format!(
        "<|im_start|>system\n{AGENT_PROBE_SYSTEM}<|im_end|>\n<|im_start|>user\n{AGENT_PROBE_USER}<|im_end|>\n<|im_start|>assistant\n"
    );
    let rendered = rendered?;
    let prefix = rendered.strip_suffix(expected.as_str())?;
    (architecture == "lfm2"
        && !supports_thinking
        && !prefix.contains("<|im_start|>")
        && !prefix.contains("<think>"))
    .then_some(AgentFormat::Lfm2ChatMl)
}

impl InferenceProvider for LlamaProvider {
    fn load(&mut self, path: &Path, config: ModelConfig) -> Result<ModelInfo> {
        self.unload();
        config.validate()?;
        let file_bytes = crate::gguf::validate(path)?;
        let start = Instant::now();
        let backend = crate::backend::cpu_backend()?;
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
        let modified = std::fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        let model_stamp = modified.map_or(file_bytes, |time| {
            file_bytes
                ^ time.as_secs().rotate_left(23)
                ^ u64::from(time.subsec_nanos()).rotate_left(11)
        });
        let mut loaded = LoadedModel {
            model,
            config,
            template,
            supports_thinking: false,
            agent_format: None,
            agent_enabled: false,
            model_stamp,
        };
        // Validate template compatibility before marking the model ready.
        let direct = loaded.render_prompt("Template check", false)?;
        loaded.supports_thinking = loaded
            .render_prompt("Template check", true)
            .is_ok_and(|thinking| thinking != direct);
        let architecture = loaded
            .model
            .meta_val_str("general.architecture")
            .unwrap_or_else(|_| "unknown".into());
        let probe = loaded
            .render_messages(
                &[
                    PromptMessage {
                        role: MessageRole::System,
                        content: AGENT_PROBE_SYSTEM,
                    },
                    PromptMessage {
                        role: MessageRole::User,
                        content: AGENT_PROBE_USER,
                    },
                ],
                false,
            )
            .ok();
        loaded.agent_format = classify_agent_format(
            &loaded.template,
            &architecture,
            loaded.supports_thinking,
            probe.as_deref(),
        );
        loaded.agent_enabled = loaded
            .agent_format
            .is_some_and(|format| format.released() || self.evaluate_unreleased_agent_formats);
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
            architecture,
            parameters: loaded.model.n_params(),
            file_name,
            file_bytes,
            trained_context,
            supports_thinking: loaded.supports_thinking,
            supports_agent: loaded.agent_enabled,
            config,
            load_ms: millis(start.elapsed()),
        };
        self.loaded = Some(loaded);
        Ok(info)
    }

    fn decide(
        &mut self,
        request: &agent_core::agent::DecisionRequest,
        cancellation: &Cancellation,
    ) -> Result<agent_core::agent::DecisionResponse> {
        let Some(loaded) = &self.loaded else {
            return Err(InferenceError::new(
                ErrorCode::NoModel,
                "Load a GGUF model first.",
            ));
        };
        let backend = crate::backend::cpu_backend()?;
        // Decisions always run with Thinking off and grammar-constrained JSON.
        let Some(format) = loaded.agent_format.filter(|_| loaded.agent_enabled) else {
            return Err(InferenceError::new(
                ErrorCode::UnsupportedTemplate,
                "The loaded model's chat template is not validated for Agent mode.",
            ));
        };
        let grammar_text = format.request_grammar(request);
        let mut text = String::new();
        let mut emit = |delta: TextDelta| {
            if text.len().saturating_add(delta.text.len()) > agent_core::agent::MAX_PROPOSAL_BYTES
                || delta.text.contains('\u{fffd}')
            {
                return Err(InferenceError::new(
                    ErrorCode::GenerationFailed,
                    "Invalid or oversized agent decision output.",
                ));
            }
            text.push_str(&delta.text);
            Ok(())
        };
        let mut generate = |preview| {
            let generation = request.styled_generation_request(format.decision_style(), preview)?;
            let grammar = LlamaSampler::grammar(&loaded.model, &grammar_text, "root")
                .map_err(|_| generation_failed())?;
            loaded.generate(backend, &generation, cancellation, &mut emit, Some(grammar))
        };
        let mut result = generate(None);
        if request.has_observations() {
            for limit in [512, 0] {
                if !matches!(&result, Err(error) if error.code == ErrorCode::ContextLimit) {
                    break;
                }
                // Context rejection happens before decode. Fit a smaller view
                // of the same results without rerunning inference or tools.
                result = generate(Some(limit));
            }
        }
        Ok(agent_core::agent::DecisionResponse {
            text,
            stats: result?,
        })
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
        let Some(loaded) = &self.loaded else {
            return Err(InferenceError::new(
                ErrorCode::NoModel,
                "Load a GGUF model first.",
            ));
        };
        loaded.generate(
            crate::backend::cpu_backend()?,
            request,
            cancellation,
            emit,
            None,
        )
    }
}

fn unsupported_template() -> InferenceError {
    InferenceError::new(
        ErrorCode::UnsupportedTemplate,
        "The model needs a supported embedded chat template.",
    )
}

impl LoadedModel {
    fn render_prompt(&self, prompt: &str, enable_thinking: bool) -> Result<String> {
        self.render_messages(
            &[PromptMessage {
                role: MessageRole::User,
                content: prompt,
            }],
            enable_thinking,
        )
    }

    fn render_messages(
        &self,
        messages: &[PromptMessage<'_>],
        enable_thinking: bool,
    ) -> Result<String> {
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
            messages,
            &special(vocab.bos()),
            &special(vocab.eos()),
            enable_thinking,
        )
    }

    fn generate(
        &self,
        backend: &LlamaBackend,
        request: &GenerationRequest,
        cancel: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
        grammar: Option<LlamaSampler>,
    ) -> Result<GenerationStats> {
        let started = Instant::now();
        let mut stats = GenerationStats {
            reason: FinishReason::Cancelled,
            prompt_tokens: 0,
            prompt_tokens_per_second: None,
            generated_tokens: 0,
            time_to_first_token_ms: None,
            elapsed_ms: 0,
            tokens_per_second: 0.0,
            excluded_turns: 0,
            output_budget: None,
            context_usage: None,
            assistant_replay: None,
        };
        if cancel.is_cancelled() {
            return Ok(stats);
        }

        if request.enable_thinking && !self.supports_thinking {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "The model's chat template does not expose a thinking mode.",
            ));
        }
        let vocab = self.model.vocab();
        let chat = request.chat.as_ref();
        let replay = chat.and_then(|chat| chat.replay.as_ref());
        let measure = |messages: &[PromptMessage<'_>]| {
            let prompt = self.render_messages(messages, request.enable_thinking)?;
            let prefilled_thinking =
                request.enable_thinking && prompt.trim_end().ends_with("<think>");
            let tokens = vocab.tokenize(prompt.as_bytes(), false, true);
            let count = tokens.len();
            Ok(((tokens, prefilled_thinking), count))
        };
        let context = self.config.context_length;
        let reserve = if let Some(seed) = replay {
            if seed.generated.is_empty()
                || seed.generated.len() >= MAX_CHAT_ATTEMPT_TOKENS
                || seed.utf8_tail.len() > 3
                || (!seed.utf8_tail.is_empty()
                    && !matches!(std::str::from_utf8(&seed.utf8_tail),
                    Err(error) if error.valid_up_to() == 0 && error.error_len().is_none()))
                || seed
                    .generated
                    .iter()
                    .any(|id| *id < 0 || *id >= vocab.n_tokens() || vocab.is_eog(LlamaToken(*id)))
            {
                return Err(replay_mismatch());
            }
            seed.generated.len() as u32 + 1
        } else if chat.is_some_and(|chat| chat.adaptive) {
            let share = if request.enable_thinking { 3 } else { 4 };
            request.max_tokens.min((context / share).max(1))
        } else {
            request.max_tokens
        };
        let augmented_history = request.context.as_ref().map(|context| {
            let mut history = request.history.clone();
            context.append_system_instructions(&mut history.system);
            history
        });
        let history = augmented_history.as_ref().unwrap_or(&request.history);
        let mut usage = request.context.as_ref().map(|context| context.usage());
        let fit = |output, usage| {
            let augmented;
            let latest = if let (Some(data), Some(usage)) = (&request.context, usage) {
                augmented = data.render(&request.prompt, usage);
                &augmented
            } else {
                &request.prompt
            };
            fit_context(history, latest, context, output, measure)
        };
        let ((mut tokens, prefilled_thinking), excluded_turns) = loop {
            match fit(reserve, usage) {
                Err(error) if error.code == ErrorCode::ContextLimit => {
                    if let Some(usage) = &mut usage {
                        if usage.memories > 0 {
                            usage.memories -= 1;
                            continue;
                        }
                        if usage.sources > 0 {
                            usage.sources -= 1;
                            continue;
                        }
                    }
                    if replay.is_none() && chat.is_some_and(|chat| chat.adaptive) {
                        break fit(1, usage)?;
                    }
                    return Err(error);
                }
                result => break result?,
            }
        };
        stats.context_usage = usage;
        let fingerprint = token_fingerprint(self.model_stamp, &tokens);
        let mut generated = Vec::new();
        let mut pending = Vec::new();
        if let Some(seed) = replay {
            if excluded_turns != 0 {
                return Err(InferenceError::new(
                    ErrorCode::ContextLimit,
                    "The original response prefix does not fit. Reload this model with a larger context.",
                ));
            }
            if fingerprint != seed.prompt_fingerprint {
                return Err(replay_mismatch());
            }
            tokens.extend(seed.generated.iter().copied().map(LlamaToken));
            generated.clone_from(&seed.generated);
            pending.clone_from(&seed.utf8_tail);
        }
        let effective = request
            .max_tokens
            .min(context.saturating_sub(tokens.len() as u32))
            .min((MAX_CHAT_ATTEMPT_TOKENS - generated.len()) as u32);
        if effective == 0 {
            return Err(InferenceError::new(
                ErrorCode::ContextLimit,
                "There is no model context left for more output.",
            ));
        }
        stats.prompt_tokens = tokens.len();
        stats.excluded_turns = replay.map_or(excluded_turns, |seed| seed.excluded_turns);
        if chat.is_some() {
            stats.output_budget = Some(OutputBudget {
                requested: request.max_tokens,
                effective,
                context_length: context,
            });
        }
        if cancel.is_cancelled() {
            stats.elapsed_ms = millis(started.elapsed());
            return Ok(stats);
        }

        let threads = self.config.threads as i32;
        let prefill_batch = if grammar.is_some() {
            AGENT_PREFILL_BATCH
        } else {
            PREFILL_BATCH
        };
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.config.context_length))
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_n_batch(prefill_batch as u32)
            .with_n_ubatch(prefill_batch as u32)
            .with_offload_kqv(false);
        let mut ctx = self
            .model
            .new_context(backend, params)
            .map_err(|_| generation_failed())?;
        let mut batch = LlamaBatch::new(prefill_batch, 1);
        let prefill = Instant::now();
        for (chunk_index, chunk) in tokens.chunks(prefill_batch).enumerate() {
            if cancel.is_cancelled() {
                stats.elapsed_ms = millis(started.elapsed());
                return Ok(stats);
            }
            batch.clear();
            for (index, token) in chunk.iter().enumerate() {
                let pos = chunk_index * prefill_batch + index;
                batch
                    .add(*token, pos as i32, &[0], pos + 1 == tokens.len())
                    .map_err(|_| generation_failed())?;
            }
            ctx.decode(&mut batch).map_err(|_| generation_failed())?;
        }
        if cancel.is_cancelled() {
            stats.elapsed_ms = millis(started.elapsed());
            return Ok(stats);
        }
        if let (Some(observer), Some(usage)) = (&request.context_observer, usage) {
            observer.decoded(usage)?;
        }
        stats.prompt_tokens_per_second =
            Some(tokens.len() as f64 / prefill.elapsed().as_secs_f64().max(f64::EPSILON));

        let sampler = if request.temperature == 0.0 {
            LlamaSampler::greedy()
        } else {
            LlamaSampler::chain_simple([
                LlamaSampler::top_k(request.top_k),
                LlamaSampler::top_p(request.top_p, 1),
                LlamaSampler::temp(request.temperature),
                LlamaSampler::dist(request.seed.unwrap_or(u32::MAX)),
            ])
        };
        let mut sampler = match grammar {
            Some(grammar) => LlamaSampler::chain_simple([grammar, sampler]),
            None => sampler,
        };

        let mut first = None;
        stats.reason = FinishReason::TokenLimit;
        for index in 0..effective {
            if cancel.is_cancelled() {
                stats.reason = FinishReason::Cancelled;
                break;
            }
            let token = sampler.sample(&ctx, -1);
            if vocab.is_eog(token) {
                stats.reason = FinishReason::EndOfGeneration;
                break;
            }
            first.get_or_insert_with(Instant::now);
            // Qwen3.5 places the opener in the prompt instead of the output.
            // Normalize it before streaming or persisting any reasoning text.
            if index == 0 && prefilled_thinking && replay.is_none() {
                emit(TextDelta {
                    text: "<think>\n".into(),
                })?;
            }
            stats.generated_tokens += 1;
            if chat.is_some() && request.context.is_none() {
                generated.push(token.0);
            }
            pending.extend(vocab.token_to_piece(token, true, None));
            send_text(&mut pending, false, emit)?;
            if index + 1 < effective && !cancel.is_cancelled() {
                batch.clear();
                batch
                    .add(token, (tokens.len() + index as usize) as i32, &[0], true)
                    .map_err(|_| generation_failed())?;
                ctx.decode(&mut batch).map_err(|_| generation_failed())?;
            }
        }
        if cancel.is_cancelled() {
            stats.reason = FinishReason::Cancelled;
        }
        if chat.is_some()
            && request.context.is_none()
            && stats.reason == FinishReason::TokenLimit
            && !generated.is_empty()
            && generated.len() < MAX_CHAT_ATTEMPT_TOKENS
        {
            stats.assistant_replay = Some(AssistantReplay {
                prompt_fingerprint: fingerprint,
                generated,
                utf8_tail: pending,
                excluded_turns: stats.excluded_turns,
            });
        } else {
            send_text(&mut pending, true, emit)?;
        }
        stats.elapsed_ms = millis(started.elapsed());
        if let Some(first) = first {
            stats.time_to_first_token_ms = Some(millis(first.duration_since(started)));
            stats.tokens_per_second = f64::from(stats.generated_tokens.saturating_sub(1))
                / first.elapsed().as_secs_f64().max(f64::EPSILON);
        }
        Ok(stats)
    }
}

fn replay_mismatch() -> InferenceError {
    InferenceError::new(
        ErrorCode::InvalidConfig,
        "The original model and prompt are required to continue this response.",
    )
}

fn token_fingerprint(model_stamp: u64, tokens: &[LlamaToken]) -> u64 {
    tokens
        .iter()
        .flat_map(|token| token.0.to_le_bytes())
        .fold(0xcbf29ce484222325 ^ model_stamp, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

fn render_template(
    template: &str,
    messages: &[PromptMessage<'_>],
    bos: &str,
    eos: &str,
    enable_thinking: bool,
) -> Result<String> {
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
            messages => messages.iter().map(|message| context! {
                role => message.role.as_str(), content => message.content
            }).collect::<Vec<_>>(),
            add_generation_prompt => true,
            enable_thinking => enable_thinking,
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
    #[ignore = "uses a local GGUF and runs longer Chat plus replay on its actual tokenizer"]
    fn real_chat_exact_assistant_replay() {
        use agent_core::inference::ChatGeneration;
        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 8192,
                    threads: 4,
                },
            )
            .unwrap();
        let mut request = GenerationRequest::new(
            "Write an extensive numbered list of 300 different practical gardening tasks. Put one complete sentence on every numbered line, with no conclusion until number 300.",
        );
        request.max_tokens = 48;
        request.temperature = 0.0;
        request.chat = Some(ChatGeneration {
            adaptive: true,
            replay: None,
        });
        let mut reply = String::new();
        let first = provider
            .generate(&request, &Cancellation::default(), &mut |delta| {
                reply.push_str(&delta.text);
                Ok(())
            })
            .unwrap();
        assert_eq!(first.reason, FinishReason::TokenLimit);
        assert_eq!(first.generated_tokens, 48);
        assert_eq!(first.output_budget.as_ref().unwrap().effective, 48);
        let mut prefix = first.assistant_replay.unwrap();
        let original = reply.clone();
        println!(
            "initial output={} prompt={} elapsed_ms={} tokens_per_second={:.2}",
            first.generated_tokens, first.prompt_tokens, first.elapsed_ms, first.tokens_per_second
        );
        for expected in [80, 112] {
            let start = reply.len();
            request.max_tokens = 32;
            request.chat = Some(ChatGeneration {
                adaptive: true,
                replay: Some(prefix.clone()),
            });
            let stats = provider
                .generate(&request, &Cancellation::default(), &mut |delta| {
                    reply.push_str(&delta.text);
                    Ok(())
                })
                .unwrap();
            assert_eq!(stats.reason, FinishReason::TokenLimit);
            let restored = stats.assistant_replay.unwrap();
            assert!(restored.generated.starts_with(&prefix.generated));
            assert_eq!(restored.generated.len(), expected);
            assert!(reply.starts_with(&original));
            assert!(!reply[start..].starts_with(&original[..original.floor_char_boundary(64)]));
            println!(
                "segment: total={} delta={} prefill_tokens={} first_ms={:?}",
                expected, stats.generated_tokens, stats.prompt_tokens, stats.time_to_first_token_ms
            );
            prefix = restored;
        }
        let cancel = Cancellation::default();
        let before = reply.len();
        request.chat = Some(ChatGeneration {
            adaptive: true,
            replay: Some(prefix),
        });
        let result = provider
            .generate(&request, &cancel, &mut |delta| {
                reply.push_str(&delta.text);
                cancel.cancel();
                Ok(())
            })
            .unwrap();
        assert_eq!(result.reason, FinishReason::Cancelled);
        assert!(reply.starts_with(&original) && reply.len() > before);
        println!(
            "cancelled continuation after {} new token(s)",
            result.generated_tokens
        );
    }

    #[test]
    #[ignore = "uses a local GGUF to verify reasoning and context boundaries"]
    fn real_chat_replay_reasoning_and_context_bounds() {
        use agent_core::{chat::CompletedTurn, inference::ChatGeneration};
        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 2048,
                    threads: 4,
                },
            )
            .unwrap();
        let mut request =
            GenerationRequest::new("What is 2 plus 2? Think briefly, then give the answer.");
        request.max_tokens = 8;
        request.temperature = 0.0;
        request.enable_thinking = true;
        request.chat = Some(ChatGeneration {
            adaptive: true,
            replay: None,
        });
        let mut output = String::new();
        let stats = provider
            .generate(&request, &Cancellation::default(), &mut |delta| {
                output.push_str(&delta.text);
                Ok(())
            })
            .unwrap();
        let prior = output.clone();
        assert!(prior.starts_with("<think>"));
        assert_eq!(stats.reason, FinishReason::TokenLimit);
        request.max_tokens = 384;
        request.chat = Some(ChatGeneration {
            adaptive: true,
            replay: stats.assistant_replay,
        });
        let stats = provider
            .generate(&request, &Cancellation::default(), &mut |delta| {
                output.push_str(&delta.text);
                Ok(())
            })
            .unwrap();
        assert!(output.starts_with(&prior));
        assert_eq!(output.matches("<think>").count(), 1);
        if let Some((_, answer)) = output.split_once("</think>") {
            assert!(answer.contains('4'));
        }
        println!(
            "reasoning replay reason={:?} new_tokens={} final_delimiter={}",
            stats.reason,
            stats.generated_tokens,
            output.contains("</think>")
        );
        let mut long = GenerationRequest::new("Say hello.");
        long.max_tokens = 768;
        long.chat = Some(ChatGeneration {
            adaptive: true,
            replay: None,
        });
        long.history.turns = (0..12)
            .map(|index| CompletedTurn {
                user: format!("Question {index}: {}", "orchard ".repeat(200)),
                assistant: "rainwater ".repeat(200),
            })
            .collect();
        let stats = provider
            .generate(&long, &Cancellation::default(), &mut |_| Ok(()))
            .unwrap();
        let output_budget = stats.output_budget.unwrap();
        assert!(stats.excluded_turns > 0);
        assert!(stats.prompt_tokens + output_budget.effective as usize <= 2048);
        assert!(output_budget.effective <= 768);
        println!(
            "history excluded={} prompt={} actual_output_budget={}",
            stats.excluded_turns, stats.prompt_tokens, output_budget.effective
        );
        long.prompt = "a b c d e f g h i j ".repeat(450);
        assert_eq!(
            provider
                .generate(&long, &Cancellation::default(), &mut |_| panic!(
                    "overfull context streamed"
                ))
                .unwrap_err()
                .code,
            ErrorCode::ContextLimit
        );
    }

    #[test]
    #[ignore = "verifies longer direct output from a supplied local GGUF"]
    fn real_chat_output_beyond_768() {
        use agent_core::inference::ChatGeneration;
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let mut provider = LlamaProvider::default();
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 8192,
                    threads: 4,
                },
            )
            .unwrap();
        for prompt in [
            "Write an original 1800-word adventure story about a forest survey team exploring an abandoned mountain weather station. Write ten detailed scenes with dialogue, sensory descriptions and a subplot; do not stop after a synopsis.",
            "Produce a complete educational Rust source file for a small CPU ray tracer. Implement 3D vectors, colors, random sampling, rays, sphere intersections, a diffuse and metal material, camera projection, supersampling, PPM output, and at least six unit tests. Include the whole code, with no placeholders.",
        ] {
            let mut request = GenerationRequest::new(prompt);
            request.max_tokens = 1024;
            request.temperature = 0.0;
            request.chat = Some(ChatGeneration {
                adaptive: true,
                replay: None,
            });
            let mut bytes = 0;
            let result = provider
                .generate(&request, &Cancellation::default(), &mut |delta| {
                    bytes += delta.text.len();
                    Ok(())
                })
                .unwrap();
            println!(
                "output={} effective={} bytes={} reason={:?} elapsed_ms={} generation_tps={:.2}",
                result.generated_tokens,
                result.output_budget.as_ref().unwrap().effective,
                bytes,
                result.reason,
                result.elapsed_ms,
                result.tokens_per_second
            );
            if result.generated_tokens > 768 {
                return;
            }
        }
        panic!("neither real direct reply exceeded 768 tokens");
    }

    #[test]
    #[ignore = "measures only fixed synthetic Chat context on a supplied local GGUF"]
    fn real_chat_context_profile() {
        use agent_core::inference::ChatGeneration;
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let context: u32 = std::env::var("AGENT_BENCH_CONTEXT")
            .unwrap()
            .parse()
            .unwrap();
        let mut provider = LlamaProvider::default();
        let info = provider
            .load(
                &path,
                ModelConfig {
                    context_length: context,
                    threads: 4,
                },
            )
            .unwrap();
        let marker = "A small synthetic orchard collects rainwater and grows apples. ";
        let mut request = GenerationRequest::new(format!(
            "Read these synthetic repeated notes then write a detailed numbered summary of the topic. {} Start the summary.",
            marker.repeat(45)
        ));
        request.max_tokens = 128;
        request.temperature = 0.0;
        request.chat = Some(ChatGeneration {
            adaptive: true,
            replay: None,
        });
        let result = provider
            .generate(&request, &Cancellation::default(), &mut |_| Ok(()))
            .unwrap();
        println!(
            "context={} model_load_ms={} prompt={} first_ms={:?} prompt_tps={:?} output={} tps={:.2} elapsed_ms={}",
            context,
            info.load_ms,
            result.prompt_tokens,
            result.time_to_first_token_ms,
            result.prompt_tokens_per_second,
            result.generated_tokens,
            result.tokens_per_second,
            result.elapsed_ms
        );
        assert!(result.generated_tokens > 0);
        assert!(result.prompt_tokens + result.generated_tokens as usize <= context as usize);
    }

    /// Task 06-A manual review cases through the router, the evidence prompt
    /// and native generation. Model-dependent wording is printed; assertions
    /// check only grounding properties.
    #[test]
    #[ignore = "routes the Task 06-A review cases through the local GGUF in AGENT_TEST_MODEL"]
    fn real_routed_document_chat() {
        use agent_core::{
            chat_context::{ChatContext, ChatIntent, EvidenceTask, Grounding},
            chat_route::route,
            inference::ChatGeneration,
        };
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let mut provider = LlamaProvider::default();
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 4096,
                    threads: 4,
                },
            )
            .unwrap();
        let passages = [
            "Orchard Visitor Guide. The orchard gate opens at nine and closes at seven. Visitors may pick apples in rows A to D. Run the hand pump only at the well marked with a blue flag.",
            "Orchard Visitor Guide, page 2. Parking is free on weekdays. Dogs must stay on a lead. Report lost items at the farm shop.",
        ];
        let cases = [
            ("fins run in this selected file", ChatIntent::Search),
            ("what it contain", ChatIntent::Overview),
            ("what it do", ChatIntent::Overview),
            ("Write a four-line poem about rain", ChatIntent::General),
        ];
        for (question, expected) in cases {
            let plan = route(question, None, Grounding::Auto);
            let (task, evidence) = match plan.intent {
                ChatIntent::Search => (
                    EvidenceTask::Search {
                        target: plan.query.clone(),
                    },
                    passages
                        .iter()
                        .filter(|text| text.to_lowercase().contains(&plan.query))
                        .map(|text| text.to_string())
                        .collect::<Vec<_>>(),
                ),
                ChatIntent::Overview => (
                    EvidenceTask::Overview { total_chunks: 2 },
                    passages.iter().map(|text| text.to_string()).collect(),
                ),
                // No passage is relevant to the poem, so automatic grounding
                // answers it as ordinary Chat.
                _ => (EvidenceTask::Answer, Vec::new()),
            };
            let intent = if plan.intent == ChatIntent::Question && evidence.is_empty() {
                ChatIntent::General
            } else {
                plan.intent
            };
            assert_eq!(intent, expected, "{question}");
            let mut request = GenerationRequest::new(question);
            request.max_tokens = 160;
            request.temperature = 0.0;
            request.chat = Some(ChatGeneration {
                adaptive: false,
                replay: None,
            });
            if intent != ChatIntent::General {
                request.context = Some(ChatContext {
                    evidence: Some(evidence.clone()),
                    memories: Vec::new(),
                    task,
                });
            }
            let mut answer = String::new();
            provider
                .generate(&request, &Cancellation::default(), &mut |delta| {
                    answer.push_str(&delta.text);
                    Ok(())
                })
                .unwrap();
            // Drop any reasoning block; only the visible answer is checked.
            let answer = answer
                .rsplit_once("</think>")
                .map_or(answer.as_str(), |(_, visible)| visible)
                .trim()
                .to_owned();
            println!(
                "--- {question:?} -> {intent:?} ({} passages)\n{answer}",
                evidence.len()
            );
            let lower = answer.to_lowercase();
            assert!(!answer.trim().is_empty(), "{question}");
            assert!(
                !lower.contains("cannot answer") && !lower.contains("no document evidence"),
                "{question}: {answer}"
            );
            match intent {
                ChatIntent::Overview => assert!(lower.contains("orchard"), "{answer}"),
                ChatIntent::Search => {
                    assert!(lower.contains("run") || lower.contains("[s1]"), "{answer}")
                }
                // An ordinary creative reply: several lines, no evidence refusal.
                _ => assert!(
                    answer
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count()
                        >= 3,
                    "{answer}"
                ),
            }
        }
    }

    /// Task 06-C: a refinement re-answers the previous document question from
    /// current evidence, with an ordinary answer in the conversation that must
    /// not leak into the evidence turn.
    #[test]
    #[ignore = "checks Task 06-C refinements with the local GGUF in AGENT_TEST_MODEL"]
    fn real_refinement_uses_current_evidence_only() {
        use agent_core::{
            chat::{ChatHistory, CompletedTurn},
            chat_context::{ChatContext, EvidenceTask, Grounding},
            chat_route::{route, routed_question},
            inference::ChatGeneration,
        };
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let mut provider = LlamaProvider::default();
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 4096,
                    threads: 4,
                },
            )
            .unwrap();
        let previous = "Which port does the backend use, and how do I start it?";
        let evidence = vec![
            "Backend guide. The backend listens on port 8080. Start it from the backend folder with bash run_dev.sh. Logs are written to logs/api.log, and the health check is at /healthz."
                .to_owned(),
        ];
        let plan = route("make it shorter", Some(previous), Grounding::Documents);
        assert!(plan.refinement.is_some());
        let mut request =
            GenerationRequest::new(routed_question(&plan, "make it shorter", Some(previous)));
        request.max_tokens = 120;
        request.temperature = 0.0;
        request.chat = Some(ChatGeneration {
            adaptive: false,
            replay: None,
        });
        request.context = Some(ChatContext {
            evidence: Some(evidence),
            memories: Vec::new(),
            task: EvidenceTask::Answer,
        });
        // The host clears earlier turns for evidence answers. Keep the ordinary
        // poem out exactly as the host does.
        let ordinary = CompletedTurn {
            user: "Write a four-line poem about rain".into(),
            assistant: "Soft rain falls on the quiet ground.".into(),
        };
        let mut history = ChatHistory {
            system: String::new(),
            turns: vec![ordinary],
        };
        history.turns.clear();
        request.history = history;
        let mut answer = String::new();
        provider
            .generate(&request, &Cancellation::default(), &mut |delta| {
                answer.push_str(&delta.text);
                Ok(())
            })
            .unwrap();
        let answer = answer
            .rsplit_once("</think>")
            .map_or(answer.as_str(), |(_, visible)| visible)
            .trim()
            .to_owned();
        println!("--- refinement\n{answer}");
        assert!(answer.contains("8080"), "{answer}");
        assert!(!answer.to_lowercase().contains("rain"), "{answer}");
        assert!(answer.split_whitespace().count() < 60, "{answer}");
    }

    #[test]
    #[ignore = "reports only synthetic PDF search decisions with a local model"]
    fn synthetic_pdf_search_decision_probe() {
        use agent_core::agent::{
            DecisionRequest, ScopeKind, ScopeState, ScopeSummary, ToolRegistry, parse_decision,
        };
        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 4096,
                    threads: 4,
                },
            )
            .unwrap();
        let registry = ToolRegistry::mvp().unwrap();
        let scope = ScopeSummary {
            id: "fixture_scope".into(),
            kind: ScopeKind::Directory,
            state: ScopeState::Active,
            remaining_ms: 300000,
            display_name: Some("Synthetic mixed files".into()),
        };
        let mut request = DecisionRequest::new(
            "Find PDF files in the selected folder.",
            &registry,
            &[scope],
            512,
        )
        .unwrap();
        request.set_conversation_context(std::iter::empty());
        let result = provider.decide(&request, &Cancellation::default()).unwrap();
        println!(
            "synthetic decision: {} | validation: {:?}",
            result.text,
            parse_decision(&registry, &result)
        );
    }

    #[test]
    fn renders_the_model_template_with_generation_prompt() {
        let template = "{{ bos_token }}{% for message in messages %}{{ message.role }}: {{ message.content.strip() }}{{ eos_token }}{% endfor %}{% if add_generation_prompt %}assistant: {% if not enable_thinking %}direct{% endif %}{% endif %}";
        let messages = [PromptMessage {
            role: MessageRole::User,
            content: " Hi ",
        }];
        assert_eq!(
            render_template(template, &messages, "<start>", "<end>", false).unwrap(),
            "<start>user: Hi<end>assistant: direct",
        );
        assert_eq!(
            render_template(template, &messages, "<start>", "<end>", true).unwrap(),
            "<start>user: Hi<end>assistant: ",
        );
        assert_eq!(
            render_template("{% invalid %}", &messages, "", "", false)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedTemplate,
        );
    }

    const CHATML_PROBE: &str = "{{ bos_token }}{% for message in messages %}{{ '<|im_start|>' + message.role + '\n' + message.content + '<|im_end|>\n' }}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}";

    fn probe_messages() -> [PromptMessage<'static>; 2] {
        [
            PromptMessage {
                role: MessageRole::System,
                content: AGENT_PROBE_SYSTEM,
            },
            PromptMessage {
                role: MessageRole::User,
                content: AGENT_PROBE_USER,
            },
        ]
    }

    #[test]
    fn agent_format_is_independent_of_thinking_and_family_bound() {
        let rendered = render_template(
            CHATML_PROBE,
            &probe_messages(),
            "<|startoftext|>",
            "",
            false,
        )
        .unwrap();
        assert_eq!(
            classify_agent_format(CHATML_PROBE, "lfm2", false, Some(&rendered)),
            Some(AgentFormat::Lfm2ChatMl)
        );
        // Unvalidated families and Thinking-capable templates without the
        // Qwen switch stay unavailable.
        assert_eq!(
            classify_agent_format(CHATML_PROBE, "llama", false, Some(&rendered)),
            None
        );
        assert_eq!(
            classify_agent_format(CHATML_PROBE, "lfm2", true, Some(&rendered)),
            None
        );
        assert_eq!(
            classify_agent_format(CHATML_PROBE, "lfm2", false, None),
            None
        );
        // Recognition is separate from release: LFM2 Agent mode stays off.
        assert!(AgentFormat::QwenChatMl.released() && !AgentFormat::Lfm2ChatMl.released());
        // Qwen keeps its original gate.
        let qwen = format!("{CHATML_PROBE}{{# enable_thinking #}}");
        assert_eq!(
            classify_agent_format(&qwen, "qwen35", true, None),
            Some(AgentFormat::QwenChatMl)
        );
        assert_eq!(classify_agent_format(&qwen, "qwen35", false, None), None);
        assert_eq!(
            classify_agent_format("{{ messages }}", "lfm2", false, Some(&rendered)),
            None
        );
    }

    #[test]
    fn lfm2_grammar_restricts_tool_names_and_empty_arguments() {
        use agent_core::agent::{DecisionRequest, ToolRegistry};
        let registry = ToolRegistry::mvp().unwrap();
        let request = DecisionRequest::new("Hello", &registry, &[], 64).unwrap();
        let lfm = AgentFormat::Lfm2ChatMl.request_grammar(&request);
        assert!(!lfm.contains(r#""\"tool\"" ws ":" ws string ws"#));
        assert!(
            lfm.contains(r#""\"device.info\"" ws "," ws "\"arguments\"" ws ":" ws "{" ws "}""#)
        );
        assert!(lfm.contains(r#""\"math.calculate\"" ws "," ws "\"arguments\"" ws ":" ws args-0"#));
        assert!(lfm.contains(r#"key-0 ::= "\"expression\"""#));
        assert!(lfm.contains(r#""\"scope_id\"""#) && !lfm.contains(r#""\"properties\"""#));
        assert!(lfm.ends_with("ws ::= \" \"?\n"));
    }

    #[test]
    fn qwen_read_grammar_binds_paths_to_the_active_scope_kind() {
        use agent_core::agent::{ScopeState, ScopeSummary, ToolRegistry};

        let scope = |id: &str, kind, state| ScopeSummary {
            id: id.into(),
            kind,
            state,
            display_name: None,
            remaining_ms: 1000,
        };
        let file = scope("one_file", ScopeKind::File, ScopeState::Active);
        let directory = scope("one_folder", ScopeKind::Directory, ScopeState::Active);
        let revoked = scope("revoked_file", ScopeKind::File, ScopeState::Revoked);
        let expired = scope("expired_folder", ScopeKind::Directory, ScopeState::Expired);
        let registry = ToolRegistry::mvp().unwrap();
        let mut request = DecisionRequest::new("Read page five.", &registry, &[], 64).unwrap();
        let format = AgentFormat::QwenChatMl;
        assert!(!format.request_grammar(&request).contains("files.read"));

        request.update_scopes(std::slice::from_ref(&file));
        let grammar = format.request_grammar(&request);
        assert!(grammar.contains(r#"file-scope ::= "\"one_file\"""#));
        assert!(grammar.contains("(file-scope) read-pages?"));
        assert!(!grammar.contains("relative_path"));

        request.update_scopes(std::slice::from_ref(&directory));
        let grammar = format.request_grammar(&request);
        assert!(!grammar.contains("file-scope"));
        assert!(grammar.contains(
            r#"(directory-scope ws "," ws "\"relative_path\"" ws ":" ws string) read-pages?"#
        ));

        request.update_scopes(&[file, directory, revoked, expired]);
        let grammar = format.request_grammar(&request);
        assert!(grammar.contains(r#"directory-scope ::= "\"one_folder\"""#));
        assert!(grammar.contains(r#"(file-scope | directory-scope ws "," ws "\"relative_path\"" ws ":" ws string) read-pages?"#));
        assert!(grammar.contains(r#""\"pdf_pages\"""#));
        assert!(!grammar.contains("revoked_file") && !grammar.contains("expired_folder"));
        assert!(grammar.ends_with(STANDARD_WS));
    }

    #[test]
    fn lfm2_gate_rejects_hidden_template_content() {
        let tool_list = "{{ bos_token }}<|im_start|>system\nList of tools: []<|im_end|>\n{% for message in messages %}{{ '<|im_start|>' + message.role + '\n' + message.content + '<|im_end|>\n' }}{% endfor %}{{ '<|im_start|>assistant\n' }}";
        let think = format!("{CHATML_PROBE}<think>");
        for template in [tool_list, think.as_str()] {
            let rendered = render_template(template, &probe_messages(), "", "", false).unwrap();
            assert_eq!(
                classify_agent_format(template, "lfm2", false, Some(&rendered)),
                None,
                "{template}"
            );
        }
    }

    fn peak_rss_mib() -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .and_then(|value| {
                value
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .map(|kib| kib / 1024)
    }

    #[test]
    #[ignore = "uses AGENT_TEST_MODEL (LFM2.5) and AGENT_TEST_MODEL_ALT (Qwen3.5-4B)"]
    fn lfm2_chat_recall_and_model_switching() {
        use agent_core::{
            agent::{AgentDecision, DecisionRequest, ToolId, ToolRegistry, parse_decision},
            chat::{ChatHistory, CompletedTurn},
            inference::ChatGeneration,
        };
        let lfm = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let qwen = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL_ALT").unwrap());
        let config = ModelConfig {
            context_length: 4096,
            threads: 4,
        };
        let mut provider = LlamaProvider::default();
        let chat = |provider: &mut LlamaProvider, prompt: &str, turns: Vec<CompletedTurn>| {
            let mut request = GenerationRequest::new(prompt);
            request.temperature = 0.0;
            request.max_tokens = 96;
            request.history = ChatHistory {
                system: String::new(),
                turns,
            };
            request.chat = Some(ChatGeneration {
                adaptive: true,
                replay: None,
            });
            let mut text = String::new();
            let stats = provider
                .generate(&request, &Cancellation::default(), &mut |delta| {
                    text.push_str(&delta.text);
                    Ok(())
                })
                .unwrap();
            println!(
                "chat prompt={prompt:?} prompt_tokens={} generated={} first_ms={:?} tok_s={:.1} reason={:?} reply={text:?}",
                stats.prompt_tokens,
                stats.generated_tokens,
                stats.time_to_first_token_ms,
                stats.tokens_per_second,
                stats.reason
            );
            text
        };

        let info = provider.load(&lfm, config).unwrap();
        println!(
            "lfm load_ms={} thinking={} agent={} rss_mib={:?}",
            info.load_ms,
            info.supports_thinking,
            info.supports_agent,
            peak_rss_mib()
        );
        assert!(!info.supports_thinking && !info.supports_agent);
        let first = chat(
            &mut provider,
            "My name is Ravi and my favourite colour is teal. Reply with one short sentence.",
            Vec::new(),
        );
        let recall = chat(
            &mut provider,
            "What is my name and favourite colour?",
            vec![CompletedTurn {
                user: "My name is Ravi and my favourite colour is teal. Reply with one short sentence."
                    .into(),
                assistant: first,
            }],
        );
        let recall = recall.to_lowercase();
        assert!(
            recall.contains("ravi") && recall.contains("teal"),
            "{recall}"
        );

        provider.unload();
        let info = provider.load(&qwen, config).unwrap();
        println!(
            "qwen load_ms={} arch={} thinking={} agent={} rss_mib={:?}",
            info.load_ms,
            info.architecture,
            info.supports_thinking,
            info.supports_agent,
            peak_rss_mib()
        );
        assert!(info.supports_thinking && info.supports_agent);
        assert_eq!(
            provider.loaded.as_ref().unwrap().agent_format,
            Some(AgentFormat::QwenChatMl)
        );
        let registry = ToolRegistry::mvp().unwrap();
        let request = DecisionRequest::new(
            "Use the local calculator to work out 137 times 89. Report the result.",
            &registry,
            &[],
            512,
        )
        .unwrap();
        let started = Instant::now();
        let reply = provider.decide(&request, &Cancellation::default()).unwrap();
        println!(
            "qwen decision elapsed_ms={} text={:?}",
            started.elapsed().as_millis(),
            reply.text
        );
        assert!(matches!(
            parse_decision(&registry, &reply),
            Ok(AgentDecision::Tool(call)) if call.tool() == ToolId::Calculate
        ));

        provider.unload();
        let info = provider.load(&lfm, config).unwrap();
        assert!(!info.supports_agent);
        let reply = chat(
            &mut provider,
            "What is 2 plus 2? Answer with the number.",
            Vec::new(),
        );
        assert!(reply.contains('4'), "{reply}");
        println!("final rss_mib={:?}", peak_rss_mib());
    }

    #[test]
    #[ignore = "validates actual LFM2.5 Agent decisions with the local GGUF in AGENT_TEST_MODEL"]
    fn lfm2_agent_decisions() {
        use agent_core::agent::{
            AgentDecision, DecisionRequest, DecisionStep, ScopeKind, ScopeState, ScopeSummary,
            ToolId, ToolObservation, ToolOutput, ToolRegistry, parse_decision,
        };

        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let config = ModelConfig {
            context_length: 4096,
            threads: 4,
        };
        // The released desktop provider keeps Agent mode off for LFM2.5.
        let mut released = LlamaProvider::default();
        let info = released.load(&path, config).unwrap();
        assert!(!info.supports_agent && !info.supports_thinking);
        let registry = ToolRegistry::mvp().unwrap();
        let probe = DecisionRequest::new("Say hello", &registry, &[], 64).unwrap();
        assert_eq!(
            released
                .decide(&probe, &Cancellation::default())
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedTemplate
        );
        drop(released);

        let mut provider = LlamaProvider::for_agent_format_evaluation();
        let info = provider.load(&path, config).unwrap();
        println!(
            "loaded architecture={} params={} load_ms={} thinking={} agent={} rss_mib={:?}",
            info.architecture,
            info.parameters,
            info.load_ms,
            info.supports_thinking,
            info.supports_agent,
            peak_rss_mib()
        );
        assert_eq!(info.architecture, "lfm2");
        assert!(!info.supports_thinking && info.supports_agent);
        assert_eq!(
            provider.loaded.as_ref().unwrap().agent_format,
            Some(AgentFormat::Lfm2ChatMl)
        );

        // Thinking stays disabled for this model.
        let mut thinking = GenerationRequest::new("Hello");
        thinking.enable_thinking = true;
        assert_eq!(
            provider
                .generate(&thinking, &Cancellation::default(), &mut |_| Ok(()))
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );

        let scope = ScopeSummary {
            display_name: None,
            id: "synthetic_scope_1".into(),
            kind: ScopeKind::Directory,
            state: ScopeState::Active,
            remaining_ms: 3_600_000,
        };
        let calc = "Use the local calculator to work out 137 times 89. Report the result.";
        let cases: [(&str, &str, bool, Option<ToolId>, &str); 8] = [
            (
                "conversation_search",
                "Find .txt files whose names contain ledger in the selected folder. Report the names in native order.",
                true,
                Some(ToolId::FileSearch),
                "",
            ),
            ("calculate", calc, true, Some(ToolId::Calculate), ""),
            (
                "device",
                "Inspect this device using the available tool. What operating system is it running?",
                true,
                Some(ToolId::DeviceInfo),
                "",
            ),
            (
                "search",
                "Find text files whose names contain ledger in the authorized directory.",
                true,
                Some(ToolId::FileSearch),
                "",
            ),
            (
                "read",
                "Read ledger.txt from the authorized directory and report its fixture code.",
                true,
                Some(ToolId::FileRead),
                "",
            ),
            ("after_calculate", calc, true, None, "12193"),
            (
                "greeting",
                "Say hello in one short sentence.",
                true,
                None,
                "",
            ),
            (
                "no_scope",
                "Read notes.txt from my Documents folder and show its contents.",
                false,
                None,
                "",
            ),
        ];
        let mut failures = Vec::new();
        let mut generated = 0u32;
        let mut decode_ms = 0u64;
        for (case, prompt, with_scope, expected_tool, expected_text) in cases {
            let scopes = if with_scope {
                std::slice::from_ref(&scope)
            } else {
                &[]
            };
            let mut request = DecisionRequest::new(prompt, &registry, scopes, 512).unwrap();
            if case == "conversation_search" {
                // The desktop Agent always supplies (possibly empty) history.
                let named = ScopeSummary {
                    display_name: Some("ledgers".into()),
                    ..scope.clone()
                };
                request.update_scopes(std::slice::from_ref(&named));
                request.set_conversation_context(std::iter::empty());
            }
            if case == "after_calculate" {
                let call = registry
                    .validate(ToolId::Calculate, r#"{"expression":"137*89"}"#)
                    .unwrap();
                let observation =
                    ToolObservation::success(&call, ToolOutput::Calculate { value: 12193.0 })
                        .unwrap();
                request
                    .push(DecisionStep::Observation {
                        call,
                        observation: Box::new(observation),
                    })
                    .unwrap();
            }
            let started = Instant::now();
            let reply = provider.decide(&request, &Cancellation::default()).unwrap();
            let elapsed = started.elapsed().as_millis();
            generated += reply.stats.generated_tokens;
            decode_ms += reply.stats.elapsed_ms;
            let decision = parse_decision(&registry, &reply);
            println!(
                "{case}: prompt_tokens={} generated={} elapsed_ms={elapsed} tok_s={:.1} end={:?} response={:?}",
                reply.stats.prompt_tokens,
                reply.stats.generated_tokens,
                reply.stats.tokens_per_second,
                reply.stats.reason,
                reply.text.chars().take(300).collect::<String>(),
            );
            let ok = match (&decision, expected_tool) {
                (Ok(AgentDecision::Tool(call)), Some(tool)) => {
                    call.tool() == tool
                        && (!matches!(tool, ToolId::FileSearch | ToolId::FileRead)
                            || call.scope_id() == Some("synthetic_scope_1"))
                }
                (Ok(AgentDecision::Final(answer)), None) => answer
                    .replace(',', "")
                    .to_lowercase()
                    .contains(expected_text),
                // Without a scope, a proposed file call must still name an
                // inactive scope, which the permission engine denies.
                (Ok(AgentDecision::Tool(call)), None) if case == "no_scope" => {
                    call.scope_id() != Some("synthetic_scope_1")
                }
                // A rejected proposal executes nothing; the runtime corrects it.
                (Err(_), None) if case == "no_scope" => true,
                _ => false,
            };
            if !ok {
                failures.push((case, format!("{decision:?}")));
            }
        }
        println!(
            "decisions: generated={generated} total_decode_ms={decode_ms} rss_mib={:?}",
            peak_rss_mib()
        );
        assert!(failures.is_empty(), "LFM2.5 decisions failed: {failures:?}");
    }

    #[test]
    #[ignore = "requires the pinned local GGUF via AGENT_TEST_MODEL"]
    fn renders_pinned_qwen_template_in_both_modes() {
        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        provider.load(&path, ModelConfig::default()).unwrap();
        let model = provider.loaded.as_ref().unwrap();
        let prefix = "<|im_start|>user\n你好<|im_end|>\n<|im_start|>assistant\n";
        assert_eq!(model.render_prompt("你好", true).unwrap(), prefix);
        assert_eq!(
            model.render_prompt("你好", false).unwrap(),
            format!("{prefix}<think>\n\n</think>\n\n"),
        );
    }

    #[test]
    #[ignore = "runs synthetic agent prompts through the local model supplied in AGENT_TEST_MODEL"]
    fn qwen_agent_decision_diagnostics() {
        use agent_core::agent::{
            DecisionRequest, DecisionStep, ScopeKind, ScopeState, ScopeSummary, ToolId,
            ToolObservation, ToolOutput, ToolRegistry, parse_decision,
        };

        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let config = ModelConfig {
            context_length: 4096,
            ..ModelConfig::default()
        };
        provider.load(&path, config).unwrap();
        let model = provider.loaded.as_ref().unwrap();
        println!(
            "embedded template offers tools block: {}, tool-call tags: {}",
            model.template.contains("if tools"),
            model.template.contains("<tool_call>")
        );
        let registry = ToolRegistry::mvp().unwrap();
        let scope = ScopeSummary {
            display_name: None,
            id: "synthetic_scope_1".into(),
            kind: ScopeKind::Directory,
            state: ScopeState::Active,
            remaining_ms: 3_600_000,
        };
        for (case, prompt) in [
            (
                "calculate",
                "Use the local calculator to work out 137 times 89. Report the result.",
            ),
            (
                "device",
                "Inspect this device using the available tool. What operating system is it running?",
            ),
            (
                "search",
                "Find text files whose names contain ledger in the authorized directory.",
            ),
            (
                "read",
                "Read ledger.txt from the authorized directory and report its fixture code.",
            ),
            (
                "discovery",
                "List the locally installed desktop applications using the discovery tool.",
            ),
            (
                "after_calculate",
                "Use the local calculator to work out 137 times 89. Report the result.",
            ),
        ] {
            let mut request =
                DecisionRequest::new(prompt, &registry, std::slice::from_ref(&scope), 512).unwrap();
            if case == "after_calculate" {
                let call = registry
                    .validate(ToolId::Calculate, r#"{"expression":"137*89"}"#)
                    .unwrap();
                let observation =
                    ToolObservation::success(&call, ToolOutput::Calculate { value: 12193.0 })
                        .unwrap();
                request
                    .push(DecisionStep::Observation {
                        call,
                        observation: Box::new(observation),
                    })
                    .unwrap();
            }
            let generation = request.generation_request().unwrap();
            let rendered = provider
                .loaded
                .as_ref()
                .unwrap()
                .render_messages(
                    &[
                        PromptMessage {
                            role: MessageRole::System,
                            content: &generation.history.system,
                        },
                        PromptMessage {
                            role: MessageRole::User,
                            content: &generation.prompt,
                        },
                    ],
                    false,
                )
                .unwrap();
            assert!(rendered.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
            if case == "after_calculate" {
                println!("synthetic rendered prompt: {rendered:?}");
            }
            let reply = provider.decide(&request, &Cancellation::default()).unwrap();
            assert!(
                reply.stats.prompt_tokens + generation.max_tokens as usize
                    <= config.context_length as usize
            );
            println!(
                "{case}: context={} prompt={} reserved={} generated={} end={:?} response={:?} validation={:?}",
                config.context_length,
                reply.stats.prompt_tokens,
                generation.max_tokens,
                reply.stats.generated_tokens,
                reply.stats.reason,
                reply.text.chars().take(400).collect::<String>(),
                parse_decision(&registry, &reply)
            );
        }
    }

    #[test]
    #[ignore = "profiles only fixed synthetic prompts using AGENT_TEST_MODEL"]
    fn local_agent_model_profile() {
        use agent_core::agent::{DecisionRequest, ToolRegistry, parse_decision};

        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let config = ModelConfig {
            context_length: 4096,
            threads: 4,
        };
        let started = Instant::now();
        let info = provider.load(&path, config).unwrap();
        println!(
            "loaded architecture={} parameters={} trained_context={} engine_ms={} wall_ms={}",
            info.architecture,
            info.parameters,
            info.trained_context,
            info.load_ms,
            millis(started.elapsed())
        );
        if let Ok(expected) = std::env::var("AGENT_EXPECTED_ARCH") {
            assert_eq!(info.architecture, expected);
        }
        assert!(info.supports_thinking);
        let model = provider.loaded.as_ref().unwrap();
        let enabled_suffix = if info.architecture == "qwen35" {
            "<|im_start|>assistant\n<think>\n"
        } else {
            "<|im_start|>assistant\n"
        };
        assert!(
            model
                .render_prompt("Synthetic fixture", true)
                .unwrap()
                .ends_with(enabled_suffix)
        );
        assert!(
            model
                .render_prompt("Synthetic fixture", false)
                .unwrap()
                .ends_with("<think>\n\n</think>\n\n")
        );
        let vocab = model.model.vocab();
        for marker in [
            "<|im_start|>",
            "<|im_end|>",
            "<|endoftext|>",
            "<think>",
            "</think>",
        ] {
            let tokens = vocab.tokenize(marker.as_bytes(), false, true);
            println!(
                "marker {marker}: {tokens:?}, eog={}",
                tokens.iter().any(|token| vocab.is_eog(*token))
            );
        }
        let registry = ToolRegistry::mvp().unwrap();
        for (label, prompt) in [
            (
                "compound",
                "What is 19 times the sum of 23 and 7? Use the calculator.",
            ),
            (
                "simple",
                "Use the local calculator to work out 137 times 89. Report the result.",
            ),
        ] {
            let request = DecisionRequest::new(prompt, &registry, &[], 512).unwrap();
            let reply = provider.decide(&request, &Cancellation::default()).unwrap();
            println!(
                "{label}: stats={:?} raw={:?} validated={:?}",
                reply.stats,
                reply.text,
                parse_decision(&registry, &reply)
            );
            assert!(reply.stats.prompt_tokens + 512 <= config.context_length as usize);
        }

        let request =
            GenerationRequest::new("Write a 400-word description of a fictional station.");
        let cancellation = Cancellation::default();
        let mut requested_at = None;
        let stats = provider
            .generate(&request, &cancellation, &mut |_| {
                if requested_at.is_none() {
                    requested_at = Some(Instant::now());
                    cancellation.cancel();
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(stats.reason, FinishReason::Cancelled);
        println!(
            "first-chunk cancellation: {:.2} ms; stats={stats:?}",
            requested_at.unwrap().elapsed().as_secs_f64() * 1000.0
        );
    }

    #[test]
    #[ignore = "requires the local Qwen3.5 GGUF via AGENT_TEST_MODEL"]
    fn foundation_agent_fits_large_native_observation() {
        use agent_core::agent::{
            AgentDecision, DecisionRequest, DecisionStep, ToolId, ToolObservation, ToolOutput,
            ToolRegistry, parse_decision,
        };

        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let mut provider = LlamaProvider::default();
        let info = provider.load(&path, ModelConfig::default()).unwrap();
        let registry = ToolRegistry::mvp().unwrap();
        let call = registry
            .validate(ToolId::FileSearch, r#"{"scope_id":"fixture_scope"}"#)
            .unwrap();
        let paths: Vec<_> = (1..=80_u64)
            .map(|index| {
                let a = index.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                format!(
                    "group-{}/ledger-{a:016x}{:016x}{:016x}-résumé.txt",
                    index % 8,
                    a.rotate_left(18),
                    !a
                )
            })
            .collect();
        let observation = ToolObservation::success(
            &call,
            ToolOutput::FileSearch {
                paths,
                truncated: true,
                extension: None,
            },
        )
        .unwrap();
        let mut request = DecisionRequest::new(
            "Summarize the result of the ledger file search in a short sentence.",
            &registry,
            &[],
            512,
        )
        .unwrap();
        request
            .push(DecisionStep::Observation {
                call,
                observation: Box::new(observation),
            })
            .unwrap();
        let full = request.generation_request().unwrap();
        let model = provider.loaded.as_ref().unwrap();
        let rendered = model
            .render_messages(
                &[
                    PromptMessage {
                        role: MessageRole::System,
                        content: &full.history.system,
                    },
                    PromptMessage {
                        role: MessageRole::User,
                        content: &full.prompt,
                    },
                ],
                false,
            )
            .unwrap();
        let full_tokens = model
            .model
            .vocab()
            .tokenize(rendered.as_bytes(), false, true)
            .len();
        assert!(full_tokens + 512 > info.config.context_length as usize);
        let grammar = LlamaSampler::grammar(
            &model.model,
            &AgentFormat::QwenChatMl.request_grammar(&request),
            "root",
        )
        .unwrap();
        let before = model
            .generate(
                crate::backend::cpu_backend().unwrap(),
                &full,
                &Cancellation::default(),
                &mut |_| panic!("no decoding before context fit"),
                Some(grammar),
            )
            .unwrap_err();
        assert_eq!(before.code, ErrorCode::ContextLimit);
        let result = provider.decide(&request, &Cancellation::default()).unwrap();
        let AgentDecision::Final(answer) = parse_decision(&registry, &result).unwrap() else {
            panic!("expected synthesis")
        };
        println!(
            "{} full prompt={full_tokens}, preview prompt={}, output={}, answer={answer:?}",
            info.architecture, result.stats.prompt_tokens, result.stats.generated_tokens
        );
        assert!(answer.contains("80") && answer.to_lowercase().contains("ledger"));
    }

    #[test]
    #[ignore = "measures constrained decode with the local GGUF in AGENT_TEST_MODEL"]
    fn local_agent_decode_cancellation() {
        use agent_core::agent::{DecisionRequest, ToolRegistry};
        use std::{sync::mpsc, thread, time::Duration};

        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        provider
            .load(
                &path,
                ModelConfig {
                    context_length: 4096,
                    threads: 4,
                },
            )
            .unwrap();
        let model = provider.loaded.as_ref().unwrap();
        let registry = ToolRegistry::mvp().unwrap();
        let request = DecisionRequest::new(
            "Describe a fictional station in detail. Write at least 400 words. Do not call tools.",
            &registry,
            &[],
            512,
        )
        .unwrap();
        let grammar = LlamaSampler::grammar(
            &model.model,
            &AgentFormat::QwenChatMl.request_grammar(&request),
            "root",
        )
        .unwrap();
        let request = request.generation_request().unwrap();
        let cancellation = Cancellation::default();
        let (sender, receiver) = mpsc::channel();
        let (stats, latency) = thread::scope(|scope| {
            let cancel_flag = &cancellation;
            let cancel = scope.spawn(move || {
                receiver.recv_timeout(Duration::from_secs(240)).unwrap();
                // Let the worker continue past the emitted piece into decode.
                thread::sleep(Duration::from_millis(10));
                let started = Instant::now();
                cancel_flag.cancel();
                started
            });
            let mut first_piece = Some(sender);
            let stats = model
                .generate(
                    crate::backend::cpu_backend().unwrap(),
                    &request,
                    &cancellation,
                    &mut |_| {
                        if let Some(sender) = first_piece.take() {
                            sender.send(()).unwrap();
                        }
                        Ok(())
                    },
                    Some(grammar),
                )
                .unwrap();
            let finished = Instant::now();
            drop(first_piece);
            (stats, finished.duration_since(cancel.join().unwrap()))
        });
        assert_eq!(stats.reason, FinishReason::Cancelled);
        assert!(stats.generated_tokens > 0 && stats.generated_tokens < request.max_tokens);
        println!(
            "agent decode cancellation: {:.2} ms; stats={stats:?}",
            latency.as_secs_f64() * 1000.0
        );
    }

    #[test]
    #[ignore = "runs a separate synthetic native-format probe using a local Qwen3.5 file"]
    fn local_qwen35_native_tool_format() {
        let mut provider = LlamaProvider::default();
        let path = std::path::PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let config = ModelConfig {
            context_length: 4096,
            threads: 4,
        };
        let info = provider.load(&path, config).unwrap();
        assert_eq!(info.architecture, "qwen35");

        // Supply a synthetic tool list to this loaded template only. The
        // diagnostic never parses a launch or dispatches a native tool.
        let model = provider.loaded.as_mut().unwrap();
        let fixture = r#"{%- set tools = [{"type":"function","function":{"name":"math.calculate","description":"Evaluate a finite arithmetic expression.","parameters":{"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"],"additionalProperties":false}}}] -%}"#;
        model.template = format!("{fixture}{}", model.template);
        let mut request = GenerationRequest::new("Use the calculator to compute 137 times 89.");
        request.temperature = 0.0;
        let rendered = model.render_prompt(&request.prompt, false).unwrap();
        assert!(
            rendered.contains("<tools>") && rendered.contains("<parameter=example_parameter_1>")
        );
        let mut output = String::new();
        let stats = provider
            .generate(&request, &Cancellation::default(), &mut |delta| {
                output.push_str(&delta.text);
                Ok(())
            })
            .unwrap();
        println!(
            "native-format synthetic prompt={rendered:?}\nresponse={output:?}\nstats={stats:?}"
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
