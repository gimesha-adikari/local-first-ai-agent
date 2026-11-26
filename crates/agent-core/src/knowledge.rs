//! Local knowledge document types and deterministic source-aware chunking.

use serde::{Deserialize, Serialize};

use crate::agent::{AgentError, AgentErrorCode, AgentResult, MAX_PDF_BYTES};

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 2048;
pub const MAX_CHUNKS: usize = 1024;
pub const MAX_PDF_PAGES_EXAMINED: u32 = 100;
pub const MAX_DOCUMENTS: usize = 128;
pub const MAX_LIBRARY_TEXT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_DOCUMENT_ID: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    Txt,
    Markdown,
    Pdf,
}

impl DocumentFormat {
    pub const fn max_file_bytes(self) -> usize {
        match self {
            Self::Txt | Self::Markdown => MAX_TEXT_BYTES,
            Self::Pdf => MAX_PDF_BYTES as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SourceReference {
    Lines { start_line: u32, end_line: u32 },
    Page { page: u32 },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfCoverage {
    pub page_count: u32,
    pub pages_examined: u32,
    pub pages_without_text: u32,
}

impl PdfCoverage {
    pub fn is_partial(self) -> bool {
        self.pages_examined < self.page_count || self.pages_without_text != 0
    }
}

#[derive(Debug)]
pub struct DocumentChunk {
    pub source: SourceReference,
    pub text: String,
}

pub fn append_chunks(
    chunks: &mut Vec<DocumentChunk>,
    text: &str,
    page: Option<u32>,
    check: impl Fn() -> AgentResult<()>,
) -> AgentResult<()> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(knowledge_limit());
    }

    let mut remaining = text;
    let mut line = 1;
    while !remaining.is_empty() {
        check()?;
        let end = if remaining.len() > MAX_CHUNK_BYTES {
            let boundary = remaining.floor_char_boundary(MAX_CHUNK_BYTES);
            remaining[..boundary]
                .char_indices()
                .rev()
                .take_while(|(index, _)| *index >= MAX_CHUNK_BYTES / 2)
                .find_map(|(index, ch)| ch.is_whitespace().then_some(index + ch.len_utf8()))
                .unwrap_or(boundary)
        } else {
            remaining.len()
        };
        let (text, rest) = remaining.split_at(end);
        let newlines = text.bytes().filter(|byte| *byte == b'\n').count() as u32;
        if !text.trim().is_empty() {
            if chunks.len() == MAX_CHUNKS {
                return Err(knowledge_limit());
            }
            let source = match page {
                Some(page) => SourceReference::Page { page },
                None => SourceReference::Lines {
                    start_line: line,
                    end_line: line + newlines - u32::from(text.ends_with('\n')),
                },
            };
            chunks.push(DocumentChunk {
                source,
                text: text.to_owned(),
            });
        }
        line += newlines;
        remaining = rest;
    }
    Ok(())
}

pub const fn knowledge_limit() -> AgentError {
    AgentError::new(
        AgentErrorCode::KnowledgeLimit,
        "A local knowledge document or library limit was reached.",
    )
}
