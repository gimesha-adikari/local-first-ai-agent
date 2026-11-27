//! A Chat turn with attached files (Stage 04-D). Attached content is
//! prepared locally, selected deterministically, labelled, fitted and cited
//! like other evidence. It reaches a remote or self-hosted provider only
//! under a per-request authorization bound to the exact attachments, and
//! an image only when the user chose to send that image.

use std::{collections::BTreeSet, sync::Arc};

use agent_core::{
    attachment::{
        AttachmentRecord, AttachmentTask, AttachmentUse, MAX_SCOPE_ATTACHMENTS, PassageRecord,
        PassageSource, valid_attachment_id,
    },
    capability::{IMAGE, ImageInput, MAX_IMAGE_BYTES},
    chat::{ChatStore, ConversationId, Message, ModelId, TurnAction},
    chat_context::{AttachmentPrompt, AttemptContext, ChatContext, EvidenceTask, Grounding},
    chat_route::{route, routed_question},
    classification::{Labels, SYSTEM_TEXT},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    intelligence::{ContextSource, SourceKind},
};
use inference::Phase;
use serde::Deserialize;

use super::{ChatEvents, ChatOptions, ChatRuntime, StreamContext, context_candidates, remote};
use crate::{
    attachments::{PreparedContent, Resolved, select},
    database,
};

/// The attachments a message uses, and any sharing the user authorized.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRequest {
    pub ids: Vec<String>,
    /// Present only when the user reviewed the remote sharing notice.
    #[serde(default)]
    pub share: Option<AttachmentSharing>,
}

/// A one-request authorization to send attached content to one provider.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentSharing {
    /// The provider the notice named.
    pub provider: i64,
    /// Attachments whose extracted or recognized text may be sent. Must be
    /// exactly the attachments of the message.
    pub ids: Vec<String>,
    /// Images whose validated bytes may be sent.
    #[serde(default)]
    pub images: Vec<String>,
}

fn invalid(message: impl Into<String>) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

/// Bytes of evidence a model with `context_tokens` can afford, leaving room
/// for instructions, the question and the answer.
fn budget(context_tokens: u32) -> usize {
    (context_tokens as usize * 7 / 4).clamp(1536, 24 * 1024)
}

fn evidence_line(name: &str, source: PassageSource, text: &str) -> String {
    let location = match source {
        PassageSource::ImageText => {
            "text recognized from the image (may contain recognition errors)".to_owned()
        }
        other => other.describe(),
    };
    format!("{name} · {location}\n{text}")
}

/// Facts the model needs about what could not be read.
fn notes(resolved: &[Resolved], images_visible: &[bool]) -> Vec<String> {
    let mut notes = Vec::new();
    for (attachment, visible) in resolved.iter().zip(images_visible) {
        let content: &PreparedContent = &attachment.content;
        if let Some(pages) = content.pages {
            if pages.pages_examined < pages.page_count {
                notes.push(format!(
                    "{}: only the first {} of {} pages were read.",
                    attachment.name, pages.pages_examined, pages.page_count
                ));
            }
            if pages.pages_without_text > 0 {
                notes.push(format!(
                    "{}: {} of {} pages have no text layer (possibly scanned) and were not read.",
                    attachment.name, pages.pages_without_text, pages.pages_examined
                ));
            }
        }
        if attachment.kind.is_image() && content.passages.is_empty() && !visible {
            notes.push(format!(
                "{}: no text was recognized in this image, and you cannot see it.",
                attachment.name
            ));
        }
    }
    notes
}

impl ChatRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_attachments(
        &self,
        id: ConversationId,
        action: TurnAction,
        question: String,
        request: AttachmentRequest,
        options: ChatOptions,
        events: ChatEvents,
        cancellation: Cancellation,
        model_id: Option<ModelId>,
    ) -> Result<Message> {
        if options.research.is_some() {
            return Err(invalid(
                "Web search can't be combined with attachments yet. Remove the attachments or turn off Search.",
            ));
        }
        if options.knowledge.is_some() {
            return Err(invalid(
                "Library documents can't be combined with attachments yet. Turn off Knowledge or remove the attachments.",
            ));
        }
        let mut seen = BTreeSet::new();
        if request.ids.is_empty()
            || request.ids.len() > MAX_SCOPE_ATTACHMENTS
            || request
                .ids
                .iter()
                .any(|id| !valid_attachment_id(id) || !seen.insert(id.as_str()))
        {
            return Err(invalid("Select 1–6 valid attachments."));
        }
        let regenerating = matches!(action, TurnAction::Regenerate);
        // Regeneration must use every attachment the replaced answer used.
        if regenerating
            && let Some((_, record)) = database(&self.database)?.latest_attachment_turn(id, 0)?
            && let Some(missing) = record
                .attachments
                .iter()
                .find(|used| !request.ids.contains(&used.id))
        {
            return Err(invalid(format!(
                "\u{201c}{}\u{201d} is no longer attached, so this answer can't be regenerated with the same files. Reattach it, or ask a new question.",
                missing.name
            )));
        }
        let (resolved, bind) = self.attachments.resolve(id, &request.ids)?;

        // The saved selection decides the target. There is no fallback in
        // either direction.
        let selection = database(&self.database)?.intelligence_settings()?;
        let remote = crate::providers::remote_target(&self.database, selection)?;
        let (context_tokens, model_name, recipe) = match &remote {
            Some((saved, _)) => {
                if options.enable_thinking {
                    return Err(InferenceError::new(
                        ErrorCode::UnsupportedCapability,
                        "Thinking mode is available only with a local model. Send without thinking.",
                    ));
                }
                if options
                    .max_tokens
                    .is_some_and(|value| !(1..=32_768).contains(&value))
                {
                    return Err(invalid("Use 1–32768 output tokens."));
                }
                (
                    saved.settings.context_tokens,
                    format!("{} · {}", saved.settings.name, saved.settings.model),
                    None,
                )
            }
            None => {
                let model_id = model_id.ok_or_else(|| {
                    InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first.")
                })?;
                let status = self.adapter.status()?;
                let Some(model) = status
                    .model
                    .as_ref()
                    .filter(|_| status.phase == Phase::Ready)
                else {
                    return Err(InferenceError::new(
                        ErrorCode::Busy,
                        "Wait for the model to be ready.",
                    ));
                };
                let mut settings = database(&self.database)?
                    .model(model_id)?
                    .reference
                    .settings;
                settings.enable_thinking = options.enable_thinking;
                if let Some(max_tokens) = options.max_tokens {
                    settings.max_tokens = max_tokens;
                }
                settings.validate()?;
                if settings.enable_thinking && !model.supports_thinking {
                    return Err(invalid(
                        "This model does not support thinking mode. Send without thinking.",
                    ));
                }
                (
                    model.config.context_length,
                    model.file_name.clone(),
                    Some(settings),
                )
            }
        };

        // Sharing with a remote provider is authorized per request, for these
        // exact attachments, and separately for each image's bytes.
        let mut visible = vec![false; resolved.len()];
        if let Some((saved, _)) = &remote {
            let share = request.share.as_ref().ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::PolicyDenied,
                    format!(
                        "Sending attachment content to {} wasn't authorized for this message. Review the sharing notice and send again, or switch to a local model.",
                        saved.settings.name
                    ),
                )
            })?;
            let authorized: BTreeSet<&str> = share.ids.iter().map(String::as_str).collect();
            if share.provider != saved.id || authorized != seen {
                return Err(InferenceError::new(
                    ErrorCode::PolicyDenied,
                    "The attachments or provider changed after you reviewed the sharing notice. Review it and send again.",
                ));
            }
            for image in &share.images {
                let index = resolved
                    .iter()
                    .position(|item| &item.id == image && item.kind.is_image())
                    .ok_or_else(|| {
                        InferenceError::new(
                            ErrorCode::PolicyDenied,
                            "Only attached images can be sent as images.",
                        )
                    })?;
                let size = resolved[index]
                    .content
                    .image
                    .as_ref()
                    .map_or(usize::MAX, |image| image.bytes.len());
                if size > MAX_IMAGE_BYTES {
                    return Err(invalid(format!(
                        "\u{201c}{}\u{201d} is larger than the 3 MB a provider can receive. Send its recognized text only.",
                        resolved[index].name
                    )));
                }
                visible[index] = true;
            }
            if visible.contains(&true) && !crate::providers::offer(saved).accepts(IMAGE) {
                return Err(InferenceError::new(
                    ErrorCode::UnsupportedCapability,
                    format!(
                        "{} isn't configured for image input. Send recognized text only, or enable images in the provider settings.",
                        saved.settings.name
                    ),
                ));
            }
        }

        // Follow-ups re-rank with the previous attachment question.
        let previous = database(&self.database)?
            .latest_attachment_turn(id, u32::from(regenerating))?
            .map(|(question, _)| question);
        let plan = route(&question, previous.as_deref(), Grounding::Documents);
        let contents: Vec<&PreparedContent> = resolved.iter().map(|item| &*item.content).collect();
        let chosen = select(&contents, plan.intent, &plan.query, budget(context_tokens));
        let images_visible = visible.contains(&true);
        let total_passages = contents
            .iter()
            .map(|content| content.passages.len() as u32)
            .sum();
        let record = AttachmentRecord {
            version: 1,
            task: chosen.task,
            attachments: resolved
                .iter()
                .zip(&visible)
                .map(|(item, sent)| AttachmentUse {
                    id: item.id.clone(),
                    name: item.name.clone(),
                    kind: item.kind,
                    origin: item.origin,
                    retention: item.retention,
                    total_passages: item.content.passages.len() as u32,
                    pages: item.content.pages,
                    recognized_text: item.kind.is_image() && !item.content.passages.is_empty(),
                    image_sent: *sent,
                })
                .collect(),
            passages: chosen
                .picks
                .iter()
                .map(|&(attachment, ordinal)| PassageRecord {
                    attachment: resolved[attachment].id.clone(),
                    ordinal,
                    source: resolved[attachment].content.passages[ordinal as usize].source,
                    cited: false,
                })
                .collect(),
            shared_remotely: remote.is_some(),
            no_match: chosen.no_match,
        };
        let deterministic = match (chosen.task, chosen.picks.is_empty()) {
            (AttachmentTask::Search, true) => Some(format!(
                "The text \u{201c}{}\u{201d} wasn't found in the attachments.",
                plan.query
            )),
            (_, true) if !images_visible => Some(agent_core::chat_context::no_evidence_answer(
                &EvidenceTask::Attachment(AttachmentPrompt::default()),
            )),
            _ => None,
        };
        let evidence: Vec<String> = chosen
            .picks
            .iter()
            .map(|&(attachment, ordinal)| {
                let passage = &resolved[attachment].content.passages[ordinal as usize];
                evidence_line(&resolved[attachment].name, passage.source, &passage.text)
            })
            .collect();
        let passage_sources: Vec<ContextSource> = chosen
            .picks
            .iter()
            .map(|&(attachment, _)| ContextSource {
                kind: SourceKind::Attachment,
                record: None,
                labels: resolved[attachment]
                    .origin
                    .text_labels(resolved[attachment].kind),
            })
            .collect();
        let mut images = Vec::new();
        let mut image_sources = Vec::new();
        for (item, sent) in resolved.iter().zip(&visible) {
            if !sent {
                continue;
            }
            let image = item
                .content
                .image
                .as_ref()
                .ok_or_else(|| invalid("The image is unavailable."))?;
            let input = ImageInput::new(image.bytes.clone())
                .ok_or_else(|| invalid("The image can't be sent in this format."))?;
            images.push((input, (image.width, image.height)));
            image_sources.push(ContextSource {
                kind: SourceKind::AttachmentImage,
                record: None,
                labels: item.origin.image_labels(),
            });
        }
        let prompt_task = EvidenceTask::Attachment(AttachmentPrompt {
            task: chosen.task,
            total_passages,
            images_visible,
            notes: notes(&resolved, &visible),
        });

        // The user turn and its pending reply are saved together. Attachment
        // answers are contextual: never replayed as ordinary history.
        let store = Arc::clone(&self.database);
        let initial = AttemptContext {
            attachments: Some(record),
            ..Default::default()
        };
        let name = model_name.clone();
        let mut turn = self
            .blocking(move || {
                database(&store)?.begin_chat_with_context(id, action, Some(&name), initial)
            })
            .await
            .ok_or_else(|| {
                InferenceError::new(ErrorCode::WorkerStopped, "Chat preparation stopped.")
            })??;
        crate::Engine::bind_attachments(
            Arc::clone(&self.database),
            Arc::clone(&self.policy),
            Arc::clone(&self.platform),
            id,
            bind,
        )
        .await;
        if let Some(text) = deterministic {
            return self.finish_without_generation(
                turn,
                &events,
                None,
                text,
                Vec::new(),
                cancellation,
            );
        }
        if plan.refinement.is_some() {
            turn.prompt = routed_question(&plan, &question, previous.as_deref());
        }
        let mut candidates = context_candidates(&turn, false);
        candidates.required.push(ContextSource {
            kind: SourceKind::SystemPrompt,
            record: None,
            labels: Labels::from_source(SYSTEM_TEXT),
        });
        candidates.required.extend(image_sources);
        // Evidence answers see no earlier turns, memories or documents.
        turn.history.turns.clear();
        candidates.withheld.append(&mut candidates.history);
        candidates.knowledge = passage_sources;
        let context = ChatContext {
            evidence: Some(evidence),
            memories: Vec::new(),
            task: prompt_task,
        };
        let Some(settings) = recipe else {
            let target = remote.ok_or_else(|| {
                InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first.")
            })?;
            return self
                .stream_remote(
                    remote::RemoteTurn {
                        turn,
                        context: Some(context),
                        candidates,
                        coverage: None,
                        sources: Vec::new(),
                        max_tokens: options.max_tokens,
                        cancellation,
                        images,
                        external: Vec::new(),
                        web: false,
                        attachments: true,
                    },
                    target,
                    events,
                )
                .await;
        };
        let mut request = settings.request(
            std::mem::take(&mut turn.prompt),
            std::mem::take(&mut turn.history),
        );
        request.chat = Some(agent_core::inference::ChatGeneration {
            adaptive: options.max_tokens.is_none(),
            replay: None,
        });
        request.context = Some(context);
        self.stream_chat(
            turn,
            request,
            events,
            StreamContext {
                previous: None,
                cancellation,
                sources: Vec::new(),
                coverage: None,
                candidates,
                external: Vec::new(),
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_scale_with_context_within_bounds() {
        assert_eq!(budget(0), 1536);
        assert_eq!(budget(4096), 7168);
        assert_eq!(budget(1_000_000), 24 * 1024);
    }

    #[test]
    fn requests_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<AttachmentRequest>(serde_json::json!({
                "ids": [], "share": {"provider": 1, "ids": [], "everything": true}
            }))
            .is_err()
        );
        let parsed: AttachmentRequest =
            serde_json::from_value(serde_json::json!({"ids": ["a"]})).unwrap();
        assert!(parsed.share.is_none());
    }

    #[test]
    fn evidence_lines_name_the_file_and_location() {
        assert_eq!(
            evidence_line("a.pdf", PassageSource::Page { page: 3 }, "Text"),
            "a.pdf · page 3\nText"
        );
        assert!(
            evidence_line("scan.png", PassageSource::ImageText, "T").contains("recognition errors")
        );
    }
}
