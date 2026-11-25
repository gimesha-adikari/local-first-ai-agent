//! Unified conversation storage. Native observations never enter this module.
use super::{
    SqliteStore, active_generation, insert_message, not_found, read_conversation, sql_error,
    storage_error, title_from_prompt, touch_conversation, unix_ms,
};
use agent_core::{
    agent::{TaskId, TaskState},
    chat::{
        CompletedTurn, Conversation, ConversationId, GenerationState, Message, MessageId,
        MessageRole,
    },
    inference::{ErrorCode, InferenceError, Result},
};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::collections::HashMap;

use super::agent::{parse_state, read_excluded_turns};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTurnLink {
    pub conversation_id: ConversationId,
    pub turn_id: MessageId,
    pub assistant_id: MessageId,
    pub task_id: TaskId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    Chat,
    Agent,
}

#[derive(Debug)]
pub struct ConversationTurn {
    pub user: Message,
    pub assistants: Vec<(Message, Option<String>)>,
    pub agent_task: Option<TaskId>,
    pub task_timeout_ms: Option<u32>,
    pub task_state: Option<TaskState>,
    pub task_error: Option<String>,
    pub excluded_turns: Option<usize>,
}

struct TurnTask {
    id: TaskId,
    timeout_ms: u32,
    state: TaskState,
    error: Option<String>,
    excluded_turns: Option<usize>,
}

impl ConversationTurn {
    pub fn kind(&self) -> TurnKind {
        if self.agent_task.is_some() {
            TurnKind::Agent
        } else {
            TurnKind::Chat
        }
    }
}

fn task_id(value: i64) -> rusqlite::Result<TaskId> {
    u64::try_from(value)
        .ok()
        .and_then(TaskId::new)
        .ok_or(rusqlite::Error::InvalidQuery)
}

const CONTEXT_TURNS: i64 = 12;

impl SqliteStore {
    pub fn begin_agent_turn(
        &mut self,
        id: ConversationId,
        prompt: &str,
        model_name: &str,
        timeout_ms: u64,
    ) -> Result<AgentTurnLink> {
        if prompt.trim().is_empty()
            || prompt.len() > 8192
            || prompt.contains('\0')
            || !(1000..=600_000).contains(&timeout_ms)
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Invalid Agent turn.",
            ));
        }
        let timeout_ms = i64::try_from(timeout_ms).map_err(|_| storage_error())?;
        let now = unix_ms()?;
        let tx = self.connection.transaction().map_err(sql_error)?;
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE finalized = 0)
                 OR EXISTS(SELECT 1 FROM messages WHERE state = 'streaming')",
                [],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if active {
            return Err(active_generation());
        }
        let title: String = tx
            .query_row("SELECT title FROM conversations WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(sql_error)?
            .ok_or_else(not_found)?;
        let position: i64 = tx
            .query_row(
                "SELECT coalesce(max(position),-1) FROM messages WHERE conversation_id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        let user = insert_message(
            &tx,
            id,
            position.checked_add(1).ok_or_else(storage_error)?,
            MessageRole::User,
            GenerationState::Complete,
            prompt.to_owned(),
            now,
        )?;
        let assistant = insert_message(
            &tx,
            id,
            position.checked_add(2).ok_or_else(storage_error)?,
            MessageRole::Assistant,
            GenerationState::Streaming,
            String::new(),
            now,
        )?;
        tx.execute(
            "UPDATE messages SET model_name=?1 WHERE id=?2",
            params![model_name, assistant.id],
        )
        .map_err(sql_error)?;
        let task: i64 = tx
            .query_row(
                "INSERT INTO agent_tasks(created_at,updated_at) VALUES(?1,?1) RETURNING id",
                [now],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        tx.execute(
            "INSERT INTO conversation_agent_turns(task_id, conversation_id, user_message_id,
                assistant_message_id, task_timeout_ms, excluded_turns) VALUES(?1,?2,?3,?4,?5,
                (SELECT max(count(*)-?6, 0) FROM messages
                 WHERE conversation_id=?2 AND role='user' AND position<?7))",
            params![
                task,
                id,
                user.id,
                assistant.id,
                timeout_ms,
                CONTEXT_TURNS,
                user.position
            ],
        )
        .map_err(sql_error)?;
        if position == -1 && title == super::NEW_TITLE {
            title_from_prompt(&tx, id, prompt)?;
        }
        touch_conversation(&tx, id, now)?;
        tx.commit().map_err(sql_error)?;
        Ok(AgentTurnLink {
            conversation_id: id,
            turn_id: user.id,
            assistant_id: assistant.id,
            task_id: task_id(task).map_err(sql_error)?,
        })
    }

    pub fn unified_conversation(
        &self,
        id: ConversationId,
    ) -> Result<(Conversation, Vec<ConversationTurn>)> {
        let mut conversation = read_conversation(&self.connection, id)?;
        let mut stmt = self.connection.prepare(
            "SELECT id, model_name FROM messages WHERE conversation_id=?1 AND model_name IS NOT NULL",
        ).map_err(sql_error)?;
        let mut models: HashMap<MessageId, String> = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)?;
        let mut stmt = self.connection.prepare(
            "SELECT l.user_message_id, t.id, l.task_timeout_ms, t.state, t.error, l.excluded_turns
             FROM conversation_agent_turns l JOIN agent_tasks t ON t.id=l.task_id
             WHERE l.conversation_id=?1",
        ).map_err(sql_error)?;
        let mut links: HashMap<MessageId, TurnTask> = stmt
            .query_map([id], |r| {
                Ok((
                    r.get(0)?,
                    TurnTask {
                        id: task_id(r.get(1)?)?,
                        timeout_ms: r.get(2)?,
                        state: parse_state(&r.get::<_, String>(3)?)?,
                        error: r.get(4)?,
                        excluded_turns: read_excluded_turns(r, 5)?,
                    },
                ))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)?;
        let mut turns: Vec<ConversationTurn> = Vec::new();
        for message in std::mem::take(&mut conversation.messages) {
            match message.role {
                MessageRole::User => {
                    let task = links.remove(&message.id);
                    turns.push(ConversationTurn {
                        agent_task: task.as_ref().map(|task| task.id),
                        task_timeout_ms: task.as_ref().map(|task| task.timeout_ms),
                        task_state: task.as_ref().map(|task| task.state),
                        excluded_turns: task.as_ref().and_then(|task| task.excluded_turns),
                        task_error: task.and_then(|task| task.error),
                        user: message,
                        assistants: Vec::new(),
                    });
                }
                MessageRole::Assistant => {
                    if let Some(turn) = turns.last_mut() {
                        let model = models.remove(&message.id);
                        turn.assistants.push((message, model));
                    }
                }
                MessageRole::System => {}
            }
        }
        Ok((conversation, turns))
    }

    /// Fetch only a bounded suffix, and only the latest attempt of a Chat turn.
    /// An incomplete earlier task can still have a transient native result.
    pub fn agent_context(
        &self,
        id: ConversationId,
    ) -> Result<(Vec<(MessageId, CompletedTurn)>, usize)> {
        let count: i64 = self
            .connection
            .query_row(
                "SELECT count(*) FROM messages WHERE conversation_id=?1 AND role='user'",
                [id],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT u.id,u.content, substr(a.content,1,8192),a.state
             FROM messages u LEFT JOIN messages a ON a.id=(
                SELECT older.id FROM messages older WHERE older.conversation_id=u.conversation_id
                  AND older.position>u.position AND older.position<coalesce((
                    SELECT min(n.position) FROM messages n WHERE n.conversation_id=u.conversation_id
                       AND n.position>u.position AND n.role='user'),9223372036854775807)
                  AND older.role='assistant' ORDER BY older.position DESC LIMIT 1)
             WHERE u.conversation_id=?1 AND u.role='user'
             ORDER BY u.position DESC LIMIT ?2",
            )
            .map_err(sql_error)?;
        let mut turns: Vec<_> = statement
            .query_map(params![id, CONTEXT_TURNS], |r| {
                let state: Option<String> = r.get(3)?;
                Ok((
                    r.get(0)?,
                    CompletedTurn {
                        user: r.get(1)?,
                        assistant: if state.as_deref() == Some("complete") {
                            r.get::<_, Option<String>>(2)?.unwrap_or_default()
                        } else {
                            String::new()
                        },
                    },
                ))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)?;
        turns.reverse();
        Ok((
            turns,
            usize::try_from((count - CONTEXT_TURNS).max(0)).map_err(|_| storage_error())?,
        ))
    }

    pub(crate) fn delete_with_agent_turns(&mut self, id: ConversationId) -> Result<()> {
        let tx = self.connection.transaction().map_err(sql_error)?;
        let generating: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id=?1 AND state='streaming')
               OR EXISTS(SELECT 1 FROM conversation_agent_turns l JOIN agent_tasks t ON t.id=l.task_id
                 WHERE l.conversation_id=?1 AND t.finalized=0)", [id], |r| r.get(0),
        ).map_err(sql_error)?;
        if generating {
            return Err(active_generation());
        }
        // Retain only detached permission audit metadata and its sequence ledger.
        // No path, prompt, answer, task link, or operation/scope ID remains in these rows.
        tx.execute(
            "UPDATE permission_audit SET task_id=NULL, operation_id=NULL,
               record=json_remove(record,'$.taskId','$.operationId','$.scopeId')
             WHERE task_id IN (SELECT task_id FROM conversation_agent_turns WHERE conversation_id=?1)",
            [id],
        ).map_err(sql_error)?;
        for statement in [
            "DELETE FROM agent_executions WHERE task_id IN (SELECT task_id FROM conversation_agent_turns WHERE conversation_id=?1)",
            "DELETE FROM agent_task_events WHERE task_id IN (SELECT task_id FROM conversation_agent_turns WHERE conversation_id=?1)",
            "DELETE FROM agent_tasks WHERE id IN (SELECT task_id FROM conversation_agent_turns WHERE conversation_id=?1)",
        ] {
            tx.execute(statement, [id]).map_err(sql_error)?;
        }
        if tx
            .execute("DELETE FROM conversations WHERE id=?1", [id])
            .map_err(sql_error)?
            != 1
        {
            return Err(not_found());
        }
        tx.commit().map_err(sql_error)
    }
}
