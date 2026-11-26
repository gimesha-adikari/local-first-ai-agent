//! Retrieval snapshots and historical source resolution for ordinary Chat.

use agent_core::{
    agent::AgentResult,
    chat::{Conversation, GenerationState, MessageId},
    chat_context::{
        AttemptContext, ContextUsage, MAX_CHAT_SOURCES, OverviewCoverage, citation_id, cited_slots,
    },
    inference::Result,
    knowledge::SourceReference,
    semantic::{EmbeddingKey, UnitVector},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    SqliteStore, json_column,
    knowledge::{KnowledgeHit, KnowledgeSearch, validate_document_filter},
    not_found,
    semantic::{EmbeddingIndexState, RetrievalMode},
    sql_error, storage_error,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCitation {
    pub citation_id: String,
    pub document_id: i64,
    pub revision: i64,
    pub chunk_ordinal: u32,
    pub document_name: String,
    pub source: SourceReference,
    pub available: bool,
    pub cited: bool,
}

pub use agent_core::chat_context::KnowledgeCoverage;

pub struct ChatRetrieval {
    pub coverage: KnowledgeCoverage,
    pub sources: Vec<KnowledgeHit>,
}

impl SqliteStore {
    pub fn retrieve_for_chat(
        &self,
        query: Option<&str>,
        ids: &[i64],
        mode: RetrievalMode,
        key: EmbeddingKey,
        vector: Option<&UnitVector>,
    ) -> AgentResult<ChatRetrieval> {
        let (partial_document_ids, unavailable_document_ids) = self.selection_status(ids)?;
        // Build the search filter from the selection, including an empty filter.
        // The query can be absent for a prompt with no searchable characters.
        let (embeddings, hits) = if let Some(query) = query {
            let selected = KnowledgeSearch {
                query: query.into(),
                document_ids: Some(ids.to_vec()),
                limit: MAX_CHAT_SOURCES as u32,
            };
            let result = self.retrieve_ranked(&selected, mode, key, vector, true)?;
            (result.coverage, result.hits)
        } else {
            let coverage = (mode != RetrievalMode::Keyword)
                .then(|| self.embedding_coverage(key, Some(ids)))
                .transpose()?;
            (coverage, Vec::new())
        };
        let mut missing_index_document_ids = Vec::new();
        let mut incompatible_index_document_ids = Vec::new();
        if mode != RetrievalMode::Keyword {
            for (id, state) in self.embedding_states(key)? {
                if !ids.contains(&id) {
                    continue;
                }
                match state {
                    EmbeddingIndexState::Missing => missing_index_document_ids.push(id),
                    EmbeddingIndexState::Incompatible => incompatible_index_document_ids.push(id),
                    EmbeddingIndexState::Ready => {}
                }
            }
        }
        let unsearched_document_ids = ids
            .iter()
            .copied()
            .filter(|id| {
                query.is_none()
                    || unavailable_document_ids.contains(id)
                    || (mode == RetrievalMode::Semantic
                        && (missing_index_document_ids.contains(id)
                            || incompatible_index_document_ids.contains(id)))
            })
            .collect();
        Ok(ChatRetrieval {
            coverage: KnowledgeCoverage {
                mode,
                intent: None,
                grounding: None,
                overview: Vec::new(),
                requested_mode: None,
                embeddings,
                partial_document_ids,
                unavailable_document_ids,
                retrieved: hits.len(),
                missing_index_document_ids,
                incompatible_index_document_ids,
                unsearched_document_ids,
            },
            sources: hits,
        })
    }

    /// Partial and absent documents of a selection, in selection order.
    fn selection_status(&self, ids: &[i64]) -> AgentResult<(Vec<i64>, Vec<i64>)> {
        use crate::knowledge::storage_error;
        validate_document_filter(Some(ids))?;
        let mut stmt = self
            .connection
            .prepare("SELECT id, indexing_status='partial' FROM knowledge_documents ORDER BY id")
            .map_err(|_| storage_error())?;
        let mut rows = stmt.query([]).map_err(|_| storage_error())?;
        let mut partial_document_ids = Vec::new();
        let mut unavailable_document_ids = ids.to_vec();
        while let Some(row) = rows.next().map_err(|_| storage_error())? {
            let id: i64 = row.get(0).map_err(|_| storage_error())?;
            if ids.contains(&id) {
                unavailable_document_ids.retain(|missing| *missing != id);
                if row.get(1).map_err(|_| storage_error())? {
                    partial_document_ids.push(id);
                }
            }
        }
        Ok((partial_document_ids, unavailable_document_ids))
    }

    fn plain_coverage(
        &self,
        ids: &[i64],
        mode: RetrievalMode,
        hits: &[KnowledgeHit],
    ) -> AgentResult<KnowledgeCoverage> {
        let (partial_document_ids, unavailable_document_ids) = self.selection_status(ids)?;
        Ok(KnowledgeCoverage {
            mode,
            intent: None,
            grounding: None,
            overview: Vec::new(),
            requested_mode: None,
            embeddings: None,
            partial_document_ids,
            unsearched_document_ids: unavailable_document_ids.clone(),
            unavailable_document_ids,
            missing_index_document_ids: Vec::new(),
            incompatible_index_document_ids: Vec::new(),
            retrieved: hits.len(),
        })
    }

    /// Exact search for literal text in the selected documents. Every token of
    /// the target must appear consecutively; no dense index is needed.
    pub fn search_for_chat(
        &self,
        target: &str,
        ids: &[i64],
        mode: RetrievalMode,
    ) -> AgentResult<ChatRetrieval> {
        let target = target.trim();
        if target.is_empty() || target.len() > 256 || target.chars().any(char::is_control) {
            return Err(crate::knowledge::invalid_request());
        }
        let tokens: Vec<&str> = target
            .split(|c: char| !c.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .take(16)
            .collect();
        if tokens.is_empty() {
            return Err(crate::knowledge::invalid_request());
        }
        let hits = self.phrase_hits(&tokens.join(" "), ids)?;
        let coverage = self.plain_coverage(ids, mode, &hits)?;
        Ok(ChatRetrieval {
            coverage,
            sources: hits,
        })
    }

    /// A bounded, evenly spread sample of the selected documents' passages,
    /// read directly rather than ranked against the request.
    pub fn overview_for_chat(
        &self,
        ids: &[i64],
        mode: RetrievalMode,
    ) -> AgentResult<ChatRetrieval> {
        use crate::knowledge::storage_error;
        validate_document_filter(Some(ids))?;
        let mut documents = Vec::new();
        for &id in ids {
            let total: Option<u32> = self
                .connection
                .query_row(
                    "SELECT chunk_count FROM knowledge_documents WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| storage_error())?;
            if let Some(total) = total {
                documents.push((id, total, 0u32));
            }
        }
        // Round-robin the citation slots so each document is represented
        // before any document receives a second passage.
        let mut assigned = 0;
        while assigned < MAX_CHAT_SOURCES {
            let before = assigned;
            for (_, total, slots) in documents.iter_mut() {
                if assigned < MAX_CHAT_SOURCES && *slots < *total {
                    *slots += 1;
                    assigned += 1;
                }
            }
            if assigned == before {
                break;
            }
        }
        let mut hydrate = self
            .connection
            .prepare(
                "SELECT d.id,d.name,c.ordinal,c.page,c.start_line,c.end_line,k.text,0.0,d.revision
             FROM knowledge_chunks c JOIN knowledge_documents d ON d.id=c.document_id
             JOIN knowledge_fts k ON k.rowid=c.id WHERE c.document_id=?1 AND c.ordinal=?2",
            )
            .map_err(|_| storage_error())?;
        let mut hits = Vec::new();
        let mut overview = Vec::new();
        for &(id, total, slots) in &documents {
            for index in 0..slots {
                // Evenly spaced ordinals, always starting with the first passage.
                let ordinal = (u64::from(index) * u64::from(total) / u64::from(slots)) as u32;
                hits.push(
                    hydrate
                        .query_row(params![id, ordinal], crate::knowledge::read_hit)
                        .map_err(|_| storage_error())?,
                );
            }
            overview.push(OverviewCoverage {
                document_id: id,
                supplied_chunks: slots,
                total_chunks: total,
            });
        }
        let mut coverage = self.plain_coverage(ids, mode, &hits)?;
        coverage.overview = overview;
        Ok(ChatRetrieval {
            coverage,
            sources: hits,
        })
    }

    fn phrase_hits(&self, phrase: &str, ids: &[i64]) -> AgentResult<Vec<KnowledgeHit>> {
        let request = KnowledgeSearch {
            query: phrase.to_owned(),
            document_ids: Some(ids.to_vec()),
            limit: MAX_CHAT_SOURCES as u32,
        };
        self.search_phrase(&request)
    }

    pub fn chat_source_references(&self, message: MessageId) -> Result<Vec<SourceCitation>> {
        self.source_rows(message, true)
    }

    pub fn chat_supplied_evidence(&self, message: MessageId) -> Result<Vec<SourceCitation>> {
        self.source_rows(message, false)
    }

    fn source_rows(&self, message: MessageId, cited_only: bool) -> Result<Vec<SourceCitation>> {
        let valid = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND role='assistant')",
                [message],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if !valid {
            return Err(not_found());
        }
        let mut stmt = self
            .connection
            .prepare(
                "SELECT s.slot,s.document_id,s.revision,s.chunk_ordinal,s.document_name,s.source,
                    EXISTS(SELECT 1 FROM knowledge_documents d JOIN knowledge_chunks c
                           ON c.document_id=d.id JOIN knowledge_fts k ON k.rowid=c.id
                           WHERE d.id=s.document_id AND d.revision=s.revision
                           AND c.ordinal=s.chunk_ordinal),s.cited
             FROM chat_source_references s WHERE s.message_id=?1 AND (?2=0 OR s.cited=1) ORDER BY s.slot",
            )
            .map_err(sql_error)?;
        stmt.query_map(params![message, cited_only], |row| {
            Ok(SourceCitation {
                citation_id: citation_id(row.get::<_, u32>(0)? as usize),
                document_id: row.get(1)?,
                revision: row.get(2)?,
                chunk_ordinal: row.get(3)?,
                document_name: row.get(4)?,
                source: json_column(row, 5)?,
                available: row.get(6)?,
                cited: row.get(7)?,
            })
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)
    }

    pub fn resolve_chat_source(
        &self,
        message: MessageId,
        citation: &str,
    ) -> Result<Option<String>> {
        self.resolve_evidence(message, citation, true)
    }

    pub fn resolve_chat_evidence(
        &self,
        message: MessageId,
        citation: &str,
    ) -> Result<Option<String>> {
        self.resolve_evidence(message, citation, false)
    }

    fn resolve_evidence(
        &self,
        message: MessageId,
        citation: &str,
        cited_only: bool,
    ) -> Result<Option<String>> {
        let sources = self.source_rows(message, cited_only)?;
        let Some(source) = sources
            .into_iter()
            .find(|source| source.available && source.citation_id == citation)
        else {
            return Ok(None);
        };
        self.connection.query_row(
            "SELECT k.text FROM knowledge_documents d JOIN knowledge_chunks c ON c.document_id=d.id
             JOIN knowledge_fts k ON k.rowid=c.id WHERE d.id=?1 AND d.revision=?2 AND c.ordinal=?3",
            params![source.document_id, source.revision, source.chunk_ordinal],
            |row| row.get(0),
        ).optional().map_err(sql_error)
    }
}

impl SqliteStore {
    /// The previous user question when an answer to it used selected
    /// documents. Only the turn immediately before the current one counts.
    /// `regenerating` means the latest user message is the current question.
    pub fn previous_document_question(
        &self,
        conversation: agent_core::chat::ConversationId,
        regenerating: bool,
    ) -> Result<Option<String>> {
        // Walk back over consecutive document turns, skipping revision
        // requests ("make it shorter") to reach the question they revise.
        for offset in i64::from(regenerating)..i64::from(regenerating) + 8 {
            let previous: Option<(i64, String)> = self
                .connection
                .query_row(
                    "SELECT position, content FROM messages WHERE conversation_id=?1 AND role='user'
                     ORDER BY position DESC LIMIT 1 OFFSET ?2",
                    params![conversation, offset],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            let Some((position, content)) = previous else {
                return Ok(None);
            };
            if !self.answered_from_documents(conversation, position)? {
                return Ok(None);
            }
            if !agent_core::chat_route::is_refinement(&content) {
                return Ok(Some(content));
            }
        }
        Ok(None)
    }

    fn answered_from_documents(
        &self,
        conversation: agent_core::chat::ConversationId,
        position: i64,
    ) -> Result<bool> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT x.metadata FROM messages m JOIN chat_context_attempts x ON x.message_id=m.id
                 WHERE m.conversation_id=?1 AND m.role='assistant' AND m.position>?2
                 AND NOT EXISTS(SELECT 1 FROM messages u WHERE u.conversation_id=?1
                     AND u.role='user' AND u.position>?2 AND u.position<m.position)",
            )
            .map_err(sql_error)?;
        let documented = stmt
            .query_map(params![conversation, position], |row| {
                row.get::<_, Option<String>>(0)
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
            .into_iter()
            .flatten()
            .filter_map(|metadata| serde_json::from_str::<AttemptContext>(&metadata).ok())
            .any(|context| {
                context.coverage.is_some_and(|coverage| {
                    coverage.intent != Some(agent_core::chat_context::ChatIntent::General)
                })
            });
        Ok(documented)
    }

    pub fn chat_attempt_context(&self, message: MessageId) -> Result<Option<AttemptContext>> {
        let value: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT metadata FROM chat_context_attempts WHERE message_id=?1",
                [message],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        value
            .map(|value| {
                value
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map(|metadata| metadata.unwrap_or_default())
                    .map_err(|_| storage_error())
            })
            .transpose()
    }

    pub fn checkpoint_chat_evidence(
        &mut self,
        message: MessageId,
        context: &AttemptContext,
        usage: ContextUsage,
        candidates: &[KnowledgeHit],
    ) -> Result<()> {
        let mut context = context.clone();
        context.fitted = Some(usage);
        let tx = self.connection.transaction().map_err(sql_error)?;
        save_context(&tx, message, &context)?;
        record_evidence(&tx, message, candidates, usage.sources)?;
        tx.commit().map_err(sql_error)
    }
}

pub(super) fn save_context(
    connection: &Connection,
    message: MessageId,
    context: &AttemptContext,
) -> Result<()> {
    let metadata = serde_json::to_string(context).map_err(|_| storage_error())?;
    connection
        .execute(
            "INSERT INTO chat_context_attempts (message_id,metadata) VALUES (?1,?2)
         ON CONFLICT(message_id) DO UPDATE SET metadata=excluded.metadata",
            params![message, metadata],
        )
        .map_err(sql_error)?;
    Ok(())
}

pub(super) fn is_contextual(connection: &Connection, message: MessageId) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_context_attempts WHERE message_id=?1)",
            [message],
            |row| row.get(0),
        )
        .map_err(sql_error)
}

pub(super) fn exclude_contextual_answers(
    connection: &Connection,
    conversation: &mut Conversation,
) -> Result<()> {
    let mut stmt = connection
        .prepare(
            "SELECT x.message_id FROM chat_context_attempts x JOIN messages m ON m.id=x.message_id
         WHERE m.conversation_id=?1",
        )
        .map_err(sql_error)?;
    let excluded: std::collections::HashSet<MessageId> = stmt
        .query_map([conversation.id], |row| row.get(0))
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)?;
    for message in &mut conversation.messages {
        if excluded.contains(&message.id) {
            // Keep the last-attempt semantics, including when an earlier
            // attempt in the same turn was ordinary Chat.
            message.state = GenerationState::Interrupted;
        }
    }
    Ok(())
}

pub(super) fn record_evidence(
    connection: &Connection,
    message: MessageId,
    candidates: &[KnowledgeHit],
    supplied: usize,
) -> Result<()> {
    for (slot, hit) in candidates
        .iter()
        .take(supplied.min(MAX_CHAT_SOURCES))
        .enumerate()
    {
        let source = serde_json::to_string(&hit.source).map_err(|_| storage_error())?;
        connection
            .execute(
                "INSERT INTO chat_source_references
                (message_id,slot,document_id,revision,chunk_ordinal,document_name,source,cited)
             VALUES (?1,?2,?3,?4,?5,?6,?7,0) ON CONFLICT(message_id,slot) DO NOTHING",
                params![
                    message,
                    slot as u32,
                    hit.document_id,
                    hit.revision,
                    hit.ordinal,
                    hit.document_name,
                    source
                ],
            )
            .map_err(sql_error)?;
    }
    Ok(())
}

pub(super) fn mark_citations(
    connection: &Connection,
    message: MessageId,
    answer: &str,
) -> Result<()> {
    let count: u32 = connection
        .query_row(
            "SELECT count(*) FROM chat_source_references WHERE message_id=?1",
            [message],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if count == 0 {
        return Ok(());
    }
    for (slot, cited) in cited_slots(answer, count as usize)
        .into_iter()
        .enumerate()
        .take(count as usize)
    {
        connection
            .execute(
                "UPDATE chat_source_references SET cited=?3 WHERE message_id=?1 AND slot=?2",
                params![message, slot as u32, cited],
            )
            .map_err(sql_error)?;
    }
    Ok(())
}

pub(super) fn recover_citations(connection: &Connection) -> Result<()> {
    let mut stmt = connection.prepare(
        "SELECT m.id,m.content FROM messages m JOIN chat_context_attempts x ON x.message_id=m.id
         WHERE m.state='streaming' AND EXISTS(SELECT 1 FROM chat_source_references s WHERE s.message_id=m.id)",
    ).map_err(sql_error)?;
    let interrupted = stmt
        .query_map([], |row| {
            Ok((row.get::<_, MessageId>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    for (message, answer) in interrupted {
        mark_citations(connection, message, &answer)?;
    }
    Ok(())
}
