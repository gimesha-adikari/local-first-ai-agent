//! Chat through a configured remote or self-hosted provider. The turn is
//! prepared exactly like local Chat; this module withholds what the
//! provider may not receive, fits the rest, reserves budget, obtains
//! one-operation consent and streams the answer through the boundary.

use std::time::{Duration, Instant};

use agent_core::{
    capability::ImageInput,
    capability::{GENERATE, IMAGE, Usage},
    chat::{ChatHistory, GenerationState, Message, MessageRole, PromptMessage, fit_context},
    chat_context::{ChatContext, ContextUsage},
    classification::{
        CLIPBOARD_TEXT, Classified, DataClasses, Labels, SCREEN_IMAGE, SCREEN_TEXT, USER_INPUT,
    },
    inference::{
        Cancellation, ErrorCode, FinishReason, GenerationStats, InferenceError, OutputBudget,
        Result,
    },
    intelligence::{EgressRecord, SessionState, Transmission, UsageBasis},
    provider::ProviderSettings,
};
use storage::{
    chat_context::KnowledgeCoverage,
    knowledge::KnowledgeHit,
    providers::{LedgerBasis, LedgerTransmission, Limits, Reservation, SavedProvider, Settlement},
};

use super::{ChatEvent, ChatEvents, ChatRuntime, Transcript, deliver, transcript_lock};
use crate::{
    chat_session::{Candidates, ChatSession, policy_error},
    database,
    network::{Destination, Failed, Method, Outbound, Streaming},
    policy::Session,
    providers::{consent, failure_error, offer, openai},
    usage,
};

/// What a prepared turn hands to the remote path.
pub(super) struct RemoteTurn {
    pub turn: agent_core::chat::PendingTurn,
    pub context: Option<ChatContext>,
    pub candidates: Candidates,
    pub coverage: Option<KnowledgeCoverage>,
    pub sources: Vec<KnowledgeHit>,
    pub max_tokens: Option<u32>,
    pub cancellation: Cancellation,
    /// Images for providers that accept image input, already authorized for
    /// this request. Each is also a required candidate source.
    pub images: Vec<(ImageInput, (u32, u32))>,
    /// Transport records of research requests made for this turn.
    pub external: Vec<agent_core::intelligence::EgressRecord>,
    /// The user authorized sending this message's attachment content to
    /// this provider for this request (Stage 04-D).
    pub attachments: bool,
}

/// Classes of attached content, admitted only for a request whose sharing
/// the user authorized for those exact attachments.
/// A camera snapshot attached to Chat keeps its sensor classes (Stage 04-E).
pub(crate) const ATTACHMENT_CLASSES: [agent_core::classification::DataClass; 5] = [
    agent_core::classification::ATTACHMENT,
    agent_core::classification::ATTACHMENT_TEXT,
    agent_core::classification::ATTACHMENT_IMAGE,
    agent_core::classification::CAMERA_IMAGE,
    agent_core::classification::CAMERA_TEXT,
];

fn denied(message: impl Into<String>) -> InferenceError {
    InferenceError::new(ErrorCode::PolicyDenied, message)
}

fn permitted(allowed: &DataClasses, labels: &Labels) -> bool {
    labels.ids().all(|id| allowed.sensitivity(id).is_some())
}

/// The fitted request: owned messages and what was used.
struct Fitted {
    messages: Vec<(MessageRole, String)>,
    usage: ContextUsage,
    excluded_history: usize,
    input_estimate: u64,
}

/// Fit system instructions, context and history into the provider's
/// context window with a conservative estimate, shedding memories and then
/// evidence like the local adapter, and dropping the oldest history first.
fn fit(
    history: &ChatHistory,
    prompt: &str,
    context: Option<&ChatContext>,
    window: u32,
    output: u32,
) -> Result<Fitted> {
    let augmented = context.map(|context| {
        let mut history = history.clone();
        context.append_system_instructions(&mut history.system);
        history
    });
    let history = augmented.as_ref().unwrap_or(history);
    let mut usage = context.map(ChatContext::usage);
    loop {
        let latest = match (context, usage) {
            (Some(context), Some(usage)) => context.render(prompt, usage),
            _ => prompt.to_owned(),
        };
        let measured = fit_context(history, &latest, window, output, |messages| {
            let estimate = usage::estimate_messages(messages);
            let owned = messages
                .iter()
                .map(|message| (message.role, message.content.to_owned()))
                .collect::<Vec<_>>();
            Ok((
                (owned, estimate),
                usize::try_from(estimate).unwrap_or(usize::MAX),
            ))
        });
        match measured {
            Ok(((messages, input_estimate), excluded_history)) => {
                return Ok(Fitted {
                    messages,
                    usage: usage.unwrap_or_default(),
                    excluded_history,
                    input_estimate,
                });
            }
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
                return Err(InferenceError::new(
                    ErrorCode::ContextLimit,
                    "The message does not fit this provider's context window with the requested output.",
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

struct RemoteBudget {
    id: i64,
    tokens: u64,
    cost: Option<u64>,
}

struct Outcome {
    reason: Option<FinishReason>,
    done: bool,
    usage: Option<openai::ReportedUsage>,
    first_token: Option<Duration>,
}

impl ChatRuntime {
    pub(super) async fn stream_remote(
        &self,
        remote: RemoteTurn,
        target: (SavedProvider, Destination),
        events: ChatEvents,
    ) -> Result<Message> {
        let RemoteTurn {
            mut turn,
            mut context,
            candidates,
            coverage,
            sources,
            max_tokens,
            cancellation,
            images: attached,
            external,
            attachments,
        } = remote;
        let (saved, destination) = target;
        let settings = saved.settings.clone();
        let offer = offer(&saved);
        let started = Instant::now();
        let message_id = turn.assistant.id;
        let ephemeral = self.glance_receipt.is_some();
        let mut allowed = settings.sharing.allowed_classes();
        if ephemeral {
            allowed.extend([
                CLIPBOARD_TEXT,
                SCREEN_TEXT,
                SCREEN_IMAGE,
                agent_core::classification::CAMERA_TEXT,
                agent_core::classification::CAMERA_IMAGE,
            ]);
        }
        if attachments {
            allowed.extend(ATTACHMENT_CLASSES);
        }
        let allowed = DataClasses::new(allowed);

        // Withhold what this provider may not receive before anything is
        // built. The current message must be shareable; documents the turn
        // depends on must be too, or the turn stops with an explanation.
        let mut refusal = None;
        if candidates
            .required
            .iter()
            .any(|source| !permitted(&allowed, &source.labels))
        {
            refusal = Some(denied(format!(
                "This message includes information you have not allowed {} to receive.",
                settings.name
            )));
        }
        if refusal.is_none()
            && context
                .as_ref()
                .is_some_and(|context| context.evidence.is_some())
            && candidates
                .knowledge
                .iter()
                .any(|source| !permitted(&allowed, &source.labels))
        {
            refusal = Some(denied(format!(
                "Document sharing with {} is off. Turn it on in the provider settings, or switch to the local model.",
                settings.name
            )));
        }
        if let Some(context) = &mut context
            && candidates
                .memories
                .iter()
                .any(|source| !permitted(&allowed, &source.labels))
        {
            // Memories are optional background: withhold all of them.
            context.memories.clear();
        }
        let withheld_history = candidates
            .history
            .iter()
            .rposition(|source| !permitted(&allowed, &source.labels))
            .map_or(0, |index| index + 1);
        turn.history.turns.drain(..withheld_history);

        let mut session = ChatSession::new(
            candidates,
            cancellation.clone(),
            agent_core::classification::MODEL_OUTPUT,
        )
        .ephemeral(ephemeral);
        for record in external {
            session.record_external(record);
        }
        let output = max_tokens
            .unwrap_or(settings.max_output_tokens)
            .clamp(1, settings.max_output_tokens);
        let prepared = refusal.map_or_else(
            || {
                session.authorize_remote(&self.policy, &offer)?;
                let fitted = fit(
                    &turn.history,
                    &turn.prompt,
                    context.as_ref(),
                    settings.context_tokens,
                    output,
                )?;
                let labels = session
                    .fitted(fitted.usage, withheld_history + fitted.excluded_history)?
                    .clone();
                Ok((fitted, labels))
            },
            Err,
        );
        let (fitted, output_labels) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return self.fail_unsent(session, message_id, &turn.assistant.content, error);
            }
        };
        let messages: Vec<PromptMessage<'_>> = fitted
            .messages
            .iter()
            .map(|(role, content)| PromptMessage {
                role: *role,
                content,
            })
            .collect();
        let images: Vec<&ImageInput> = attached.iter().map(|(image, _)| image).collect();
        if !images.is_empty() && !offer.accepts(IMAGE) {
            return self.fail_unsent(
                session,
                message_id,
                "",
                InferenceError::new(
                    ErrorCode::UnsupportedCapability,
                    "This provider is not configured for image input.",
                ),
            );
        }
        let body = openai::chat_body(&openai::ChatRequest {
            model: &settings.model,
            messages: &messages,
            images: &images,
            max_tokens: output,
            temperature: settings.temperature,
            stream_usage: settings.stream_usage,
        });
        let input_labels = session.input_labels().unwrap_or_default();
        let outbound = Outbound::new(
            session.policy_session(),
            GENERATE,
            if ephemeral {
                "glance"
            } else if attachments {
                "chat-attachments"
            } else {
                "chat"
            },
            destination.locality(),
            destination,
            Method::Post,
            openai::CHAT_COMPLETIONS,
            Classified::derived(body, [&input_labels], USER_INPUT),
            settings.retention.retention(),
        );

        // Reserve the most this request could use before admitting it.
        let input_estimate = fitted.input_estimate.saturating_add(
            attached
                .iter()
                .map(|(_, (width, height))| usage::estimate_image(*width, *height))
                .sum::<u64>(),
        );
        let budget = match self.reserve_remote(&saved, input_estimate, output) {
            Ok(budget) => budget,
            Err(error) => return self.fail_unsent(session, message_id, "", error),
        };
        let until = Instant::now() + Duration::from_secs(600);
        let (process, share) = match consent(&self.policy, &settings, outbound.operation(), until) {
            Ok(grants) => grants,
            Err(denial) => {
                self.release(budget.id, message_id);
                return self.fail_unsent(session, message_id, "", policy_error(denial));
            }
        };

        // The fitted classes are committed before any byte is sent.
        if !ephemeral
            && let Err(error) = database(&self.database).and_then(|mut db| {
                db.checkpoint_intelligence_context(
                    message_id,
                    &output_labels,
                    turn.assistant.context.as_deref(),
                    fitted.usage,
                    &sources,
                )
            })
        {
            self.release(budget.id, message_id);
            return self.fail_unsent(session, message_id, "", error);
        }
        session.submitted(&offer);
        if let Some(coverage) = coverage {
            deliver(&events, ChatEvent::Knowledge { coverage });
        }
        let mut assistant = turn.assistant;
        deliver(
            &events,
            ChatEvent::Started {
                user: turn.user,
                assistant: Box::new(assistant.clone()),
            },
        );

        let transcript = std::sync::Mutex::new(Transcript {
            text: assistant.content.clone(),
            checkpoint_at: Instant::now(),
            checkpoint_bytes: assistant.content.len(),
        });
        let exchange = self.boundary.send(outbound, process, share).await;
        let (result, record) = match exchange {
            Err(failed) => {
                let Failed { failure, record } = *failed;
                (Err(failure_error(&failure)), record)
            }
            Ok(streaming) => {
                let mut streaming = streaming;
                let result = read_stream(&mut streaming, started, |text| {
                    transcript_lock(&transcript)?.push(
                        &text,
                        &self.database,
                        self.glance_receipt.is_none().then_some(message_id),
                    )?;
                    deliver(
                        &events,
                        ChatEvent::Delta {
                            id: message_id,
                            text,
                        },
                    );
                    Ok(())
                })
                .await;
                let (record, live) = streaming.finish_checked().await;
                (
                    result
                        .and_then(|outcome| live.map(|()| outcome).map_err(|f| failure_error(&f))),
                    record,
                )
            }
        };
        let _ = self.policy.revoke(process);
        let _ = self.policy.revoke(share);
        let content = std::mem::take(&mut transcript_lock(&transcript)?.text);

        // Settle usage from the transport record and the provider's report.
        let reported = result.as_ref().ok().and_then(|outcome| outcome.usage);
        let (basis, settled) = self.settle_remote(
            &settings,
            budget,
            record.transmission,
            reported,
            self.glance_receipt.is_none().then_some(message_id),
        );
        let (usage_basis, measured_input, measured_output) = match (basis, reported) {
            (LedgerBasis::ProviderReported, Some(usage)) => {
                (UsageBasis::ProviderReported, usage.input, usage.output)
            }
            (LedgerBasis::None, _) => (UsageBasis::None, 0, 0),
            _ => (
                UsageBasis::Estimated,
                input_estimate,
                usage::estimate_text(&content),
            ),
        };
        session.record_usage(
            vec![
                Usage {
                    unit: "input-token",
                    amount: measured_input,
                },
                Usage {
                    unit: "output-token",
                    amount: measured_output,
                },
            ],
            usage_basis,
        );
        session.record_external(record);

        let elapsed = started.elapsed();
        let outcome = result.and_then(|outcome| {
            settled.clone()?;
            match (outcome.reason, outcome.done) {
                (Some(reason), _) => Ok((reason, outcome)),
                (None, true) => Ok((FinishReason::EndOfGeneration, outcome)),
                (None, false) => Err(InferenceError::new(
                    ErrorCode::ProviderInvalidResponse,
                    "The provider ended the response early.",
                )),
            }
        });
        let (reason, outcome) = match outcome {
            Ok(value) => value,
            Err(error) => {
                let (state, session_state) = if error.code == ErrorCode::Cancelled {
                    (GenerationState::Cancelled, SessionState::Cancelled)
                } else {
                    (GenerationState::Failed, SessionState::Failed)
                };
                let receipt = session.receipt(session_state, None);
                self.finish_turn(message_id, &content, state, None, &receipt)?;
                return Err(error);
            }
        };
        let generated = u32::try_from(measured_output).unwrap_or(u32::MAX);
        let stats = GenerationStats {
            reason,
            prompt_tokens: usize::try_from(measured_input).unwrap_or(usize::MAX),
            prompt_tokens_per_second: None,
            generated_tokens: generated,
            time_to_first_token_ms: outcome
                .first_token
                .map(|time| time.as_millis().try_into().unwrap_or(u64::MAX)),
            elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            tokens_per_second: f64::from(generated) / elapsed.as_secs_f64().max(f64::EPSILON),
            excluded_turns: withheld_history + fitted.excluded_history,
            output_budget: Some(OutputBudget {
                requested: max_tokens.unwrap_or(settings.max_output_tokens),
                effective: output,
                context_length: settings.context_tokens,
            }),
            assistant_replay: None,
            context_usage: Some(fitted.usage),
        };
        assistant.content = content;
        assistant.state = GenerationState::from_output(reason, &assistant.content);
        let terminal = if reason == FinishReason::Cancelled {
            SessionState::Cancelled
        } else {
            SessionState::Completed
        };
        let receipt = session.receipt(terminal, Some(&stats));
        self.finish_turn(
            message_id,
            &assistant.content,
            assistant.state,
            Some(&stats),
            &receipt,
        )?;
        assistant.labels = receipt.output_labels;
        if assistant.context.is_some() {
            assistant.context = database(&self.database)?
                .chat_attempt_context(message_id)?
                .map(Box::new);
        }
        assistant.stats = Some(stats);
        Ok(assistant)
    }

    /// A private, bounded model step. It never publishes raw model output to Chat.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn complete_research_step(
        &self,
        target: &(SavedProvider, Destination),
        session: &Session,
        system: &str,
        prompt: &str,
        labels: &Labels,
        output: u32,
        message_id: i64,
        external: &mut Vec<EgressRecord>,
    ) -> Result<String> {
        let (saved, destination) = target;
        let settings = &saved.settings;
        let messages = [
            PromptMessage {
                role: MessageRole::System,
                content: system,
            },
            PromptMessage {
                role: MessageRole::User,
                content: prompt,
            },
        ];
        let output = output.min(settings.max_output_tokens);
        let input = usage::estimate_messages(&messages);
        if input.saturating_add(output.into()) > u64::from(settings.context_tokens) {
            return Err(InferenceError::new(
                ErrorCode::ContextLimit,
                "Research exceeds the selected provider's context window.",
            ));
        }
        let body = openai::chat_body(&openai::ChatRequest {
            model: &settings.model,
            messages: &messages,
            images: &[],
            max_tokens: output,
            temperature: 0.0,
            stream_usage: settings.stream_usage,
        });
        let outbound = Outbound::new(
            session,
            GENERATE,
            crate::research::MODEL_PURPOSE,
            destination.locality(),
            destination.clone(),
            Method::Post,
            openai::CHAT_COMPLETIONS,
            Classified::derived(body, [labels], USER_INPUT),
            settings.retention.retention(),
        );
        let budget = self.reserve_remote(saved, input, output)?;
        let (process, share) = match consent(
            &self.policy,
            settings,
            outbound.operation(),
            Instant::now() + Duration::from_secs(90),
        ) {
            Ok(grants) => grants,
            Err(denial) => {
                self.release(budget.id, message_id);
                return Err(policy_error(denial));
            }
        };

        let mut text = String::new();
        let (result, record) = match self.boundary.send(outbound, process, share).await {
            Ok(mut streaming) => {
                let result = read_stream(&mut streaming, Instant::now(), |delta| {
                    crate::research::push_step_output(&mut text, &delta)
                })
                .await;
                let (record, live) = streaming.finish_checked().await;
                (
                    result
                        .and_then(|outcome| live.map(|()| outcome).map_err(|f| failure_error(&f))),
                    record,
                )
            }
            Err(failed) => {
                let Failed { failure, record } = *failed;
                (Err(failure_error(&failure)), record)
            }
        };
        let _ = self.policy.revoke(process);
        let _ = self.policy.revoke(share);
        let reported = result.as_ref().ok().and_then(|outcome| outcome.usage);
        let (_, settled) = self.settle_remote(
            settings,
            budget,
            record.transmission,
            reported,
            Some(message_id),
        );
        external.push(record);
        let outcome = result?;
        settled?;
        if outcome.reason == Some(FinishReason::EndOfGeneration)
            || (outcome.done && outcome.reason.is_none())
        {
            return Ok(text);
        }
        Err(InferenceError::new(
            ErrorCode::ProviderInvalidResponse,
            "The provider did not finish the research step.",
        ))
    }

    fn reserve_remote(
        &self,
        saved: &SavedProvider,
        input: u64,
        output: u32,
    ) -> Result<RemoteBudget> {
        let tokens = input.saturating_add(output.into());
        let cost = usage::estimate_cost(saved.settings.pricing.as_ref(), input, output.into());
        let id = database(&self.database)?.reserve_usage(
            &usage::current_period(),
            Reservation {
                provider: saved.id,
                tokens,
                cost,
            },
            limits(&saved.settings),
        )?;
        Ok(RemoteBudget { id, tokens, cost })
    }

    fn settle_remote(
        &self,
        settings: &ProviderSettings,
        budget: RemoteBudget,
        transmission: Transmission,
        reported: Option<openai::ReportedUsage>,
        message: Option<i64>,
    ) -> (LedgerBasis, Result<()>) {
        let (basis, charged_tokens, charged_cost, input_tokens, output_tokens) =
            match (transmission, reported) {
                (_, Some(usage)) => (
                    LedgerBasis::ProviderReported,
                    usage.input.saturating_add(usage.output),
                    usage::estimate_cost(settings.pricing.as_ref(), usage.input, usage.output),
                    Some(usage.input),
                    Some(usage.output),
                ),
                (Transmission::Unsent, None) => (
                    LedgerBasis::None,
                    0,
                    budget.cost.is_some().then_some(0),
                    None,
                    None,
                ),
                (_, None) => (
                    LedgerBasis::Estimated,
                    budget.tokens,
                    budget.cost,
                    None,
                    None,
                ),
            };
        let result = database(&self.database).and_then(|mut db| {
            db.settle_usage(
                budget.id,
                Settlement {
                    transmission: ledger(transmission),
                    input_tokens,
                    output_tokens,
                    basis,
                    charged_tokens,
                    charged_cost,
                    message,
                },
            )
        });
        (basis, result)
    }

    fn release(&self, reservation: i64, message_id: agent_core::chat::MessageId) {
        if let Ok(mut db) = database(&self.database) {
            let _ = db.settle_usage(
                reservation,
                Settlement {
                    transmission: LedgerTransmission::Unsent,
                    input_tokens: None,
                    output_tokens: None,
                    basis: LedgerBasis::None,
                    charged_tokens: 0,
                    charged_cost: Some(0),
                    message: self.glance_receipt.is_none().then_some(message_id),
                },
            );
        }
    }

    /// Record a turn that stopped before anything was sent.
    fn fail_unsent(
        &self,
        session: ChatSession,
        message_id: agent_core::chat::MessageId,
        content: &str,
        error: InferenceError,
    ) -> Result<Message> {
        let (state, session_state) = if error.code == ErrorCode::Cancelled {
            (GenerationState::Cancelled, SessionState::Cancelled)
        } else {
            (GenerationState::Failed, SessionState::Failed)
        };
        let receipt = session.receipt(session_state, None);
        self.finish_turn(message_id, content, state, None, &receipt)?;
        Err(error)
    }
}

fn ledger(transmission: Transmission) -> LedgerTransmission {
    match transmission {
        Transmission::Unsent => LedgerTransmission::Unsent,
        Transmission::Sent => LedgerTransmission::Sent,
        Transmission::Unknown => LedgerTransmission::Unknown,
    }
}

fn limits(settings: &ProviderSettings) -> Limits {
    Limits {
        tokens: settings.budget.monthly_tokens,
        cost: settings.budget.monthly_cost_micros,
        unpriced_micros_per_million: settings.pricing.as_ref().map(|pricing| {
            pricing
                .input_micros_per_million
                .max(pricing.output_micros_per_million)
        }),
    }
}

async fn read_stream(
    streaming: &mut Streaming,
    started: Instant,
    mut emit: impl FnMut(String) -> Result<()>,
) -> Result<Outcome> {
    let status = streaming.status;
    let event_stream = streaming
        .content_type
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
    let mut outcome = Outcome {
        reason: None,
        done: false,
        usage: None,
        first_token: None,
    };
    let mut parser = openai::Stream::default();
    let mut whole = Vec::new();
    let successful = (200..300).contains(&status);
    loop {
        let piece = match streaming.next().await {
            None => break,
            Some(piece) => piece.map_err(|failure| failure_error(&failure))?,
        };
        if !successful || !event_stream {
            if whole.len() + piece.len() > openai::MAX_JSON_BYTES {
                return Err(InferenceError::new(
                    ErrorCode::ProviderInvalidResponse,
                    "The provider's response exceeded this app's size limits.",
                ));
            }
            whole.extend(piece);
            continue;
        }
        let parsed = parser.feed(&piece)?;
        apply_events(streaming, parsed, &mut outcome, started, &mut emit)?;
        if parser.is_done() {
            return Ok(outcome);
        }
    }
    if !successful {
        return Err(openai::error_response(status, &whole));
    }
    let parsed = if event_stream {
        parser.finish()?
    } else {
        openai::complete_events(&whole)?
    };
    apply_events(streaming, parsed, &mut outcome, started, &mut emit)?;
    Ok(outcome)
}

fn apply_events(
    streaming: &Streaming,
    parsed: Vec<openai::Event>,
    outcome: &mut Outcome,
    started: Instant,
    emit: &mut impl FnMut(String) -> Result<()>,
) -> Result<()> {
    for event in parsed {
        streaming.check_live().map_err(|f| failure_error(&f))?;
        match event {
            openai::Event::Delta(text) => {
                outcome.first_token.get_or_insert_with(|| started.elapsed());
                emit(text)?;
            }
            openai::Event::Finish(reason) => outcome.reason = Some(reason),
            openai::Event::Usage(usage) => outcome.usage = Some(usage),
            openai::Event::Done => outcome.done = true,
        }
    }
    Ok(())
}
