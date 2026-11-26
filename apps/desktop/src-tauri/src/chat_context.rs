//! Main-window contracts for contextual Chat and explicitly saved memory.

use agent_core::{
    agent::{AgentError, AgentErrorCode},
    chat::MessageId,
    chat_context::Grounding,
    inference::{ErrorCode, InferenceError, Result},
    knowledge::{MAX_DOCUMENT_ID, MAX_DOCUMENTS},
};
use serde::Deserialize;
use storage::{chat_context::SourceCitation, memory::PersonalMemory, semantic::RetrievalMode};
use tauri::State;

use crate::commands::{Desktop, database};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KnowledgeSelection {
    pub mode: RetrievalMode,
    pub document_ids: Vec<i64>,
    /// Omitted means automatic: documents are available without blocking
    /// unrelated requests.
    #[serde(default)]
    pub grounding: Grounding,
}

impl KnowledgeSelection {
    pub fn validate(&self) -> Result<()> {
        if self.document_ids.is_empty()
            || self.document_ids.len() > MAX_DOCUMENTS
            || self
                .document_ids
                .iter()
                .any(|id| !(1..=MAX_DOCUMENT_ID).contains(id))
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Select 1–128 valid knowledge document IDs.",
            ));
        }
        Ok(())
    }
}

pub(crate) fn retrieval_query(question: &str) -> Option<String> {
    let mut query = String::new();
    for term in question
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(16)
    {
        let remaining = 256 - query.len() - usize::from(!query.is_empty());
        let boundary = term.floor_char_boundary(remaining);
        if boundary == 0 {
            break;
        }
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(&term[..boundary]);
        if boundary < term.len() || query.len() >= 255 {
            break;
        }
    }
    query.chars().any(char::is_alphanumeric).then_some(query)
}

pub(crate) fn retrieval_error(error: AgentError) -> InferenceError {
    let code = match error.code {
        AgentErrorCode::EmbeddingUnavailable => ErrorCode::EmbeddingUnavailable,
        AgentErrorCode::EmbeddingIncompatible => ErrorCode::EmbeddingIncompatible,
        AgentErrorCode::EmbeddingIndexUnavailable => ErrorCode::EmbeddingIndexUnavailable,
        AgentErrorCode::InvalidArguments => ErrorCode::InvalidConfig,
        AgentErrorCode::Cancelled => ErrorCode::Cancelled,
        AgentErrorCode::TimedOut => ErrorCode::TimedOut,
        AgentErrorCode::StorageFailed => ErrorCode::Storage,
        _ => ErrorCode::GenerationFailed,
    };
    InferenceError::new(code, error.message)
}

#[tauri::command]
pub fn chat_source_references(
    state: State<'_, Desktop>,
    message_id: MessageId,
) -> Result<Vec<SourceCitation>> {
    validate_message_id(message_id)?;
    database(&state.database)?.chat_source_references(message_id)
}

#[tauri::command]
pub fn chat_resolve_source(
    state: State<'_, Desktop>,
    message_id: MessageId,
    citation_id: &str,
) -> Result<Option<String>> {
    validate_message_id(message_id)?;
    if !matches!(citation_id, "S1" | "S2" | "S3" | "S4") {
        return Ok(None);
    }
    database(&state.database)?.resolve_chat_source(message_id, citation_id)
}

#[tauri::command]
pub fn chat_supplied_evidence(
    state: State<'_, Desktop>,
    message_id: MessageId,
) -> Result<Vec<SourceCitation>> {
    validate_message_id(message_id)?;
    database(&state.database)?.chat_supplied_evidence(message_id)
}

#[tauri::command]
pub fn chat_resolve_evidence(
    state: State<'_, Desktop>,
    message_id: MessageId,
    citation_id: &str,
) -> Result<Option<String>> {
    validate_message_id(message_id)?;
    if !matches!(citation_id, "S1" | "S2" | "S3" | "S4") {
        return Ok(None);
    }
    database(&state.database)?.resolve_chat_evidence(message_id, citation_id)
}

#[tauri::command]
pub fn memory_page(
    state: State<'_, Desktop>,
    query: Option<&str>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<storage::memory::MemoryPage> {
    let db = database(&state.database)?;
    db.list_memory_page(query.unwrap_or_default(), before_id, limit)
}

fn validate_message_id(id: MessageId) -> Result<()> {
    if !(1..=MAX_DOCUMENT_ID).contains(&id) {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Enter a valid assistant message ID.",
        ));
    }
    Ok(())
}

#[tauri::command]
pub fn memory_retrieval_enabled(state: State<'_, Desktop>) -> Result<bool> {
    database(&state.database)?.memory_enabled()
}

#[tauri::command]
pub fn memory_set_retrieval_enabled(state: State<'_, Desktop>, enabled: bool) -> Result<()> {
    database(&state.database)?.set_memory_enabled(enabled)
}

#[tauri::command]
pub fn memory_create(state: State<'_, Desktop>, content: &str) -> Result<PersonalMemory> {
    database(&state.database)?.create_memory(content)
}

#[tauri::command]
pub fn memory_list(
    state: State<'_, Desktop>,
    query: Option<&str>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<Vec<PersonalMemory>> {
    database(&state.database)?.list_memories(query.unwrap_or_default(), before_id, limit)
}

#[tauri::command]
pub fn memory_edit(state: State<'_, Desktop>, id: i64, content: &str) -> Result<PersonalMemory> {
    database(&state.database)?.edit_memory(id, content)
}

#[tauri::command]
pub fn memory_delete(state: State<'_, Desktop>, id: i64) -> Result<()> {
    database(&state.database)?.delete_memory(id)
}

#[tauri::command]
pub fn memory_clear(state: State<'_, Desktop>) -> Result<usize> {
    database(&state.database)?.clear_memories()
}
