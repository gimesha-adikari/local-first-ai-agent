//! Explicit, app-wide personal memories. Chat history is never a write source.

use agent_core::classification::{Labels, MEMORY};
use agent_core::intelligence::Receipt;
use agent_core::{
    chat_context::{MAX_CHAT_MEMORIES, MAX_MEMORY_BYTES, search_terms},
    inference::{ErrorCode, InferenceError, Result},
    knowledge::MAX_DOCUMENT_ID,
};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::{SqliteStore, sql_error, unix_ms};

const MAX_MEMORIES: u32 = 512;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalMemory {
    pub id: i64,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
    #[serde(skip)]
    classification: Option<Labels>,
}

impl PersonalMemory {
    pub fn labels(&self) -> agent_core::classification::Labels {
        Labels::derived(self.classification.clone(), MEMORY)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryPage {
    pub items: Vec<PersonalMemory>,
    pub total: u32,
    pub has_more: bool,
}

impl SqliteStore {
    pub fn memory_enabled(&self) -> Result<bool> {
        self.connection
            .query_row("SELECT enabled FROM memory_settings WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(sql_error)
    }

    pub fn set_memory_enabled(&mut self, enabled: bool) -> Result<()> {
        self.connection
            .execute(
                "UPDATE memory_settings SET enabled=?1 WHERE id=1",
                [enabled],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn create_memory(&mut self, content: &str) -> Result<PersonalMemory> {
        self.insert_memory(content, None, None)
    }

    pub fn remember_glance(&mut self, content: &str, receipt: &Receipt) -> Result<PersonalMemory> {
        let provenance = serde_json::json!({
            "source": "contextual_panel",
            "authorization": "explicit_user_remember",
            "scope": "app",
            "provider": receipt.provider,
            "used": receipt.used,
        });
        self.insert_memory(
            content,
            Some(provenance.to_string()),
            Some(&receipt.output_labels),
        )
    }

    fn insert_memory(
        &mut self,
        content: &str,
        provenance: Option<String>,
        labels: Option<&Labels>,
    ) -> Result<PersonalMemory> {
        let classification = labels
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| crate::storage_error())?;
        let content = validate_content(content)?;
        let now = unix_ms()?;
        self.connection
            .query_row(
                "INSERT INTO personal_memories(content,created_at,updated_at,provenance,classification)
             SELECT ?1,?2,?2,?4,?5 WHERE (SELECT count(*) FROM personal_memories) < ?3
             RETURNING id,content,created_at,updated_at,provenance,classification",
                params![content, now, MAX_MEMORIES, provenance, classification],
                read_memory,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::MemoryLimit,
                    "At most 512 personal memories can be saved.",
                )
            })
    }

    pub fn edit_memory(&mut self, id: i64, content: &str) -> Result<PersonalMemory> {
        validate_id(id)?;
        let content = validate_content(content)?;
        self.connection
            .query_row(
                "UPDATE personal_memories SET content=?1,updated_at=max(updated_at,?2)
             WHERE id=?3 RETURNING id,content,created_at,updated_at,provenance,classification",
                params![content, unix_ms()?, id],
                read_memory,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(missing_memory)
    }

    pub fn delete_memory(&mut self, id: i64) -> Result<()> {
        validate_id(id)?;
        if self
            .connection
            .execute("DELETE FROM personal_memories WHERE id=?1", [id])
            .map_err(sql_error)?
            == 0
        {
            return Err(missing_memory());
        }
        Ok(())
    }

    pub fn clear_memories(&mut self) -> Result<usize> {
        self.connection
            .execute("DELETE FROM personal_memories", [])
            .map_err(sql_error)
    }

    pub fn list_memory_page(
        &self,
        query: &str,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<MemoryPage> {
        let mut items = self.search_memories(query, before_id, limit, true)?;
        let has_more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let total = self
            .connection
            .query_row("SELECT count(*) FROM personal_memories", [], |row| {
                row.get(0)
            })
            .map_err(sql_error)?;
        Ok(MemoryPage {
            items,
            total,
            has_more,
        })
    }

    pub fn list_memories(
        &self,
        query: &str,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<PersonalMemory>> {
        self.search_memories(query, before_id, limit, false)
    }

    fn search_memories(
        &self,
        query: &str,
        before_id: Option<i64>,
        limit: u32,
        lookahead: bool,
    ) -> Result<Vec<PersonalMemory>> {
        if query.len() > 256 || query.contains('\0') || !(1..=100).contains(&limit) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Use a query of at most 256 bytes and a limit of 1–100.",
            ));
        }
        if let Some(id) = before_id {
            validate_id(id)?;
        }
        let query = query.trim().to_lowercase();
        let limit = limit + u32::from(lookahead);
        let rows = self.memory_page(
            before_id,
            if query.is_empty() {
                limit
            } else {
                MAX_MEMORIES
            },
        )?;
        Ok(rows
            .into_iter()
            .filter(|memory| memory.content.to_lowercase().contains(&query))
            .take(limit as usize)
            .collect())
    }

    pub fn relevant_memories(&self, question: &str) -> Result<Vec<String>> {
        Ok(self
            .relevant_memory_records(question)?
            .into_iter()
            .map(|memory| memory.content)
            .collect())
    }

    pub fn relevant_memory_records(&self, question: &str) -> Result<Vec<PersonalMemory>> {
        if !self.memory_enabled()? {
            return Ok(Vec::new());
        }
        let query = search_terms(question, 48);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut ranked = self
            .memory_page(None, MAX_MEMORIES)?
            .into_iter()
            .filter_map(|memory| {
                let terms = search_terms(&memory.content, MAX_MEMORY_BYTES);
                let score = query.iter().filter(|term| terms.contains(term)).count();
                (score > 0).then_some((score, memory))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, memory)| {
            std::cmp::Reverse((*score, memory.updated_at, memory.id))
        });
        Ok(ranked
            .into_iter()
            .take(MAX_CHAT_MEMORIES)
            .map(|(_, memory)| memory)
            .collect())
    }

    fn memory_page(&self, before_id: Option<i64>, limit: u32) -> Result<Vec<PersonalMemory>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id,content,created_at,updated_at,provenance,classification FROM personal_memories
             WHERE id < coalesce(?1,9223372036854775807) ORDER BY id DESC LIMIT ?2",
            )
            .map_err(sql_error)?;
        stmt.query_map(params![before_id, limit], read_memory)
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }
}

fn read_memory(row: &Row<'_>) -> rusqlite::Result<PersonalMemory> {
    Ok(PersonalMemory {
        id: row.get(0)?,
        content: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        provenance: decode_memory_metadata(row, 4)?,
        classification: decode_memory_metadata(row, 5)?,
    })
}

fn decode_memory_metadata<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    col: usize,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(col)?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    col,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn validate_content(content: &str) -> Result<&str> {
    let content = content.trim();
    if content.is_empty() || content.len() > MAX_MEMORY_BYTES || content.contains('\0') {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Enter a nonempty memory of at most 1024 UTF-8 bytes without NUL characters.",
        ));
    }
    Ok(content)
}

fn validate_id(id: i64) -> Result<()> {
    if !(1..=MAX_DOCUMENT_ID).contains(&id) {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Enter a valid personal memory ID.",
        ));
    }
    Ok(())
}

fn missing_memory() -> InferenceError {
    InferenceError::new(ErrorCode::NotFound, "The personal memory was not found.")
}
