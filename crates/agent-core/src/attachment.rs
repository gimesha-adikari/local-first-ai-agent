//! Conversation attachments: limits, kinds, retention and the per-attempt
//! record of what attached content an answer actually used.
//!
//! Attached files are untrusted data. Selecting one permits only local
//! preparation; remote transmission and retention each need their own
//! explicit authorization, checked by the engine.

use serde::{Deserialize, Serialize};

use crate::classification::{
    ATTACHMENT, ATTACHMENT_IMAGE, ATTACHMENT_TEXT, CAMERA_IMAGE, CAMERA_TEXT, Labels,
};

/// Attachments in one composer draft or conversation at the same time.
pub const MAX_SCOPE_ATTACHMENTS: usize = 6;
/// Attachments held in memory by one application session, across scopes.
pub const MAX_SESSION_ATTACHMENTS: usize = 24;
/// Attached content held in memory by one application session.
pub const MAX_SESSION_BYTES: usize = 64 * 1024 * 1024;
/// Largest accepted text or Markdown file.
pub const MAX_TEXT_FILE_BYTES: usize = crate::knowledge::MAX_TEXT_BYTES;
/// Largest accepted PDF file.
pub const MAX_PDF_FILE_BYTES: usize = crate::agent::MAX_PDF_BYTES as usize;
/// Largest accepted PNG or JPEG file.
pub const MAX_IMAGE_FILE_BYTES: usize = 10 * 1024 * 1024;
/// Longest accepted image side, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 8192;
/// Most decoded pixels: about 96 MB as RGBA, the largest decode admitted.
pub const MAX_IMAGE_PIXELS: u64 = 24_000_000;
/// Evidence passages one answer can cite (S1 to S10).
pub const MAX_ATTACHMENT_SOURCES: usize = 10;
/// Retained attachments one conversation can keep.
pub const MAX_RETAINED_PER_CONVERSATION: usize = 16;
/// A temporary attachment unused for this long expires.
pub const TEMPORARY_IDLE_SECONDS: u64 = 2 * 60 * 60;
/// Longest a single file's preparation may take.
pub const PREPARE_DEADLINE_SECONDS: u64 = 60;
/// Length of an opaque attachment identifier: `a` and 32 hex digits.
pub const ATTACHMENT_ID_LEN: usize = 33;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Text,
    Markdown,
    Pdf,
    Png,
    Jpeg,
}

impl AttachmentKind {
    pub const fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg)
    }

    pub const fn max_file_bytes(self) -> usize {
        match self {
            Self::Text | Self::Markdown => MAX_TEXT_FILE_BYTES,
            Self::Pdf => MAX_PDF_FILE_BYTES,
            Self::Png | Self::Jpeg => MAX_IMAGE_FILE_BYTES,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Pdf => "pdf",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "text" => Self::Text,
            "markdown" => Self::Markdown,
            "pdf" => Self::Pdf,
            "png" => Self::Png,
            "jpeg" => Self::Jpeg,
            _ => return None,
        })
    }

    /// Labels of text derived from this kind of file: extracted document
    /// text, or text recognized from an image (which keeps the image class).
    pub fn text_labels(self) -> Labels {
        let mut source = Labels::from_source(ATTACHMENT);
        if self.is_image() {
            source.insert(ATTACHMENT_IMAGE);
        }
        Labels::derived([source], ATTACHMENT_TEXT)
    }

    /// Labels of a validated image representation.
    pub fn image_labels() -> Labels {
        Labels::derived([Labels::from_source(ATTACHMENT)], ATTACHMENT_IMAGE)
    }
}

/// Where an attachment's content came from. The origin only ever adds
/// labels: a camera snapshot keeps its raw-sensor class wherever it goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentOrigin {
    /// A file the user chose with the native picker.
    #[default]
    File,
    /// A camera snapshot the user captured, previewed and confirmed.
    Camera,
}

impl AttachmentOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Camera => "camera",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "file" => Self::File,
            "camera" => Self::Camera,
            _ => return None,
        })
    }

    /// Labels of text derived from an attachment of `kind` from this origin.
    pub fn text_labels(self, kind: AttachmentKind) -> Labels {
        let mut labels = kind.text_labels();
        if self == Self::Camera {
            labels.insert(CAMERA_IMAGE);
            labels.insert(CAMERA_TEXT);
        }
        labels
    }

    /// Labels of the validated image representation from this origin.
    pub fn image_labels(self) -> Labels {
        let mut labels = AttachmentKind::image_labels();
        if self == Self::Camera {
            labels.insert(CAMERA_IMAGE);
        }
        labels
    }
}

/// How long attached content is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRetention {
    /// In memory for this application session only. The default.
    #[default]
    Temporary,
    /// Explicitly kept in the encrypted profile with its conversation.
    Conversation,
}

/// Whether `id` has the shape of an attachment identifier. Identifiers are
/// random and unguessable, but the engine still checks every use against
/// the owning scope.
pub fn valid_attachment_id(id: &str) -> bool {
    id.len() == ATTACHMENT_ID_LEN
        && id.starts_with('a')
        && id[1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Where a passage comes from inside its attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PassageSource {
    Lines {
        start_line: u32,
        end_line: u32,
    },
    Page {
        page: u32,
    },
    /// Text recognized from an image by local OCR.
    ImageText,
}

impl PassageSource {
    pub fn describe(self) -> String {
        match self {
            Self::Lines {
                start_line,
                end_line,
            } if start_line == end_line => format!("line {start_line}"),
            Self::Lines {
                start_line,
                end_line,
            } => format!("lines {start_line}–{end_line}"),
            Self::Page { page } => format!("page {page}"),
            Self::ImageText => "text recognized from the image".to_owned(),
        }
    }
}

/// PDF page coverage of an extracted attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCoverage {
    pub page_count: u32,
    pub pages_examined: u32,
    pub pages_without_text: u32,
}

impl PageCoverage {
    pub fn is_partial(self) -> bool {
        self.pages_examined < self.page_count || self.pages_without_text != 0
    }
}

/// What the model was asked to do with the attached evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentTask {
    /// The whole readable content fit and was supplied.
    #[default]
    Complete,
    /// Passages ranked against the question.
    Relevant,
    /// A representative spread for an overview or summary.
    Representative,
    /// Passages that contain literal search text.
    Search,
}

/// One attachment an answer used, as recorded with the attempt. Only names
/// and locations are saved here, never attached content.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUse {
    pub id: String,
    pub name: String,
    pub kind: AttachmentKind,
    #[serde(default)]
    pub origin: AttachmentOrigin,
    pub retention: AttachmentRetention,
    /// Readable passages the attachment has.
    pub total_passages: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PageCoverage>,
    /// Text was recognized locally from this image.
    #[serde(default)]
    pub recognized_text: bool,
    /// The validated image itself was placed in the model request.
    #[serde(default)]
    pub image_sent: bool,
}

/// One evidence line offered to the model, in citation order. Fitting keeps
/// a prefix: the first `fitted.sources` passages were actually supplied.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassageRecord {
    pub attachment: String,
    pub ordinal: u32,
    pub source: PassageSource,
    #[serde(default)]
    pub cited: bool,
}

/// Attachment metadata of one attempt (Stage 04-D).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRecord {
    pub version: u32,
    pub task: AttachmentTask,
    pub attachments: Vec<AttachmentUse>,
    pub passages: Vec<PassageRecord>,
    /// Attached content was sent to a remote or self-hosted provider under
    /// a per-request authorization.
    #[serde(default)]
    pub shared_remotely: bool,
    /// No passage matched the question, so representative passages were
    /// supplied instead.
    #[serde(default)]
    pub no_match: bool,
}

impl AttachmentRecord {
    pub fn uses(&self, id: &str) -> bool {
        self.attachments.iter().any(|item| item.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_kinds_and_labels() {
        assert!(valid_attachment_id(&format!("a{}", "0f".repeat(16))));
        for bad in [
            "",
            "a",
            "b0123",
            &format!("a{}", "0F".repeat(16)),
            &format!("a{}", "0".repeat(33)),
        ] {
            assert!(!valid_attachment_id(bad), "{bad}");
        }
        for kind in [
            AttachmentKind::Text,
            AttachmentKind::Markdown,
            AttachmentKind::Pdf,
            AttachmentKind::Png,
            AttachmentKind::Jpeg,
        ] {
            assert_eq!(AttachmentKind::parse(kind.as_str()), Some(kind));
        }
        // Recognized text keeps the image class: recognition never declassifies.
        assert!(AttachmentKind::Png.text_labels().contains(ATTACHMENT_IMAGE));
        assert!(!AttachmentKind::Pdf.text_labels().contains(ATTACHMENT_IMAGE));
        assert!(AttachmentKind::image_labels().contains(ATTACHMENT));
        // A camera origin adds its sensor classes; a file origin adds none.
        let camera = AttachmentOrigin::Camera.text_labels(AttachmentKind::Png);
        for class in [
            ATTACHMENT,
            ATTACHMENT_IMAGE,
            ATTACHMENT_TEXT,
            CAMERA_IMAGE,
            CAMERA_TEXT,
        ] {
            assert!(camera.contains(class), "{}", class.id);
        }
        assert!(
            AttachmentOrigin::Camera
                .image_labels()
                .contains(CAMERA_IMAGE)
        );
        assert_eq!(
            AttachmentOrigin::File.text_labels(AttachmentKind::Png),
            AttachmentKind::Png.text_labels()
        );
        for origin in [AttachmentOrigin::File, AttachmentOrigin::Camera] {
            assert_eq!(AttachmentOrigin::parse(origin.as_str()), Some(origin));
        }
        // Records saved before Stage 04-E have no origin and read as files.
        let legacy: AttachmentUse = serde_json::from_str(
            r#"{"id":"a","name":"n","kind":"png","retention":"temporary","totalPassages":1}"#,
        )
        .unwrap();
        assert_eq!(legacy.origin, AttachmentOrigin::File);
    }

    #[test]
    fn records_round_trip_without_content() {
        let record = AttachmentRecord {
            version: 1,
            task: AttachmentTask::Representative,
            attachments: vec![AttachmentUse {
                id: format!("a{}", "1".repeat(32)),
                name: "report.pdf".into(),
                kind: AttachmentKind::Pdf,
                origin: AttachmentOrigin::File,
                retention: AttachmentRetention::Temporary,
                total_passages: 9,
                pages: Some(PageCoverage {
                    page_count: 3,
                    pages_examined: 3,
                    pages_without_text: 1,
                }),
                recognized_text: false,
                image_sent: false,
            }],
            passages: vec![PassageRecord {
                attachment: format!("a{}", "1".repeat(32)),
                ordinal: 0,
                source: PassageSource::Page { page: 1 },
                cited: true,
            }],
            shared_remotely: false,
            no_match: false,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"kind\":\"page\""));
        assert_eq!(
            serde_json::from_str::<AttachmentRecord>(&json).unwrap(),
            record
        );
        assert_eq!(
            PassageSource::Lines {
                start_line: 2,
                end_line: 2
            }
            .describe(),
            "line 2"
        );
    }
}
