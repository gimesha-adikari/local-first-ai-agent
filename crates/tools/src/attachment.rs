//! Trusted access to files attached to a Chat message from the native
//! picker. The selection is pinned to an open handle immediately; reading
//! re-verifies that handle and produces an immutable in-memory snapshot, so
//! a file replaced, changed or removed after selection is detected and the
//! validated bytes are the processed bytes. The original file is only read.

use std::{ffi::OsStr, fs::File, path::Path};

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    attachment::AttachmentKind,
    knowledge::{DocumentFormat, knowledge_limit},
};
use zeroize::Zeroizing;

use crate::{
    filesystem::{pin_selected_regular, read_bytes, reopen_regular, selected_display_name},
    image::{self, ValidatedImage},
    knowledge::{ExtractedKnowledge, extract},
};

/// A native picker selection. Only trusted host code constructs it.
pub struct SelectedAttachment {
    handle: File,
    name: String,
    kind: AttachmentKind,
    file_bytes: u64,
}

fn kind_from_name(path: &Path) -> AgentResult<AttachmentKind> {
    Ok(
        match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("txt" | "text") => AttachmentKind::Text,
            Some("md" | "markdown") => AttachmentKind::Markdown,
            Some("pdf") => AttachmentKind::Pdf,
            Some("png") => AttachmentKind::Png,
            Some("jpg" | "jpeg") => AttachmentKind::Jpeg,
            _ => {
                return Err(AgentError::new(
                    AgentErrorCode::InvalidArguments,
                    "Attach a text, Markdown, PDF, PNG or JPEG file.",
                ));
            }
        },
    )
}

fn oversized() -> AgentError {
    AgentError::new(
        AgentErrorCode::KnowledgeLimit,
        "The file is larger than attachments of this type allow.",
    )
}

impl SelectedAttachment {
    pub fn pin(path: &Path) -> AgentResult<Self> {
        let kind = kind_from_name(path)?;
        let handle = pin_selected_regular(path)?;
        let file_bytes = handle
            .metadata()
            .map_err(|_| {
                AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "The selected file is unavailable.",
                )
            })?
            .len();
        if file_bytes == 0 {
            return Err(AgentError::new(
                AgentErrorCode::InvalidArguments,
                "The selected file is empty.",
            ));
        }
        if file_bytes > kind.max_file_bytes() as u64 {
            return Err(oversized());
        }
        Ok(Self {
            handle,
            name: selected_display_name(path).unwrap_or_else(|| "Attachment".into()),
            kind,
            file_bytes,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> AttachmentKind {
        self.kind
    }

    pub fn file_bytes(&self) -> u64 {
        self.file_bytes
    }

    /// Read the pinned file once into an owned snapshot, then drop the
    /// descriptor. The content must match the file type.
    pub fn read(self, check: &dyn Fn() -> AgentResult<()>) -> AgentResult<AttachmentSnapshot> {
        let file = reopen_regular(&self.handle, check)?;
        drop(self.handle);
        let limit: fn() -> AgentError = if self.kind.is_image() {
            oversized
        } else {
            knowledge_limit
        };
        let bytes = Zeroizing::new(read_bytes(&file, self.kind.max_file_bytes(), limit, check)?);
        let matches = match self.kind {
            AttachmentKind::Pdf => bytes.starts_with(b"%PDF-"),
            AttachmentKind::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            AttachmentKind::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            AttachmentKind::Text | AttachmentKind::Markdown => true,
        };
        if !matches {
            return Err(AgentError::new(
                AgentErrorCode::InvalidArguments,
                "The file's contents don't match its type.",
            ));
        }
        Ok(AttachmentSnapshot {
            kind: self.kind,
            bytes,
        })
    }
}

/// The bytes read from a selection. Wiped when dropped.
pub struct AttachmentSnapshot {
    kind: AttachmentKind,
    bytes: Zeroizing<Vec<u8>>,
}

impl AttachmentSnapshot {
    pub fn kind(&self) -> AttachmentKind {
        self.kind
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Extract chunks from a text, Markdown or text-layer PDF document.
    pub fn extract_document(
        &self,
        check: &dyn Fn() -> AgentResult<()>,
    ) -> AgentResult<ExtractedKnowledge> {
        let format = match self.kind {
            AttachmentKind::Text => DocumentFormat::Txt,
            AttachmentKind::Markdown => DocumentFormat::Markdown,
            AttachmentKind::Pdf => DocumentFormat::Pdf,
            AttachmentKind::Png | AttachmentKind::Jpeg => {
                return Err(AgentError::new(
                    AgentErrorCode::InvalidArguments,
                    "Images have no document text to extract.",
                ));
            }
        };
        extract(format, &self.bytes, check).map_err(|error| match error.code {
            AgentErrorCode::PdfNoText => AgentError::new(
                AgentErrorCode::PdfNoText,
                "This PDF has no text layer, so it may be a scan. Reading scanned PDFs isn't supported yet.",
            ),
            _ => error,
        })
    }

    /// Validate an image and remove its metadata.
    pub fn validate_image(
        &self,
        check: &dyn Fn() -> AgentResult<()>,
    ) -> AgentResult<ValidatedImage> {
        image::validate(self.kind, &self.bytes, check)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn ok() -> AgentResult<()> {
        Ok(())
    }

    #[test]
    fn selections_are_pinned_typed_and_detect_later_changes() {
        let dir = std::env::temp_dir().join(format!("attach-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let notes = dir.join("notes.MD");
        std::fs::write(&notes, "# Synthetic\nThe pump runs at noon.\n").unwrap();
        let selected = SelectedAttachment::pin(&notes).unwrap();
        assert_eq!(selected.kind(), AttachmentKind::Markdown);
        assert_eq!(selected.name(), "notes.MD");
        let snapshot = selected.read(&ok).unwrap();
        let extracted = snapshot.extract_document(&ok).unwrap();
        assert!(extracted.chunks[0].text.contains("pump"));

        // A change between selection and reading is read once, as a whole:
        // everything later validated and processed comes from that snapshot.
        let selected = SelectedAttachment::pin(&notes).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&notes)
            .unwrap()
            .write_all(b"Changed later.\n")
            .unwrap();
        let snapshot = selected.read(&ok).unwrap();
        assert_eq!(snapshot.bytes(), std::fs::read(&notes).unwrap().as_slice());

        // Replacing the file after selection does not redirect the read.
        let selected = SelectedAttachment::pin(&notes).unwrap();
        let replacement = dir.join("other.md");
        std::fs::write(&replacement, "Replacement content").unwrap();
        std::fs::rename(&replacement, &notes).unwrap();
        let snapshot = selected.read(&ok);
        assert!(
            snapshot.is_err()
                || !String::from_utf8_lossy(snapshot.unwrap().bytes()).contains("Replacement")
        );

        let fake = dir.join("fake.png");
        std::fs::write(&fake, "not an image").unwrap();
        assert!(SelectedAttachment::pin(&fake).unwrap().read(&ok).is_err());
        let empty = dir.join("empty.txt");
        std::fs::write(&empty, "").unwrap();
        assert!(SelectedAttachment::pin(&empty).is_err());
        let archive = dir.join("data.zip");
        std::fs::write(&archive, "PK").unwrap();
        assert!(SelectedAttachment::pin(&archive).is_err());
        let large = dir.join("large.txt");
        std::fs::write(
            &large,
            vec![b'a'; agent_core::attachment::MAX_TEXT_FILE_BYTES + 1],
        )
        .unwrap();
        assert_eq!(
            SelectedAttachment::pin(&large).err().unwrap().code,
            AgentErrorCode::KnowledgeLimit
        );
        // The original file is never modified.
        assert_eq!(
            std::fs::read_to_string(&notes).unwrap(),
            "Replacement content"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
