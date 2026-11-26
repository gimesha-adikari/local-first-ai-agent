use agent_core::{
    chat::{Conversation, MessageId, MessageRole},
    classification::{
        AGENT_CONTEXT, DataClasses, HISTORY, LIBRARY, Labels, MEMORY, MODEL_OUTPUT, PDF_EXCERPT,
        TEXT_EXCERPT, USER_INPUT,
    },
    inference::Result,
    intelligence::{ContextSource, Receipt, SourceKind},
};
use rusqlite::{Connection, params};

use crate::{SqliteStore, not_found, sql_error, storage_error, updated};

pub(super) fn prompt_source(conversation: &Conversation) -> Result<ContextSource> {
    let message = conversation
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .ok_or_else(not_found)?;
    Ok(ContextSource {
        kind: SourceKind::Prompt,
        record: Some(message.id),
        labels: message.labels.clone(),
    })
}

pub(super) fn classify_message(
    encoded: Option<&str>,
    role: MessageRole,
    contextual: bool,
    agent: bool,
) -> rusqlite::Result<Labels> {
    if let Some(encoded) = encoded {
        let labels: Labels =
            serde_json::from_str(encoded).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let known = DataClasses::default();
        if labels.is_empty() || labels.ids().any(|id| known.sensitivity(id).is_none()) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        return Ok(labels);
    }

    let mut labels = Labels::from_source(HISTORY);
    match role {
        MessageRole::User => labels.insert(USER_INPUT),
        MessageRole::Assistant => {
            labels.insert(MODEL_OUTPUT);
            labels.insert(USER_INPUT);
            if agent {
                labels.insert(AGENT_CONTEXT);
            }
            if contextual || agent {
                // Earlier attempts didn't always record all consulted sources.
                // Keep both excerpt origins when exact coverage is unavailable.
                for class in [LIBRARY, PDF_EXCERPT, TEXT_EXCERPT] {
                    labels.insert(class);
                }
            }
            if contextual {
                labels.insert(MEMORY);
            }
        }
        MessageRole::System => {}
    }
    Ok(labels)
}

pub(super) fn save_classification(
    connection: &Connection,
    message: MessageId,
    labels: &Labels,
) -> Result<()> {
    let encoded = serde_json::to_string(labels).map_err(|_| storage_error())?;
    updated(connection.execute(
        "UPDATE messages SET classification=?1 WHERE id=?2 AND role='assistant' AND state='streaming'",
        params![encoded, message],
    ))
}

pub(super) fn save_receipt(
    connection: &Connection,
    message: MessageId,
    receipt: &Receipt,
) -> Result<()> {
    let encoded = serde_json::to_string(receipt).map_err(|_| storage_error())?;
    connection
        .execute(
            "INSERT INTO intelligence_receipts (message_id,receipt) VALUES (?1,?2)",
            params![message, encoded],
        )
        .map_err(sql_error)?;
    Ok(())
}

impl SqliteStore {
    /// Metadata only, in invocation order. The message owns these records.
    pub fn intelligence_receipts(&self, message: MessageId) -> Result<Vec<serde_json::Value>> {
        let mut statement = self
            .connection
            .prepare("SELECT receipt FROM intelligence_receipts WHERE message_id=?1 ORDER BY id")
            .map_err(sql_error)?;
        statement
            .query_map([message], |row| crate::json_column(row, 0))
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)
    }
}
