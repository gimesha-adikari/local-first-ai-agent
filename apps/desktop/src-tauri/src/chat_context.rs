//! Main-window contracts for contextual Chat and explicitly saved memory.

use agent_core::{
    chat::MessageId,
    inference::{ErrorCode, InferenceError, Result},
    knowledge::MAX_DOCUMENT_ID,
};
use engine::database;
use storage::{chat_context::SourceCitation, memory::PersonalMemory};
use tauri::State;

use crate::commands::Desktop;

#[tauri::command]
pub fn chat_source_references(
    state: State<'_, Desktop>,
    message_id: MessageId,
) -> Result<Vec<SourceCitation>> {
    validate_message_id(message_id)?;
    database(state.engine.store())?.chat_source_references(message_id)
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
    database(state.engine.store())?.resolve_chat_source(message_id, citation_id)
}

#[tauri::command]
pub fn chat_supplied_evidence(
    state: State<'_, Desktop>,
    message_id: MessageId,
) -> Result<Vec<SourceCitation>> {
    validate_message_id(message_id)?;
    database(state.engine.store())?.chat_supplied_evidence(message_id)
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
    database(state.engine.store())?.resolve_chat_evidence(message_id, citation_id)
}

#[tauri::command]
pub fn memory_page(
    state: State<'_, Desktop>,
    query: Option<&str>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<storage::memory::MemoryPage> {
    let db = database(state.engine.store())?;
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
    database(state.engine.store())?.memory_enabled()
}

#[tauri::command]
pub fn memory_set_retrieval_enabled(state: State<'_, Desktop>, enabled: bool) -> Result<()> {
    database(state.engine.store())?.set_memory_enabled(enabled)
}

#[tauri::command]
pub fn memory_create(state: State<'_, Desktop>, content: &str) -> Result<PersonalMemory> {
    database(state.engine.store())?.create_memory(content)
}

#[tauri::command]
pub fn memory_list(
    state: State<'_, Desktop>,
    query: Option<&str>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<Vec<PersonalMemory>> {
    database(state.engine.store())?.list_memories(query.unwrap_or_default(), before_id, limit)
}

#[tauri::command]
pub fn memory_edit(state: State<'_, Desktop>, id: i64, content: &str) -> Result<PersonalMemory> {
    database(state.engine.store())?.edit_memory(id, content)
}

#[tauri::command]
pub fn memory_delete(state: State<'_, Desktop>, id: i64) -> Result<()> {
    database(state.engine.store())?.delete_memory(id)
}

#[tauri::command]
pub fn memory_clear(state: State<'_, Desktop>) -> Result<usize> {
    database(state.engine.store())?.clear_memories()
}
