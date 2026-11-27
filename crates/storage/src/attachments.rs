//! Conversation attachments the user explicitly chose to keep (Stage 04-D),
//! and citation marking for attempts that used attachments.
//!
//! Everything lives in the encrypted profile database. Rows belong to one
//! conversation and are deleted with it. Temporary attachments never reach
//! this module.

use agent_core::{
    attachment::{
        AttachmentKind, AttachmentRecord, MAX_ATTACHMENT_SOURCES, MAX_IMAGE_FILE_BYTES,
        MAX_RETAINED_PER_CONVERSATION, MAX_TEXT_FILE_BYTES, PageCoverage, PassageSource,
        valid_attachment_id,
    },
    chat::{ConversationId, MessageId},
    chat_context::{AttemptContext, cited_flags},
    inference::{ErrorCode, InferenceError, Result},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{SqliteStore, not_found, sql_error, storage_error, unix_ms};

/// One stored passage of extracted or recognized text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPassage {
    pub source: PassageSource,
    pub text: String,
}

/// A validated, metadata-stripped image representation.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredImage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for StoredImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredImage")
            .field("bytes", &self.bytes.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// An attachment snapshot kept with its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedAttachment {
    pub public_id: String,
    pub conversation_id: ConversationId,
    pub name: String,
    pub kind: AttachmentKind,
    pub file_bytes: u64,
    pub fingerprint: [u8; 32],
    pub pages: Option<PageCoverage>,
    pub passages: Vec<StoredPassage>,
    pub image: Option<StoredImage>,
    /// Engine and languages of local text recognition, for display.
    pub recognition: Option<String>,
}

fn invalid() -> InferenceError {
    InferenceError::new(
        ErrorCode::InvalidConfig,
        "The attachment snapshot is invalid.",
    )
}

impl RetainedAttachment {
    fn validate(&self) -> Result<()> {
        let text: usize = self.passages.iter().map(|passage| passage.text.len()).sum();
        if !valid_attachment_id(&self.public_id)
            || self.name.trim().is_empty()
            || self.name.len() > 320
            || self.file_bytes == 0
            || self.file_bytes > self.kind.max_file_bytes() as u64
            || text > MAX_TEXT_FILE_BYTES
            || self.image.is_some() != self.kind.is_image()
            || self.image.as_ref().is_some_and(|image| {
                image.bytes.is_empty() || image.bytes.len() > MAX_IMAGE_FILE_BYTES
            })
            || self
                .recognition
                .as_ref()
                .is_some_and(|value| value.len() > 256)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl SqliteStore {
    /// Keep an attachment snapshot with its conversation. Replacing the same
    /// identifier is allowed only within the same conversation.
    pub fn retain_attachment(&mut self, attachment: &RetainedAttachment) -> Result<()> {
        attachment.validate()?;
        let passages = serde_json::to_string(&attachment.passages).map_err(|_| storage_error())?;
        let pages = attachment
            .pages
            .map(|pages| serde_json::to_string(&pages))
            .transpose()
            .map_err(|_| storage_error())?;
        let created = unix_ms()?;
        let tx = self.connection.transaction().map_err(sql_error)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1)",
                [attachment.conversation_id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !exists {
            return Err(not_found());
        }
        let owner: Option<ConversationId> = tx
            .query_row(
                "SELECT conversation_id FROM chat_attachments WHERE public_id=?1",
                [&attachment.public_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        match owner {
            Some(owner) if owner != attachment.conversation_id => return Err(not_found()),
            Some(_) => {}
            None => {
                let count: i64 = tx
                    .query_row(
                        "SELECT count(*) FROM chat_attachments WHERE conversation_id=?1",
                        [attachment.conversation_id],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                if count >= MAX_RETAINED_PER_CONVERSATION as i64 {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        format!(
                            "A conversation can keep at most {MAX_RETAINED_PER_CONVERSATION} attachments. Remove one first."
                        ),
                    ));
                }
            }
        }
        let image = attachment.image.as_ref();
        tx.execute(
            "INSERT INTO chat_attachments
                (public_id,conversation_id,name,kind,file_bytes,fingerprint,pages,passages,
                 image,image_width,image_height,recognition,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(public_id) DO UPDATE SET name=excluded.name, passages=excluded.passages,
                 pages=excluded.pages, image=excluded.image, image_width=excluded.image_width,
                 image_height=excluded.image_height, recognition=excluded.recognition",
            params![
                attachment.public_id,
                attachment.conversation_id,
                attachment.name,
                attachment.kind.as_str(),
                attachment.file_bytes as i64,
                attachment.fingerprint.as_slice(),
                pages,
                passages,
                image.map(|image| image.bytes.as_slice()),
                image.map(|image| image.width),
                image.map(|image| image.height),
                attachment.recognition,
                created,
            ],
        )
        .map_err(sql_error)?;
        tx.commit().map_err(sql_error)
    }

    /// Every attachment kept with `conversation`, oldest first.
    pub fn retained_attachments(
        &self,
        conversation: ConversationId,
    ) -> Result<Vec<RetainedAttachment>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT public_id,conversation_id,name,kind,file_bytes,fingerprint,pages,passages,
                    image,image_width,image_height,recognition
                 FROM chat_attachments WHERE conversation_id=?1 ORDER BY id",
            )
            .map_err(sql_error)?;
        let rows = stmt
            .query_map([conversation], |row| {
                let kind: String = row.get(3)?;
                let fingerprint: Vec<u8> = row.get(5)?;
                let pages: Option<String> = row.get(6)?;
                let passages: String = row.get(7)?;
                let image: Option<Vec<u8>> = row.get(8)?;
                let width: Option<u32> = row.get(9)?;
                let height: Option<u32> = row.get(10)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, ConversationId>(1)?,
                    row.get::<_, String>(2)?,
                    kind,
                    row.get::<_, i64>(4)?,
                    fingerprint,
                    pages,
                    passages,
                    image.zip(width).zip(height),
                    row.get::<_, Option<String>>(11)?,
                ))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        rows.into_iter()
            .map(
                |(
                    public_id,
                    conversation_id,
                    name,
                    kind,
                    bytes,
                    fingerprint,
                    pages,
                    passages,
                    image,
                    recognition,
                )| {
                    Ok(RetainedAttachment {
                        public_id,
                        conversation_id,
                        name,
                        kind: AttachmentKind::parse(&kind).ok_or_else(storage_error)?,
                        file_bytes: u64::try_from(bytes).map_err(|_| storage_error())?,
                        fingerprint: fingerprint.try_into().map_err(|_| storage_error())?,
                        pages: pages
                            .map(|pages| serde_json::from_str(&pages))
                            .transpose()
                            .map_err(|_| storage_error())?,
                        passages: serde_json::from_str(&passages).map_err(|_| storage_error())?,
                        image: image.map(|((bytes, width), height)| StoredImage {
                            bytes,
                            width,
                            height,
                        }),
                        recognition,
                    })
                },
            )
            .collect()
    }

    /// Delete a kept attachment of `conversation`. Returns whether a row was
    /// removed. Secure delete overwrites the freed pages.
    pub fn remove_retained_attachment(
        &mut self,
        conversation: ConversationId,
        public_id: &str,
    ) -> Result<bool> {
        if !valid_attachment_id(public_id) {
            return Err(invalid());
        }
        let removed = self
            .connection
            .execute(
                "DELETE FROM chat_attachments WHERE conversation_id=?1 AND public_id=?2",
                params![conversation, public_id],
            )
            .map_err(sql_error)?;
        Ok(removed > 0)
    }

    /// The attachment record of the latest attempt answering a user message,
    /// with that message's text. `offset` 0 is the newest user message: the
    /// previous question while sending, the current one while regenerating.
    /// Used for follow-ups and to keep regeneration faithful.
    pub fn latest_attachment_turn(
        &self,
        conversation: ConversationId,
        offset: u32,
    ) -> Result<Option<(String, AttachmentRecord)>> {
        let user: Option<(i64, String)> = self
            .connection
            .query_row(
                "SELECT position, content FROM messages WHERE conversation_id=?1 AND role='user'
                 ORDER BY position DESC LIMIT 1 OFFSET ?2",
                params![conversation, offset],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((position, content)) = user else {
            return Ok(None);
        };
        let metadata: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT x.metadata FROM messages m JOIN chat_context_attempts x ON x.message_id=m.id
                 WHERE m.conversation_id=?1 AND m.role='assistant' AND m.position>?2
                 ORDER BY m.position DESC LIMIT 1",
                params![conversation, position],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        Ok(metadata
            .flatten()
            .and_then(|metadata| serde_json::from_str::<AttemptContext>(&metadata).ok())
            .and_then(|context| context.attachments)
            .map(|record| (content, record)))
    }
}

impl SqliteStore {
    /// The conversation and saved context of an assistant attempt.
    pub fn attachment_citation(
        &self,
        message: MessageId,
    ) -> Result<Option<(ConversationId, AttemptContext)>> {
        let row: Option<(ConversationId, Option<String>)> = self
            .connection
            .query_row(
                "SELECT m.conversation_id, x.metadata FROM messages m
                 JOIN chat_context_attempts x ON x.message_id=m.id
                 WHERE m.id=?1 AND m.role='assistant'",
                [message],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        Ok(row.and_then(|(conversation, metadata)| {
            metadata
                .and_then(|metadata| serde_json::from_str(&metadata).ok())
                .map(|context| (conversation, context))
        }))
    }
}

/// Mark which supplied attachment passages the finished answer cites.
pub(super) fn mark_citations(
    connection: &Connection,
    message: MessageId,
    answer: &str,
) -> Result<()> {
    let metadata: Option<Option<String>> = connection
        .query_row(
            "SELECT metadata FROM chat_context_attempts WHERE message_id=?1",
            [message],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(Some(metadata)) = metadata else {
        return Ok(());
    };
    let Ok(mut context) = serde_json::from_str::<AttemptContext>(&metadata) else {
        return Ok(());
    };
    let Some(record) = context.attachments.as_mut() else {
        return Ok(());
    };
    let supplied = context
        .fitted
        .map_or(0, |usage| usage.sources)
        .min(record.passages.len())
        .min(MAX_ATTACHMENT_SOURCES);
    let flags = cited_flags(answer, supplied, MAX_ATTACHMENT_SOURCES);
    for (passage, cited) in record.passages.iter_mut().zip(flags) {
        passage.cited = cited;
    }
    crate::chat_context::save_context(connection, message, &context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        attachment::{AttachmentRetention, AttachmentTask, AttachmentUse, PassageRecord},
        chat::{ChatStore, GenerationState, TurnAction},
        chat_context::ContextUsage,
        intelligence::{Receipt, SessionState},
    };

    fn id(n: u8) -> String {
        format!("a{}", format!("{n:02x}").repeat(16))
    }

    fn snapshot(conversation: ConversationId, n: u8) -> RetainedAttachment {
        RetainedAttachment {
            public_id: id(n),
            conversation_id: conversation,
            name: format!("synthetic-{n}.pdf"),
            kind: AttachmentKind::Pdf,
            file_bytes: 2048,
            fingerprint: [n; 32],
            pages: Some(PageCoverage {
                page_count: 2,
                pages_examined: 2,
                pages_without_text: 1,
            }),
            passages: vec![StoredPassage {
                source: PassageSource::Page { page: 1 },
                text: format!("Synthetic retained passage {n}."),
            }],
            image: None,
            recognition: None,
        }
    }

    #[test]
    fn retained_snapshots_stay_in_their_conversation_and_go_with_it() {
        let dir = std::env::temp_dir().join(format!("attachments-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut db = SqliteStore::open_test_fixture(&dir.join("chat.sqlite3")).unwrap();
        let first = db.create().unwrap().id;
        let second = db.create().unwrap().id;
        db.retain_attachment(&snapshot(first, 1)).unwrap();
        // The same identifier cannot be moved into another conversation.
        assert_eq!(
            db.retain_attachment(&snapshot(second, 1)).unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(
            db.retained_attachments(first).unwrap(),
            vec![snapshot(first, 1)]
        );
        assert!(db.retained_attachments(second).unwrap().is_empty());
        assert!(!db.remove_retained_attachment(second, &id(1)).unwrap());
        // An image snapshot must carry its bounded representation.
        let mut image = snapshot(first, 2);
        image.kind = AttachmentKind::Png;
        assert!(db.retain_attachment(&image).is_err());
        image.image = Some(StoredImage {
            bytes: b"\x89PNG\r\n\x1a\nsynthetic".to_vec(),
            width: 4,
            height: 3,
        });
        image.pages = None;
        db.retain_attachment(&image).unwrap();
        assert_eq!(db.retained_attachments(first).unwrap().len(), 2);
        for n in 3..=(MAX_RETAINED_PER_CONVERSATION as u8) {
            db.retain_attachment(&snapshot(first, n)).unwrap();
        }
        assert!(db.retain_attachment(&snapshot(first, 200)).is_err());
        assert!(db.remove_retained_attachment(first, &id(1)).unwrap());
        db.delete(first).unwrap();
        let rows: i64 = db
            .connection
            .query_row("SELECT count(*) FROM chat_attachments", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finished_answers_mark_only_supplied_cited_passages() {
        let dir = std::env::temp_dir().join(format!("attachment-cites-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut db = SqliteStore::open_test_fixture(&dir.join("chat.sqlite3")).unwrap();
        let conversation = db.create().unwrap().id;
        let passages = (0..3)
            .map(|ordinal| PassageRecord {
                attachment: id(7),
                ordinal,
                source: PassageSource::Lines {
                    start_line: ordinal + 1,
                    end_line: ordinal + 1,
                },
                cited: false,
            })
            .collect();
        let record = AttachmentRecord {
            version: 1,
            task: AttachmentTask::Relevant,
            attachments: vec![AttachmentUse {
                id: id(7),
                name: "notes.md".into(),
                kind: AttachmentKind::Markdown,
                retention: AttachmentRetention::Temporary,
                total_passages: 3,
                pages: None,
                recognized_text: false,
                image_sent: false,
            }],
            passages,
            shared_remotely: false,
            no_match: false,
        };
        let turn = db
            .begin_chat_with_context(
                conversation,
                TurnAction::Send("What does it say?".into()),
                None,
                AttemptContext {
                    attachments: Some(record.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let message = turn.assistant.id;
        db.checkpoint_chat_evidence(
            message,
            &AttemptContext {
                attachments: Some(record),
                ..Default::default()
            },
            ContextUsage {
                sources: 2,
                memories: 0,
            },
            &[],
        )
        .unwrap();
        let receipt = Receipt {
            version: 1,
            capability: agent_core::capability::GENERATE,
            state: SessionState::Completed,
            elapsed_ms: 1,
            provider: None,
            local_authorization: None,
            used: Vec::new(),
            withheld: Vec::new(),
            output_labels: AttachmentKind::Markdown.text_labels(),
            application_retention: agent_core::capability::Retention::SavedLocally,
            external: Vec::new(),
            usage: Vec::new(),
            usage_basis: None,
        };
        db.finish_intelligence_chat(
            message,
            "It says two things [S1] [S3].",
            GenerationState::Complete,
            None,
            &receipt,
        )
        .unwrap();
        let saved = db.chat_attempt_context(message).unwrap().unwrap();
        let cited: Vec<bool> = saved
            .attachments
            .unwrap()
            .passages
            .iter()
            .map(|passage| passage.cited)
            .collect();
        // S3 was not supplied, so it is not a valid citation.
        assert_eq!(cited, vec![true, false, false]);
        let (question, _) = db.latest_attachment_turn(conversation, 0).unwrap().unwrap();
        assert_eq!(question, "What does it say?");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
