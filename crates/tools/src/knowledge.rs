//! One-use local document access for the trusted native picker.

use std::{ffi::OsStr, fs::File, path::Path};

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    knowledge::{
        DocumentChunk, DocumentFormat, MAX_PDF_PAGES_EXAMINED, MAX_TEXT_BYTES, PdfCoverage,
        append_chunks, knowledge_limit,
    },
};

use crate::{
    filesystem::{
        pin_selected_regular, read_bytes, reopen_regular, selected_display_name, utf8_text,
    },
    pdf,
};

/// Only trusted host code may construct this from a native picker result.
/// No Agent permission or path is retained.
pub struct SelectedKnowledgeFile {
    handle: File,
    display_name: String,
    format: DocumentFormat,
}

impl SelectedKnowledgeFile {
    pub fn pin(path: &Path) -> AgentResult<Self> {
        let format = match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("txt") => DocumentFormat::Txt,
            Some("md" | "markdown") => DocumentFormat::Markdown,
            Some("pdf") => DocumentFormat::Pdf,
            _ => {
                return Err(AgentError::new(
                    AgentErrorCode::InvalidArguments,
                    "Select a TXT, Markdown, or PDF document.",
                ));
            }
        };
        Ok(Self {
            handle: pin_selected_regular(path)?,
            display_name: selected_display_name(path).unwrap_or_else(|| "Document".into()),
            format,
        })
    }

    /// Consume the selection and drop the descriptor once reading finishes.
    pub fn read(self, check: impl Fn() -> AgentResult<()>) -> AgentResult<KnowledgeFileContents> {
        let file = reopen_regular(&self.handle, &check)?;
        let bytes = read_bytes(&file, self.format.max_file_bytes(), knowledge_limit, &check)?;
        Ok(KnowledgeFileContents {
            display_name: self.display_name,
            format: self.format,
            bytes,
        })
    }
}

pub struct KnowledgeFileContents {
    pub display_name: String,
    pub format: DocumentFormat,
    bytes: Vec<u8>,
}

impl KnowledgeFileContents {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn extract(&self, check: impl Fn() -> AgentResult<()>) -> AgentResult<ExtractedKnowledge> {
        extract(self.format, &self.bytes, &check)
    }
}

/// Extract source-aware chunks from document bytes already read from a
/// pinned selection. PDFs are parsed only inside the sandbox.
pub(crate) fn extract(
    format: DocumentFormat,
    bytes: &[u8],
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<ExtractedKnowledge> {
    check()?;
    let mut chunks = Vec::new();
    let pdf_coverage = if format == DocumentFormat::Pdf {
        let page_count = pdf::page_count(bytes, check)?;
        let pages_examined = page_count.min(MAX_PDF_PAGES_EXAMINED);
        let text = pdf::extract_text(bytes, 1, pages_examined, MAX_TEXT_BYTES, check)?;
        let mut pages_without_text = 0;
        for (index, raw) in pdf::page_texts(&text, pages_examined)?.enumerate() {
            check()?;
            let text = raw
                .chars()
                .filter(|ch| !ch.is_control() || matches!(ch, '\t' | '\n'))
                .collect::<String>();
            if text.trim().is_empty() {
                pages_without_text += 1;
            } else {
                append_chunks(&mut chunks, &text, Some(index as u32 + 1), check)?;
            }
        }
        if chunks.is_empty() {
            return Err(pdf::no_text());
        }
        Some(PdfCoverage {
            page_count,
            pages_examined,
            pages_without_text,
        })
    } else {
        let text = utf8_text(bytes)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        append_chunks(&mut chunks, text, None, check)?;
        if chunks.is_empty() {
            return Err(AgentError::new(
                AgentErrorCode::InvalidArguments,
                "The selected document has no indexable text.",
            ));
        }
        None
    };
    check()?;
    Ok(ExtractedKnowledge {
        chunks,
        pdf_coverage,
    })
}

#[derive(Debug)]
pub struct ExtractedKnowledge {
    pub chunks: Vec<DocumentChunk>,
    pub pdf_coverage: Option<PdfCoverage>,
}
