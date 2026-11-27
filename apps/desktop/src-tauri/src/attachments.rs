//! Main-window IPC for conversation attachments (Stage 04-D). The host owns
//! the native multi-file picker and the trusted adapters: each selection is
//! pinned immediately, read once into a snapshot, validated, and extracted
//! by the sandboxed PDF parser or recognized by the sandboxed Tesseract.
//! The WebView never names a path and can only refer to attachments by the
//! engine's opaque identifiers, which the engine checks against the owning
//! draft or conversation on every use.

use agent_core::{
    attachment::{AttachmentRetention, MAX_SCOPE_ATTACHMENTS, valid_attachment_id},
    chat::{ConversationId, MessageId},
    inference::{ErrorCode, InferenceError, Result},
};
use engine::attachments::AttachmentView;
use serde::Serialize;
use tauri::{Manager, State, WebviewWindow};

use crate::commands::Desktop;

fn invalid(message: &str) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

fn validate_conversation(conversation: Option<ConversationId>) -> Result<()> {
    if conversation.is_some_and(|id| !(1..=agent_core::knowledge::MAX_DOCUMENT_ID).contains(&id)) {
        return Err(invalid("The conversation identifier is invalid."));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    if !valid_attachment_id(id) {
        return Err(invalid("The attachment identifier is invalid."));
    }
    Ok(())
}

/// A file the picker returned that could not be attached, with the reason.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejected {
    name: String,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chosen {
    attachments: Vec<AttachmentView>,
    rejected: Vec<Rejected>,
}

#[cfg(target_os = "linux")]
mod source {
    use agent_core::{
        agent::{AgentError, AgentErrorCode, AgentResult},
        attachment::{AttachmentKind, PageCoverage, PassageSource},
        knowledge::{SourceReference, append_chunks},
    };
    use agent_tools::{
        attachment::SelectedAttachment,
        ocr::{OcrError, OcrErrorKind, Tesseract},
    };
    use engine::attachments::{
        AttachmentFile, PreparedContent, PreparedImage, PreparedText, Stage,
    };

    /// A pinned selection and the local recognizer available for images.
    pub struct Picked {
        pub selected: SelectedAttachment,
        pub recognizer: Option<(Tesseract, Vec<String>)>,
    }

    fn passage(source: SourceReference) -> PassageSource {
        match source {
            SourceReference::Lines {
                start_line,
                end_line,
            } => PassageSource::Lines {
                start_line,
                end_line,
            },
            SourceReference::Page { page } => PassageSource::Page { page },
        }
    }

    impl AttachmentFile for Picked {
        fn name(&self) -> &str {
            self.selected.name()
        }

        fn kind(&self) -> AttachmentKind {
            self.selected.kind()
        }

        fn file_bytes(&self) -> u64 {
            self.selected.file_bytes()
        }

        fn prepare(
            self: Box<Self>,
            stage: &dyn Fn(Stage),
            check: &dyn Fn() -> AgentResult<()>,
        ) -> AgentResult<PreparedContent> {
            let Picked {
                selected,
                recognizer,
            } = *self;
            let kind = selected.kind();
            stage(Stage::Reading);
            let snapshot = selected.read(check)?;
            stage(Stage::Extracting);
            if !kind.is_image() {
                let extracted = snapshot.extract_document(check)?;
                drop(snapshot);
                return Ok(PreparedContent {
                    passages: extracted
                        .chunks
                        .into_iter()
                        .map(|chunk| PreparedText {
                            source: passage(chunk.source),
                            text: chunk.text,
                        })
                        .collect(),
                    pages: extracted.pdf_coverage.map(|coverage| PageCoverage {
                        page_count: coverage.page_count,
                        pages_examined: coverage.pages_examined,
                        pages_without_text: coverage.pages_without_text,
                    }),
                    image: None,
                    recognition: None,
                    notes: Vec::new(),
                });
            }
            let image = snapshot.validate_image(check)?;
            drop(snapshot);
            let mut passages = Vec::new();
            let mut notes = Vec::new();
            let mut recognition = None;
            match recognizer {
                Some((tesseract, languages)) if !languages.is_empty() => {
                    stage(Stage::Recognizing);
                    let outcome = tesseract.recognize_image(&image.bytes, &languages, &|| {
                        check().map_err(|_| OcrError::cancelled())
                    });
                    match outcome {
                        Ok(output) => {
                            recognition = Some(format!(
                                "{} ({})",
                                output.engine,
                                output.languages.join(", ")
                            ));
                            let mut chunks = Vec::new();
                            append_chunks(&mut chunks, &output.text, None, check)?;
                            passages.extend(chunks.into_iter().map(|chunk| PreparedText {
                                source: PassageSource::ImageText,
                                text: chunk.text,
                            }));
                            if passages.is_empty() {
                                notes.push("No text was recognized in this image.".to_owned());
                            }
                        }
                        Err(error) if error.kind == OcrErrorKind::Cancelled => {
                            return Err(AgentError::new(
                                AgentErrorCode::Cancelled,
                                "Attachment preparation was cancelled.",
                            ));
                        }
                        // The image stays usable for a provider that can view
                        // it; the limitation is shown on the attachment.
                        Err(error) => notes.push(error.message.to_owned()),
                    }
                }
                _ => notes.push(
                    "Local text recognition isn't available. Install Tesseract with English data (sudo apt install tesseract-ocr tesseract-ocr-eng) to read text in images.".to_owned(),
                ),
            }
            Ok(PreparedContent {
                passages,
                pages: None,
                image: Some(PreparedImage {
                    bytes: image.bytes,
                    width: image.width,
                    height: image.height,
                }),
                recognition,
                notes,
            })
        }
    }
}

/// What the native picker returns in desktop WebView fixtures (test builds
/// only): each queued list answers one picker request.
#[cfg(test)]
pub(crate) static PICKER_FIXTURE: std::sync::Mutex<
    std::collections::VecDeque<Vec<std::path::PathBuf>>,
> = std::sync::Mutex::new(std::collections::VecDeque::new());

/// Choose files with the native picker and start preparing them for the
/// draft (`conversationId` absent) or an open conversation.
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn attachment_choose(
    window: WebviewWindow,
    state: State<'_, Desktop>,
    conversation_id: Option<ConversationId>,
    retention: AttachmentRetention,
) -> Result<Chosen> {
    use agent_tools::attachment::SelectedAttachment;
    use engine::attachments::AttachmentFile;
    use rfd::AsyncFileDialog;

    validate_conversation(conversation_id)?;
    #[cfg(test)]
    let fixture = PICKER_FIXTURE
        .lock()
        .ok()
        .and_then(|mut queue| queue.pop_front());
    #[cfg(not(test))]
    let fixture: Option<Vec<std::path::PathBuf>> = None;
    let paths = match fixture {
        Some(paths) => paths,
        None => {
            let Some(files) = AsyncFileDialog::new()
                .set_parent(&window)
                .set_title("Attach files")
                .add_filter(
                    "Text, Markdown, PDF, PNG and JPEG",
                    &["txt", "md", "markdown", "pdf", "png", "jpg", "jpeg"],
                )
                .add_filter("All files", &["*"]) // Suffixes are also matched case-insensitively.
                .pick_files()
                .await
            else {
                return Ok(Chosen {
                    attachments: Vec::new(),
                    rejected: Vec::new(),
                });
            };
            files.iter().map(|file| file.path().to_owned()).collect()
        }
    };
    if paths.len() > MAX_SCOPE_ATTACHMENTS {
        return Err(invalid("Attach at most 6 files at a time."));
    }
    let pinned = tauri::async_runtime::spawn_blocking(move || {
        let recognizer = paths
            .iter()
            .any(|path| {
                path.extension()
                    .and_then(|suffix| suffix.to_str())
                    .is_some_and(|suffix| {
                        matches!(suffix.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg")
                    })
            })
            .then(crate::panel::screen::recognizer)
            .flatten();
        paths
            .iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().chars().take(80).collect())
                    .unwrap_or_default();
                SelectedAttachment::pin(path)
                    .map(|selected| source::Picked {
                        selected,
                        recognizer: recognizer.clone(),
                    })
                    .map_err(|error| Rejected {
                        name,
                        message: error.message.to_owned(),
                    })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|_| InferenceError::new(ErrorCode::WorkerStopped, "The file picker stopped."))?;
    let mut accepted: Vec<Box<dyn AttachmentFile>> = Vec::new();
    let mut rejected = Vec::new();
    for outcome in pinned {
        match outcome {
            Ok(picked) => accepted.push(Box::new(picked)),
            Err(reason) => rejected.push(reason),
        }
    }
    let attachments = state
        .engine
        .attach_files(conversation_id, retention, accepted)?;
    Ok(Chosen {
        attachments,
        rejected,
    })
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub async fn attachment_choose(
    _: WebviewWindow,
    _: State<'_, Desktop>,
    _: Option<ConversationId>,
    _: AttachmentRetention,
) -> Result<Chosen> {
    Err(InferenceError::new(
        ErrorCode::UnsupportedCapability,
        "Attachments are available on Linux in this build.",
    ))
}

#[tauri::command]
pub async fn attachment_list(
    state: State<'_, Desktop>,
    conversation_id: Option<ConversationId>,
) -> Result<Vec<AttachmentView>> {
    validate_conversation(conversation_id)?;
    state.engine.attachments(conversation_id).await
}

#[tauri::command]
pub fn attachment_cancel(
    state: State<'_, Desktop>,
    conversation_id: Option<ConversationId>,
    id: String,
) -> Result<bool> {
    validate_conversation(conversation_id)?;
    validate_id(&id)?;
    state.engine.cancel_attachment(conversation_id, &id)
}

#[tauri::command]
pub async fn attachment_remove(
    state: State<'_, Desktop>,
    conversation_id: Option<ConversationId>,
    id: String,
) -> Result<()> {
    validate_conversation(conversation_id)?;
    validate_id(&id)?;
    state.engine.remove_attachment(conversation_id, &id).await
}

#[tauri::command]
pub async fn attachment_set_retention(
    state: State<'_, Desktop>,
    conversation_id: Option<ConversationId>,
    id: String,
    retention: AttachmentRetention,
) -> Result<AttachmentView> {
    validate_conversation(conversation_id)?;
    validate_id(&id)?;
    state
        .engine
        .set_attachment_retention(conversation_id, &id, retention)
        .await
}

/// The text of a supplied attachment passage, while the attachment remains
/// available in the message's conversation.
#[tauri::command]
pub async fn chat_attachment_evidence(
    state: State<'_, Desktop>,
    message_id: MessageId,
    citation_id: String,
) -> Result<Option<String>> {
    if !(1..=agent_core::knowledge::MAX_DOCUMENT_ID).contains(&message_id) || citation_id.len() > 3
    {
        return Err(invalid("The citation is invalid."));
    }
    state
        .engine
        .attachment_evidence(message_id, &citation_id)
        .await
}

/// `attachment-preview://localhost/<id>`: a ready image attachment's
/// validated, metadata-stripped bytes, served only to the main window and
/// never cached.
pub fn preview_protocol(
    context: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let refuse = |status: u16| {
        tauri::http::Response::builder()
            .status(status)
            .header("Cache-Control", "no-store")
            .body(Vec::new())
            .unwrap_or_default()
    };
    if context.webview_label() != "main" || request.method() != tauri::http::Method::GET {
        return refuse(403);
    }
    let Some(id) = request
        .uri()
        .path()
        .strip_prefix('/')
        .filter(|id| valid_attachment_id(id))
    else {
        return refuse(404);
    };
    let desktop = context.app_handle().state::<Desktop>();
    match desktop.engine.attachment_preview(id) {
        Some((bytes, mime)) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", mime)
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(bytes)
            .unwrap_or_else(|_| refuse(500)),
        None => refuse(404),
    }
}
