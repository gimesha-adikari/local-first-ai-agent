//! Transactional knowledge storage. FTS rowids track chunk IDs.

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    knowledge::{
        DocumentChunk, DocumentFormat, MAX_CHUNK_BYTES, MAX_CHUNKS, MAX_DOCUMENT_ID, MAX_DOCUMENTS,
        MAX_LIBRARY_TEXT_BYTES, MAX_TEXT_BYTES, PdfCoverage, SourceReference, knowledge_limit,
    },
};
use rusqlite::{OptionalExtension, Row, Transaction, params, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use crate::{
    SqliteStore,
    semantic::{EmbeddingIndexState, INDEX_JOIN, INDEX_STATE, read_index_state},
    unix_ms,
};

const DOCUMENT_COLUMNS: &str = "id, name, format, file_bytes, text_bytes, chunk_count,
    indexing_status, page_count, pages_examined, pages_without_text, created_at, updated_at, revision";
const MAX_QUERY_BYTES: usize = 256;
const MAX_QUERY_TERMS: usize = 16;
const MAX_SEARCH_RESULTS: u32 = 20;
const MAX_LIST_RESULTS: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexingStatus {
    Ready,
    Partial,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocument {
    pub id: i64,
    pub name: String,
    pub format: DocumentFormat,
    pub file_bytes: u32,
    pub text_bytes: u32,
    pub chunk_count: u32,
    pub indexing_status: IndexingStatus,
    pub pdf_coverage: Option<PdfCoverage>,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryDocument {
    #[serde(flatten)]
    pub document: KnowledgeDocument,
    pub embedding_state: EmbeddingIndexState,
}

pub struct DocumentImport<'a> {
    pub name: &'a str,
    pub format: DocumentFormat,
    pub fingerprint: &'a [u8; 32],
    pub file_bytes: usize,
    pub pdf_coverage: Option<PdfCoverage>,
    pub chunks: &'a [DocumentChunk],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ImportOutcome {
    Imported { document_id: i64 },
    Replaced { document_id: i64 },
    Duplicate { document_id: i64 },
}

#[derive(Clone, Copy)]
pub enum WriteStage {
    Indexing,
    Committing,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeSearch {
    pub query: String,
    pub document_ids: Option<Vec<i64>>,
    pub limit: u32,
}

impl KnowledgeSearch {
    pub fn validate(&self) -> AgentResult<()> {
        validate_search(self).map(|_| ())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeHit {
    pub document_id: i64,
    pub document_name: String,
    /// Zero-based within the current version of this document.
    pub ordinal: u32,
    pub revision: i64,
    pub source: SourceReference,
    pub text: String,
    /// SQLite BM25 score. Lower values rank first.
    pub score: f64,
}

impl KnowledgeHit {
    pub fn labels(&self) -> agent_core::classification::Labels {
        self.source.labels()
    }
}

impl SqliteStore {
    pub fn save_knowledge(
        &mut self,
        input: &DocumentImport<'_>,
        replace: Option<i64>,
        mut checkpoint: impl FnMut(WriteStage) -> AgentResult<()>,
    ) -> AgentResult<ImportOutcome> {
        checkpoint(WriteStage::Indexing)?;
        if let Some(id) = replace {
            validate_document_id(id)?;
        }
        let text_bytes: usize = input.chunks.iter().map(|chunk| chunk.text.len()).sum();
        if input.file_bytes > input.format.max_file_bytes()
            || text_bytes > MAX_TEXT_BYTES
            || input.chunks.len() > MAX_CHUNKS
            || input
                .chunks
                .iter()
                .any(|chunk| chunk.text.len() > MAX_CHUNK_BYTES)
        {
            return Err(knowledge_limit());
        }
        if input.chunks.is_empty() {
            return Err(AgentError::new(
                AgentErrorCode::InvalidArguments,
                "The selected document has no indexable text.",
            ));
        }

        let tx = self.connection.transaction().map_err(sql_error)?;
        if let Some(id) = replace {
            require_document(&tx, id)?;
        }
        let duplicate = tx
            .query_row(
                "SELECT id FROM knowledge_documents WHERE fingerprint = ?1",
                [input.fingerprint.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        if let Some(document_id) = duplicate {
            checkpoint(WriteStage::Committing)?;
            return Ok(ImportOutcome::Duplicate { document_id });
        }

        let (count, bytes): (u32, u32) = tx
            .query_row(
                "SELECT count(*), coalesce(sum(text_bytes), 0) FROM knowledge_documents
                 WHERE id <> ?1",
                [replace.unwrap_or(0)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_error)?;
        if count as usize >= MAX_DOCUMENTS || bytes as usize + text_bytes > MAX_LIBRARY_TEXT_BYTES {
            return Err(knowledge_limit());
        }

        let now = unix_ms().map_err(|_| storage_error())?;
        let format = match input.format {
            DocumentFormat::Txt => "txt",
            DocumentFormat::Markdown => "markdown",
            DocumentFormat::Pdf => "pdf",
        };
        let coverage = input.pdf_coverage;
        let status = if coverage.is_some_and(PdfCoverage::is_partial) {
            "partial"
        } else {
            "ready"
        };
        if let Some(id) = replace {
            tx.execute(
                "DELETE FROM knowledge_embedding_indexes WHERE document_id=?1",
                [id],
            )
            .map_err(sql_error)?;
            remove_chunks(&tx, id)?;
        }
        let id: i64 = tx.query_row(
            "INSERT INTO knowledge_documents (id, name, format, fingerprint, file_bytes,
             text_bytes, chunk_count, indexing_status, page_count, pages_examined,
             pages_without_text, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, format=excluded.format,
                 fingerprint=excluded.fingerprint, file_bytes=excluded.file_bytes,
                 text_bytes=excluded.text_bytes, chunk_count=excluded.chunk_count,
                 indexing_status=excluded.indexing_status, page_count=excluded.page_count,
                 pages_examined=excluded.pages_examined, pages_without_text=excluded.pages_without_text,
                 revision=knowledge_documents.revision+1,
                 updated_at=max(knowledge_documents.updated_at, excluded.updated_at)
             RETURNING id",
            params![
                replace, input.name, format, input.fingerprint.as_slice(), input.file_bytes as u32,
                text_bytes as u32, input.chunks.len() as u32, status, coverage.map(|pdf| pdf.page_count),
                coverage.map(|pdf| pdf.pages_examined), coverage.map(|pdf| pdf.pages_without_text), now,
            ],
            |row| row.get(0),
        ).map_err(sql_error)?;
        {
            let mut insert_chunk = tx
                .prepare("INSERT INTO knowledge_chunks (document_id, ordinal, page, start_line, end_line)
                          VALUES (?1, ?2, ?3, ?4, ?5)")
                .map_err(sql_error)?;
            let mut insert_text = tx
                .prepare("INSERT INTO knowledge_fts(rowid, text) VALUES (?1, ?2)")
                .map_err(sql_error)?;
            for (ordinal, chunk) in input.chunks.iter().enumerate() {
                checkpoint(WriteStage::Indexing)?;
                let (page, start, end) = match chunk.source {
                    SourceReference::Page { page } => (Some(page), None, None),
                    SourceReference::Lines {
                        start_line,
                        end_line,
                    } => (None, Some(start_line), Some(end_line)),
                };
                insert_chunk
                    .execute(params![id, ordinal as u32, page, start, end])
                    .map_err(sql_error)?;
                insert_text
                    .execute(params![tx.last_insert_rowid(), chunk.text])
                    .map_err(sql_error)?;
            }
        }
        checkpoint(WriteStage::Committing)?;
        tx.commit().map_err(sql_error)?;
        Ok(if replace.is_some() {
            ImportOutcome::Replaced { document_id: id }
        } else {
            ImportOutcome::Imported { document_id: id }
        })
    }

    pub fn knowledge_documents(
        &self,
        before_id: Option<i64>,
        limit: u32,
    ) -> AgentResult<Vec<KnowledgeDocument>> {
        if let Some(id) = before_id {
            validate_document_id(id)?;
        }
        if !(1..=MAX_LIST_RESULTS).contains(&limit) {
            return Err(invalid_request());
        }
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {DOCUMENT_COLUMNS} FROM knowledge_documents
             WHERE id < coalesce(?1, 9223372036854775807) ORDER BY id DESC LIMIT ?2"
            ))
            .map_err(sql_error)?;
        statement
            .query_map(params![before_id, limit], read_document)
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }

    pub fn knowledge_library(
        &self,
        key: agent_core::semantic::EmbeddingKey,
    ) -> AgentResult<Vec<LibraryDocument>> {
        let columns = DOCUMENT_COLUMNS
            .split(',')
            .map(|name| format!("d.{}", name.trim()))
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = self.connection.prepare(&format!(
            "SELECT {columns}, {INDEX_STATE} FROM knowledge_documents d {INDEX_JOIN} ORDER BY d.id DESC LIMIT {MAX_DOCUMENTS}"
        )).map_err(sql_error)?;
        stmt.query_map(
            params![key.fingerprint.as_slice(), key.preprocessing],
            |row| {
                Ok(LibraryDocument {
                    document: read_document(row)?,
                    embedding_state: read_index_state(row, 13)?,
                })
            },
        )
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)
    }

    pub fn knowledge_document(&self, id: i64) -> AgentResult<KnowledgeDocument> {
        validate_document_id(id)?;
        self.connection
            .query_row(
                &format!("SELECT {DOCUMENT_COLUMNS} FROM knowledge_documents WHERE id=?1"),
                [id],
                read_document,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(not_found)
    }

    pub fn delete_knowledge_document(&mut self, id: i64) -> AgentResult<()> {
        validate_document_id(id)?;
        let tx = self.connection.transaction().map_err(sql_error)?;
        require_document(&tx, id)?;
        remove_chunks(&tx, id)?;
        tx.execute("DELETE FROM knowledge_documents WHERE id=?1", [id])
            .map_err(sql_error)?;
        tx.commit().map_err(sql_error)
    }

    pub fn search_knowledge(&self, request: &KnowledgeSearch) -> AgentResult<Vec<KnowledgeHit>> {
        self.search_knowledge_candidates(request, request.limit as usize, false)
    }

    /// Passages containing every token of `request.query` consecutively.
    pub(crate) fn search_phrase(
        &self,
        request: &KnowledgeSearch,
    ) -> AgentResult<Vec<KnowledgeHit>> {
        validate_search(request)?;
        let phrase = format!("\"{}\"", request.query.replace('"', "\"\""));
        let mut sql = String::from(
            "SELECT d.id, d.name, c.ordinal, c.page, c.start_line, c.end_line, k.text,
                    bm25(knowledge_fts) AS score, d.revision
             FROM knowledge_fts k JOIN knowledge_chunks c ON c.id=k.rowid
             JOIN knowledge_documents d ON d.id=c.document_id
             WHERE knowledge_fts MATCH ?",
        );
        let mut bindings = vec![Value::Text(phrase)];
        let ids = request.document_ids.as_deref().unwrap_or_default();
        if request.document_ids.is_some() {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(" AND c.document_id IN (");
            sql.push_str(&vec!["?"; ids.len()].join(","));
            sql.push(')');
            bindings.extend(ids.iter().copied().map(Value::Integer));
        }
        // Document order, then reading order, so matches read like a listing.
        sql.push_str(" ORDER BY d.id, c.ordinal LIMIT ?");
        bindings.push(Value::Integer(i64::from(request.limit)));
        let mut statement = self.connection.prepare(&sql).map_err(sql_error)?;
        statement
            .query_map(params_from_iter(bindings), read_hit)
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }

    pub(crate) fn search_knowledge_candidates(
        &self,
        request: &KnowledgeSearch,
        fetch_limit: usize,
        chat_query: bool,
    ) -> AgentResult<Vec<KnowledgeHit>> {
        let validated = validate_search(request)?;
        let query = if chat_query {
            let terms = agent_core::chat_context::search_terms(&request.query, MAX_QUERY_TERMS);
            if terms.is_empty() {
                return Ok(Vec::new());
            }
            terms
                .iter()
                .map(|term| format!("\"{term}\""))
                .collect::<Vec<_>>()
                .join(" OR ")
        } else {
            validated
        };

        let mut sql = String::from(
            "SELECT d.id, d.name, c.ordinal, c.page, c.start_line, c.end_line, k.text,
                    bm25(knowledge_fts) AS score, d.revision
             FROM knowledge_fts k JOIN knowledge_chunks c ON c.id=k.rowid
             JOIN knowledge_documents d ON d.id=c.document_id
             WHERE knowledge_fts MATCH ?",
        );
        let mut bindings = vec![Value::Text(query)];
        if let Some(ids) = &request.document_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(" AND c.document_id IN (");
            sql.push_str(&vec!["?"; ids.len()].join(","));
            sql.push(')');
            for &id in ids {
                bindings.push(Value::Integer(id));
            }
        }
        sql.push_str(" ORDER BY score, d.id, c.ordinal LIMIT ?");
        // Chat ranking filters afterwards, so examine a wider bounded window.
        let window = if chat_query {
            fetch_limit.max(agent_core::semantic::RETRIEVAL_CANDIDATES)
        } else {
            fetch_limit
        };
        bindings.push(Value::Integer(window as i64));
        let mut statement = self.connection.prepare(&sql).map_err(sql_error)?;
        let hits: Vec<KnowledgeHit> = statement
            .query_map(params_from_iter(bindings), read_hit)
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)?;
        if !chat_query {
            return Ok(hits);
        }
        // Natural-language ranking uses OR, so one shared common word would
        // match. Keep passages that share at least half of the significant terms.
        let terms = agent_core::chat_context::search_terms(&request.query, MAX_QUERY_TERMS);
        Ok(hits
            .into_iter()
            .filter(|hit| agent_core::chat_route::lexically_relevant(&terms, &hit.text))
            .take(fetch_limit)
            .collect())
    }
}

pub(crate) fn read_hit(row: &Row<'_>) -> rusqlite::Result<KnowledgeHit> {
    let source = match row.get(3)? {
        Some(page) => SourceReference::Page { page },
        None => SourceReference::Lines {
            start_line: row.get(4)?,
            end_line: row.get(5)?,
        },
    };
    Ok(KnowledgeHit {
        document_id: row.get(0)?,
        document_name: row.get(1)?,
        ordinal: row.get(2)?,
        source,
        text: row.get(6)?,
        score: row.get(7)?,
        revision: row.get(8)?,
    })
}

pub(crate) fn validate_search(request: &KnowledgeSearch) -> AgentResult<String> {
    let query = fts_query(&request.query)?;
    if !(1..=MAX_SEARCH_RESULTS).contains(&request.limit) {
        return Err(invalid_request());
    }
    validate_document_filter(request.document_ids.as_deref())?;
    Ok(query)
}

pub(crate) fn validate_document_filter(ids: Option<&[i64]>) -> AgentResult<()> {
    if let Some(ids) = ids {
        if ids.len() > MAX_DOCUMENTS {
            return Err(invalid_request());
        }
        for &id in ids {
            validate_document_id(id)?;
        }
    }
    Ok(())
}

pub fn validate_document_id(id: i64) -> AgentResult<()> {
    if (1..=MAX_DOCUMENT_ID).contains(&id) {
        Ok(())
    } else {
        Err(invalid_request())
    }
}

fn fts_query(query: &str) -> AgentResult<String> {
    if query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
        return Err(invalid_request());
    }
    let mut phrases = Vec::new();
    for term in query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
    {
        if phrases.len() == MAX_QUERY_TERMS {
            return Err(invalid_request());
        }
        phrases.push(format!("\"{}\"", term.replace('"', "\"\"")));
    }
    if phrases.is_empty() {
        return Err(invalid_request());
    }
    Ok(phrases.join(" AND "))
}

fn read_document(row: &Row<'_>) -> rusqlite::Result<KnowledgeDocument> {
    let format = match row.get_ref(2)?.as_str()? {
        "txt" => DocumentFormat::Txt,
        "markdown" => DocumentFormat::Markdown,
        "pdf" => DocumentFormat::Pdf,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let indexing_status = match row.get_ref(6)?.as_str()? {
        "ready" => IndexingStatus::Ready,
        "partial" => IndexingStatus::Partial,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let pdf_coverage = if format == DocumentFormat::Pdf {
        Some(PdfCoverage {
            page_count: row.get(7)?,
            pages_examined: row.get(8)?,
            pages_without_text: row.get(9)?,
        })
    } else {
        None
    };
    Ok(KnowledgeDocument {
        id: row.get(0)?,
        name: row.get(1)?,
        format,
        file_bytes: row.get(3)?,
        text_bytes: row.get(4)?,
        chunk_count: row.get(5)?,
        indexing_status,
        pdf_coverage,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        revision: row.get(12)?,
    })
}

fn require_document(tx: &Transaction<'_>, id: i64) -> AgentResult<()> {
    tx.query_row(
        "SELECT 1 FROM knowledge_documents WHERE id=?1",
        [id],
        |_| Ok(()),
    )
    .optional()
    .map_err(sql_error)?
    .ok_or_else(not_found)
}

fn remove_chunks(tx: &Transaction<'_>, id: i64) -> AgentResult<()> {
    tx.execute(
        "DELETE FROM knowledge_fts WHERE rowid IN
         (SELECT id FROM knowledge_chunks WHERE document_id=?1)",
        [id],
    )
    .map_err(sql_error)?;
    tx.execute("DELETE FROM knowledge_chunks WHERE document_id=?1", [id])
        .map_err(sql_error)?;
    Ok(())
}

pub(crate) fn invalid_request() -> AgentError {
    AgentError::new(
        AgentErrorCode::InvalidArguments,
        "The knowledge request is invalid.",
    )
}

fn not_found() -> AgentError {
    AgentError::new(
        AgentErrorCode::KnowledgeNotFound,
        "The knowledge document was not found.",
    )
}

fn sql_error(_: rusqlite::Error) -> AgentError {
    storage_error()
}

pub fn storage_error() -> AgentError {
    AgentError::new(
        AgentErrorCode::StorageFailed,
        "Local knowledge storage is unavailable.",
    )
}
