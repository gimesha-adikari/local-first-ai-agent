//! Persistent document vector generations and bounded exact retrieval.

pub use agent_core::chat_context::{IndexCoverage, RetrievalMode};
use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    classification::{EMBEDDING, Labels, QUERY, USER_INPUT},
    knowledge::{SourceReference, knowledge_limit},
    semantic::{
        ChunkKey, EmbeddingKey, EmbeddingWindow, MAX_CHUNK_WINDOWS, MAX_DOCUMENT_WINDOWS,
        MAX_LIBRARY_WINDOWS, RETRIEVAL_CANDIDATES, UnitVector, WINDOW_OVERLAP, WINDOW_TOKENS,
        above_floor, incompatible, reciprocal_rank_fusion, relevant_candidates,
    },
};
use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};
use serde::Serialize;

use crate::{
    SqliteStore,
    knowledge::{
        KnowledgeHit, KnowledgeSearch, read_hit, storage_error, validate_document_filter,
        validate_document_id, validate_search,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingIndexState {
    Missing,
    Ready,
    Incompatible,
}

pub(crate) const INDEX_STATE: &str = "CASE WHEN i.document_id IS NULL THEN 0
    WHEN i.model_fingerprint=?1 AND i.preprocessing=?2 THEN 1 ELSE 2 END";
pub(crate) const INDEX_JOIN: &str = "LEFT JOIN knowledge_embedding_indexes i
    ON i.document_id=d.id AND i.revision=d.revision";

pub(crate) fn read_index_state(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<EmbeddingIndexState> {
    match row.get::<_, u32>(column)? {
        0 => Ok(EmbeddingIndexState::Missing),
        1 => Ok(EmbeddingIndexState::Ready),
        2 => Ok(EmbeddingIndexState::Incompatible),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalResults {
    pub mode: RetrievalMode,
    pub coverage: Option<IndexCoverage>,
    pub hits: Vec<KnowledgeHit>,
}

impl RetrievalResults {
    pub fn labels(&self) -> Labels {
        Labels::derived(
            std::iter::once(Labels::from_source(USER_INPUT))
                .chain(self.hits.iter().map(KnowledgeHit::labels)),
            QUERY,
        )
    }
}

#[derive(Debug)]
pub struct IndexSnapshot {
    document_id: i64,
    revision: i64,
    pub chunks: Vec<IndexChunk>,
}

#[derive(Debug)]
pub struct IndexChunk {
    row_id: i64,
    pub text: String,
    source: SourceReference,
}

impl IndexChunk {
    pub fn labels(&self) -> Labels {
        Labels::derived([self.source.labels()], EMBEDDING)
    }
}

impl SqliteStore {
    pub fn embedding_snapshot(&self, id: i64) -> AgentResult<IndexSnapshot> {
        validate_document_id(id)?;
        let revision = revision(&self.connection, id)?.ok_or_else(|| {
            AgentError::new(
                AgentErrorCode::KnowledgeNotFound,
                "The knowledge document was not found.",
            )
        })?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT c.id, k.text, c.page, c.start_line, c.end_line FROM knowledge_chunks c
             JOIN knowledge_fts k ON k.rowid=c.id WHERE c.document_id=?1 ORDER BY c.ordinal",
            )
            .map_err(sql_error)?;
        let chunks = statement
            .query_map([id], |row| {
                Ok(IndexChunk {
                    row_id: row.get(0)?,
                    text: row.get(1)?,
                    source: match row.get(2)? {
                        Some(page) => SourceReference::Page { page },
                        None => SourceReference::Lines {
                            start_line: row.get(3)?,
                            end_line: row.get(4)?,
                        },
                    },
                })
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        Ok(IndexSnapshot {
            document_id: id,
            revision,
            chunks,
        })
    }

    /// All join keys, counts, and vectors become visible at the same commit.
    pub fn install_embeddings(
        &mut self,
        snapshot: &IndexSnapshot,
        key: EmbeddingKey,
        embeddings: &[Vec<EmbeddingWindow>],
        check: impl Fn() -> AgentResult<()>,
        begin_commit: impl FnOnce() -> AgentResult<()>,
    ) -> AgentResult<usize> {
        check()?;
        let id = snapshot.document_id;
        let tx = self.connection.transaction().map_err(sql_error)?;
        if revision(&tx, id)? != Some(snapshot.revision) {
            return Err(AgentError::new(
                AgentErrorCode::KnowledgeRevisionChanged,
                "The knowledge document changed during embedding. Start indexing again.",
            ));
        }
        let count: u32 = tx
            .query_row(
                "SELECT chunk_count FROM knowledge_documents WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if embeddings.is_empty()
            || embeddings.len() != snapshot.chunks.len()
            || embeddings.len() != count as usize
        {
            return Err(incompatible());
        }
        let windows = embeddings.iter().map(Vec::len).sum::<usize>();
        let used: u32 = tx.query_row(
            "SELECT coalesce(sum(window_count), 0) FROM knowledge_embedding_indexes WHERE document_id<>?1",
            [id], |row| row.get(0),
        ).map_err(sql_error)?;
        if windows > MAX_DOCUMENT_WINDOWS || used as usize + windows > MAX_LIBRARY_WINDOWS {
            return Err(knowledge_limit());
        }
        tx.execute(
            "DELETE FROM knowledge_embedding_indexes WHERE document_id=?1",
            [id],
        )
        .map_err(sql_error)?;
        tx.execute(
            "INSERT INTO knowledge_embedding_indexes
             (document_id, revision, model_fingerprint, preprocessing, window_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                snapshot.revision,
                key.fingerprint.as_slice(),
                key.preprocessing,
                windows as u32
            ],
        )
        .map_err(sql_error)?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO knowledge_embedding_windows (document_id,chunk_id,token_start,token_end,vector)
                 VALUES (?1,?2,?3,?4,?5)",
            ).map_err(sql_error)?;
            for (chunk, windows) in snapshot.chunks.iter().zip(embeddings) {
                check()?;
                validate_windows(windows)?;
                for window in windows {
                    insert
                        .execute(params![
                            id,
                            chunk.row_id,
                            window.start,
                            window.end,
                            window.vector.to_bytes().as_slice(),
                        ])
                        .map_err(sql_error)?;
                }
            }
        }
        begin_commit()?;
        tx.commit().map_err(sql_error)?;
        Ok(windows)
    }

    pub fn embedding_coverage(
        &self,
        key: EmbeddingKey,
        ids: Option<&[i64]>,
    ) -> AgentResult<IndexCoverage> {
        validate_document_filter(ids)?;
        let mut coverage = IndexCoverage::default();
        for (id, state) in self.embedding_states(key)? {
            if ids.is_none_or(|selected| selected.contains(&id)) {
                match state {
                    EmbeddingIndexState::Missing => coverage.missing_documents += 1,
                    EmbeddingIndexState::Ready => coverage.indexed_documents += 1,
                    EmbeddingIndexState::Incompatible => coverage.incompatible_documents += 1,
                }
            }
        }
        Ok(coverage)
    }

    pub(crate) fn embedding_states(
        &self,
        key: EmbeddingKey,
    ) -> AgentResult<Vec<(i64, EmbeddingIndexState)>> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT d.id, {INDEX_STATE} FROM knowledge_documents d {INDEX_JOIN} ORDER BY d.id"
            ))
            .map_err(sql_error)?;
        statement
            .query_map(
                params![key.fingerprint.as_slice(), key.preprocessing],
                |row| Ok((row.get(0)?, read_index_state(row, 1)?)),
            )
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }

    pub fn retrieve_knowledge(
        &self,
        request: &KnowledgeSearch,
        mode: RetrievalMode,
        key: EmbeddingKey,
        vector: Option<&UnitVector>,
    ) -> AgentResult<RetrievalResults> {
        self.retrieve_ranked(request, mode, key, vector, false)
    }

    pub(crate) fn retrieve_ranked(
        &self,
        request: &KnowledgeSearch,
        mode: RetrievalMode,
        key: EmbeddingKey,
        vector: Option<&UnitVector>,
        chat_query: bool,
    ) -> AgentResult<RetrievalResults> {
        if mode == RetrievalMode::Keyword {
            return Ok(RetrievalResults {
                mode,
                coverage: None,
                hits: self.search_knowledge_candidates(
                    request,
                    request.limit as usize,
                    chat_query,
                )?,
            });
        }
        validate_search(request)?;
        let vector = vector.ok_or_else(|| {
            AgentError::new(
                AgentErrorCode::EmbeddingUnavailable,
                "Load the supported local embedding model first.",
            )
        })?;
        let ids = request.document_ids.as_deref();
        let coverage = self.embedding_coverage(key, ids)?;
        if coverage.indexed_documents == 0 {
            if coverage.missing_documents > 0 || coverage.incompatible_documents > 0 {
                return Err(AgentError::new(
                    AgentErrorCode::EmbeddingIndexUnavailable,
                    "The selected knowledge has no compatible embedding index. Index the documents first.",
                ));
            }
            return Ok(RetrievalResults {
                mode,
                coverage: Some(coverage),
                hits: Vec::new(),
            });
        }

        // Dense similarity always ranks every indexed passage. Only passages
        // that pass the calibrated relevance filter count as hits.
        let candidates = self.semantic_candidates(key, ids, vector)?;
        let ranking = if mode == RetrievalMode::Hybrid {
            let semantic = above_floor(candidates);
            let lexical =
                self.search_knowledge_candidates(request, RETRIEVAL_CANDIDATES, chat_query)?;
            reciprocal_rank_fusion(
                lexical.iter().map(|hit| (hit.document_id, hit.ordinal)),
                semantic.into_iter().map(|(key, _)| key),
            )
        } else {
            relevant_candidates(candidates)
        };
        let mut hydrate = self
            .connection
            .prepare(
                "SELECT d.id,d.name,c.ordinal,c.page,c.start_line,c.end_line,k.text,?3,d.revision
             FROM knowledge_chunks c JOIN knowledge_documents d ON d.id=c.document_id
             JOIN knowledge_fts k ON k.rowid=c.id WHERE c.document_id=?1 AND c.ordinal=?2",
            )
            .map_err(sql_error)?;
        let hits = ranking
            .into_iter()
            .take(request.limit as usize)
            .map(|((id, ordinal), score)| {
                hydrate
                    .query_row(params![id, ordinal, score], read_hit)
                    .map_err(sql_error)
            })
            .collect::<AgentResult<_>>()?;
        Ok(RetrievalResults {
            mode,
            coverage: Some(coverage),
            hits,
        })
    }

    fn semantic_candidates(
        &self,
        key: EmbeddingKey,
        ids: Option<&[i64]>,
        query: &UnitVector,
    ) -> AgentResult<Vec<(ChunkKey, f64)>> {
        let mut sql = String::from(
            "SELECT i.document_id,c.ordinal,w.vector
             FROM knowledge_embedding_windows w
             CROSS JOIN knowledge_embedding_indexes i ON i.document_id=w.document_id
             CROSS JOIN knowledge_documents d ON d.id=i.document_id AND d.revision=i.revision
             CROSS JOIN knowledge_chunks c ON c.id=w.chunk_id AND c.document_id=i.document_id
             WHERE i.model_fingerprint=? AND i.preprocessing=?",
        );
        let mut bindings = vec![
            Value::Blob(key.fingerprint.to_vec()),
            Value::Integer(i64::from(key.preprocessing)),
        ];
        if let Some(ids) = ids {
            sql.push_str(" AND w.document_id IN (");
            sql.push_str(&vec!["?"; ids.len()].join(","));
            sql.push(')');
            bindings.extend(ids.iter().copied().map(Value::Integer));
        }
        sql.push_str(" ORDER BY w.document_id,w.chunk_id,w.token_start");
        let mut stmt = self.connection.prepare(&sql).map_err(sql_error)?;
        let mut rows = stmt.query(params_from_iter(bindings)).map_err(sql_error)?;
        let mut candidates = Vec::with_capacity(RETRIEVAL_CANDIDATES);
        let mut current: Option<(ChunkKey, f64)> = None;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let id = row.get(0).map_err(sql_error)?;
            let chunk = (id, row.get(1).map_err(sql_error)?);
            let bytes = row
                .get_ref(2)
                .map_err(sql_error)?
                .as_blob()
                .map_err(|_| incompatible())?;
            let similarity = query.cosine(&UnitVector::from_bytes(bytes)?);
            if let Some((current_chunk, best)) = current.as_mut()
                && *current_chunk == chunk
            {
                *best = best.max(similarity);
                continue;
            }
            if let Some(previous) = current.replace((chunk, similarity)) {
                offer(&mut candidates, previous);
            }
        }
        if let Some(last) = current {
            offer(&mut candidates, last);
        }
        Ok(candidates)
    }
}

fn offer(ranked: &mut Vec<(ChunkKey, f64)>, candidate: (ChunkKey, f64)) {
    let index = ranked.partition_point(|(id, score)| {
        candidate
            .1
            .total_cmp(score)
            .then_with(|| id.cmp(&candidate.0))
            .is_lt()
    });
    if index < RETRIEVAL_CANDIDATES {
        ranked.insert(index, candidate);
        ranked.truncate(RETRIEVAL_CANDIDATES);
    }
}

fn validate_windows(windows: &[EmbeddingWindow]) -> AgentResult<()> {
    if windows.is_empty() || windows.len() > MAX_CHUNK_WINDOWS {
        return Err(incompatible());
    }
    let mut expected_start = 0;
    for (index, window) in windows.iter().enumerate() {
        let length = window.end.saturating_sub(window.start) as usize;
        if window.start != expected_start
            || !(1..=WINDOW_TOKENS).contains(&length)
            || (index + 1 < windows.len() && length != WINDOW_TOKENS)
            || (index > 0 && length <= WINDOW_OVERLAP)
        {
            return Err(incompatible());
        }
        expected_start = window.end.saturating_sub(WINDOW_OVERLAP as u32);
    }
    Ok(())
}

fn revision(connection: &rusqlite::Connection, id: i64) -> AgentResult<Option<i64>> {
    connection
        .query_row(
            "SELECT revision FROM knowledge_documents WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)
}

fn sql_error(_: rusqlite::Error) -> AgentError {
    storage_error()
}
