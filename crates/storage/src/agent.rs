//! SQLite implementation of the trusted metadata-only agent journal.
use agent_core::agent::{
    AgentError, AgentErrorCode, AgentResult, AgentStore, AuditAction, PermissionAuditBatch,
    RuntimeEvent, StoredAudit, StoredExecution, StoredTask, StoredTaskDetail, StoredTaskEvent,
    TaskId, TaskState,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use super::{SqliteStore, unix_ms};

const MAX_EVENTS: i64 = 128;

fn failed() -> AgentError {
    AgentError::new(
        AgentErrorCode::StorageFailed,
        "The local agent journal operation failed.",
    )
}
fn invalid() -> AgentError {
    AgentError::new(
        AgentErrorCode::InvalidArguments,
        "Invalid agent journal request.",
    )
}
fn busy() -> AgentError {
    AgentError::new(AgentErrorCode::Busy, "An agent task is already active.")
}
fn sql<T>(result: rusqlite::Result<T>) -> AgentResult<T> {
    result.map_err(|_| failed())
}
fn now() -> AgentResult<i64> {
    unix_ms().map_err(|_| failed())
}
fn id_number(id: TaskId) -> AgentResult<i64> {
    i64::try_from(id.get()).map_err(|_| invalid())
}
fn read_id(id: i64) -> rusqlite::Result<TaskId> {
    u64::try_from(id)
        .ok()
        .and_then(TaskId::new)
        .ok_or(rusqlite::Error::InvalidQuery)
}
fn encoded(value: &impl Serialize) -> AgentResult<String> {
    let text = serde_json::to_string(value).map_err(|_| failed())?;
    if text.len() > 4096 {
        return Err(invalid());
    }
    Ok(text)
}
fn enum_text(value: &impl Serialize) -> AgentResult<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(failed)
}
fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|_| rusqlite::Error::InvalidQuery)
}
pub(super) fn parse_state(text: &str) -> rusqlite::Result<TaskState> {
    parse_json(&format!("\"{text}\""))
}
pub(super) fn read_excluded_turns(row: &Row<'_>, column: usize) -> rusqlite::Result<Option<usize>> {
    row.get::<_, Option<i64>>(column)?
        .map(|count| {
            usize::try_from(count)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, count))
        })
        .transpose()
}
fn read_task(row: &Row<'_>) -> rusqlite::Result<StoredTask> {
    let usage: Option<String> = row.get(5)?;
    Ok(StoredTask {
        id: read_id(row.get(0)?)?,
        state: parse_state(&row.get::<_, String>(1)?)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        finalized: row.get(4)?,
        usage: usage.as_deref().map(parse_json).transpose()?,
        error: row.get(6)?,
        conversation_id: row.get(7)?,
        turn_id: row.get(8)?,
        excluded_turns: read_excluded_turns(row, 9)?,
    })
}

/// Called within the exclusive startup transaction, or a trusted explicit
/// cleanup transaction after the task worker has stopped. Never run while
/// a live task may still dispatch. Preserve every known execution result.
pub(super) fn recover_interrupted(connection: &Connection, id: Option<i64>) -> AgentResult<()> {
    let timestamp = now()?;
    sql(connection.execute(
        "UPDATE agent_executions SET status = 'uncertain', updated_at = max(updated_at, ?1)
         WHERE status = 'pending' AND task_id IN (
             SELECT id FROM agent_tasks WHERE finalized = 0 AND (?2 IS NULL OR id = ?2)
         )",
        params![timestamp, id],
    ))?;
    sql(connection.execute(
        "INSERT INTO agent_task_events(task_id, sequence, created_at, event)
         SELECT id, event_sequence + 1, max(updated_at, ?1), '{\"kind\":\"recovered_interrupted\"}'
         FROM agent_tasks WHERE finalized = 0 AND (?2 IS NULL OR id = ?2)",
        params![timestamp, id],
    ))?;
    sql(connection.execute(
        "UPDATE agent_tasks SET state = 'interrupted', finalized = 1,
           event_sequence = event_sequence + 1, updated_at = max(updated_at, ?1)
         WHERE finalized = 0 AND (?2 IS NULL OR id = ?2)",
        params![timestamp, id],
    ))?;
    sql(connection.execute(
        "UPDATE messages SET state='interrupted' WHERE state='streaming' AND id IN (
            SELECT l.assistant_message_id FROM conversation_agent_turns l
            JOIN agent_tasks t ON t.id=l.task_id WHERE t.state='interrupted'
              AND (?1 IS NULL OR t.id=?1))",
        [id],
    ))?;
    sql(connection.execute(
        "UPDATE conversations SET updated_at=max(updated_at,?1) WHERE id IN (
            SELECT l.conversation_id FROM conversation_agent_turns l
            JOIN agent_tasks t ON t.id=l.task_id WHERE t.state='interrupted'
              AND (?2 IS NULL OR t.id=?2) AND t.updated_at=?1)",
        params![timestamp, id],
    ))?;
    Ok(())
}

impl SqliteStore {
    pub fn record_agent_excluded_turns(&mut self, id: TaskId, count: usize) -> AgentResult<()> {
        let id = id_number(id)?;
        let count = i64::try_from(count).map_err(|_| invalid())?;
        let changed = sql(self.connection.execute(
            "UPDATE conversation_agent_turns SET excluded_turns = max(coalesce(excluded_turns, 0), ?1)
             WHERE task_id = ?2 AND EXISTS (SELECT 1 FROM agent_tasks WHERE id = ?2 AND finalized = 0)",
            params![count, id],
        ))?;
        if changed != 1 {
            return Err(failed());
        }
        Ok(())
    }

    pub fn record_agent_event_with_answer(
        &mut self,
        id: TaskId,
        event: &RuntimeEvent,
        answer: Option<&str>,
    ) -> AgentResult<()> {
        let number = id_number(id)?;
        let payload = encoded(event)?;
        let timestamp = now()?;
        let tx = sql(self.connection.transaction())?;
        let (state, started, finalized, transition_seq, event_seq): (String, bool, bool, i64, i64) =
            sql(tx.query_row(
                "SELECT state, started, finalized, transition_sequence, event_sequence
                 FROM agent_tasks WHERE id = ?1",
                [number],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            ))?;
        if finalized || event_seq >= MAX_EVENTS {
            return Err(failed());
        }
        let current = sql(parse_state(&state))?;
        if !started && !matches!(event, RuntimeEvent::Started) {
            return Err(failed());
        }
        if current.is_terminal() && !matches!(event, RuntimeEvent::Finished { .. }) {
            return Err(failed());
        }

        match event {
            RuntimeEvent::Started => {
                if started || current != TaskState::Created || event_seq != 0 {
                    return Err(failed());
                }
                sql(tx.execute("UPDATE agent_tasks SET started = 1 WHERE id = ?1", [number]))?;
            }
            RuntimeEvent::Transition { transition } => {
                if transition.task_id != id
                    || transition.from != current
                    || i64::from(transition.sequence) != transition_seq + 1
                    || !valid_transition(current, transition.to)
                {
                    return Err(failed());
                }
                sql(tx.execute(
                    "UPDATE agent_tasks SET state = ?1, transition_sequence = ?2 WHERE id = ?3",
                    params![enum_text(&transition.to)?, transition.sequence, number],
                ))?;
            }
            RuntimeEvent::Proposed { .. } if current == TaskState::Validating => {}
            RuntimeEvent::RejectedProposal { .. } if current == TaskState::Planning => {}
            RuntimeEvent::DispatchIntent { operation_id, tool } => {
                if current != TaskState::Executing || !identifier(operation_id, 64) {
                    return Err(failed());
                }
                let count: i64 = sql(tx.query_row(
                    "SELECT count(*) FROM agent_executions WHERE task_id = ?1",
                    [number],
                    |r| r.get(0),
                ))?;
                if count >= 5 {
                    return Err(failed());
                }
                let tool = enum_text(tool)?;
                // The actual consumed ticket must already exist in durable audit.
                let authorized: bool = sql(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM permission_audit
                     WHERE task_id = ?1 AND operation_id = ?2 AND tool = ?3
                     AND action = 'authorization_consumed')",
                    params![number, operation_id, tool],
                    |r| r.get(0),
                ))?;
                if !authorized {
                    return Err(failed());
                }
                sql(tx.execute(
                    "INSERT INTO agent_executions(operation_id, task_id, tool, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
                    params![operation_id, number, tool, timestamp],
                ))?;
            }
            RuntimeEvent::Execution { record } => {
                if current != TaskState::Executing {
                    return Err(failed());
                }
                let changed = sql(tx.execute(
                    "UPDATE agent_executions SET status = ?1, error = ?2, updated_at = max(updated_at, ?3)
                     WHERE operation_id = ?4 AND task_id = ?5 AND tool = ?6 AND status = 'pending'",
                    params![enum_text(&record.status)?, record.error.as_ref().map(enum_text).transpose()?,
                        timestamp, record.operation_id, number, enum_text(&record.tool)?],
                ))?;
                if changed != 1 {
                    return Err(failed());
                }
            }
            RuntimeEvent::Finished {
                state,
                usage,
                error,
            } => {
                if !state.is_terminal() || *state != current {
                    return Err(failed());
                }
                let pending: bool = sql(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_executions WHERE task_id = ?1 AND status = 'pending')",
                    [number], |r| r.get(0),
                ))?;
                if pending {
                    return Err(failed());
                }
                sql(tx.execute(
                    "UPDATE agent_tasks SET finalized = 1, usage = ?1, error = ?2 WHERE id = ?3",
                    params![
                        encoded(usage)?,
                        error.as_ref().map(enum_text).transpose()?,
                        number
                    ],
                ))?;
                let state = match state {
                    TaskState::Completed if answer.is_some() => "complete",
                    TaskState::Cancelled => "cancelled",
                    TaskState::Interrupted => "interrupted",
                    _ => "failed",
                };
                sql(tx.execute(
                    "UPDATE messages SET state=?1, content=?2 WHERE id=(
                        SELECT assistant_message_id FROM conversation_agent_turns WHERE task_id=?3
                     ) AND state='streaming'",
                    params![state, answer.unwrap_or_default(), number],
                ))?;
                sql(tx.execute(
                    "UPDATE conversations SET updated_at=max(updated_at,?1) WHERE id=(
                        SELECT conversation_id FROM conversation_agent_turns WHERE task_id=?2)",
                    params![timestamp, number],
                ))?;
            }
            _ => return Err(failed()),
        }
        sql(tx.execute(
            "INSERT INTO agent_task_events(task_id, sequence, created_at, event)
             VALUES (?1, ?2, ?3, ?4)",
            params![number, event_seq + 1, timestamp, payload],
        ))?;
        sql(tx.execute(
            "UPDATE agent_tasks SET event_sequence = ?1, updated_at = max(updated_at, ?2) WHERE id = ?3",
            params![event_seq + 1, timestamp, number],
        ))?;
        sql(tx.commit())
    }
}

impl AgentStore for SqliteStore {
    fn allocate_agent_task(&mut self) -> AgentResult<TaskId> {
        let tx = sql(self.connection.transaction())?;
        let active: bool = sql(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE finalized = 0)
                 OR EXISTS(SELECT 1 FROM messages WHERE state = 'streaming')",
            [],
            |r| r.get(0),
        ))?;
        if active {
            return Err(busy());
        }
        let timestamp = now()?;
        let id = sql(tx.query_row(
            "INSERT INTO agent_tasks(created_at, updated_at) VALUES (?1, ?1) RETURNING id",
            [timestamp],
            |r| read_id(r.get(0)?),
        ))?;
        sql(tx.commit())?;
        Ok(id)
    }

    fn record_agent_event(&mut self, id: TaskId, event: &RuntimeEvent) -> AgentResult<()> {
        self.record_agent_event_with_answer(id, event, None)
    }

    fn persist_permission_audit(
        &mut self,
        namespace: &str,
        batch: &PermissionAuditBatch,
    ) -> AgentResult<()> {
        if !identifier(namespace, 32) || batch.records.len() > 1024 {
            return Err(invalid());
        }
        let Some(first) = batch.records.first() else {
            return if batch.omitted == 0 {
                Ok(())
            } else {
                Err(failed())
            };
        };
        let last = batch.records.last().ok_or_else(failed)?;
        if first.sequence == 0
            || batch
                .records
                .windows(2)
                .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err(invalid());
        }
        let first_seq = i64::try_from(first.sequence).map_err(|_| invalid())?;
        let last_seq = i64::try_from(last.sequence).map_err(|_| invalid())?;
        let omitted = i64::try_from(batch.omitted).map_err(|_| invalid())?;
        let timestamp = now()?;
        let tx = sql(self.connection.transaction())?;
        let existing: Option<(i64, i64)> = sql(tx
            .query_row(
                "SELECT last_sequence, omitted FROM permission_audit_batches
             WHERE namespace = ?1 AND first_sequence = ?2",
                params![namespace, first_seq],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional())?;
        if let Some(previous) = existing {
            if previous != (last_seq, omitted) {
                return Err(failed());
            }
        } else {
            let previous: i64 = sql(tx.query_row(
                "SELECT coalesce(max(sequence), 0) FROM permission_audit WHERE namespace = ?1",
                [namespace],
                |r| r.get(0),
            ))?;
            if previous.checked_add(1) != Some(first_seq) {
                return Err(failed());
            }
        }
        for record in &batch.records {
            let seq = i64::try_from(record.sequence).map_err(|_| invalid())?;
            let task = record.task_id.map(id_number).transpose()?;
            let operation = record.operation_id.as_deref();
            if operation.is_some_and(|id| {
                !identifier(id, 64) || !id.starts_with(&format!("{namespace}_op_"))
            }) || record
                .scope_id
                .as_deref()
                .is_some_and(|id| !identifier(id, 64))
            {
                return Err(invalid());
            }
            if record.action == AuditAction::AuthorizationConsumed
                && (task.is_none() || operation.is_none() || record.tool.is_none())
            {
                return Err(invalid());
            }
            let payload = encoded(record)?;
            if existing.is_some() {
                let saved: String = sql(tx.query_row(
                    "SELECT record FROM permission_audit WHERE namespace = ?1 AND sequence = ?2",
                    params![namespace, seq],
                    |r| r.get(0),
                ))?;
                if saved != payload {
                    return Err(failed());
                }
            } else {
                sql(tx.execute(
                    "INSERT INTO permission_audit(namespace, sequence, task_id, operation_id, tool, action, created_at, record)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![namespace, seq, task, operation, record.tool.as_ref().map(enum_text).transpose()?,
                        enum_text(&record.action)?, timestamp, payload],
                ))?;
            }
        }
        if existing.is_none() {
            sql(tx.execute(
                "INSERT INTO permission_audit_batches(namespace, first_sequence, last_sequence, omitted)
                 VALUES (?1, ?2, ?3, ?4)",
                params![namespace, first_seq, last_seq, omitted],
            ))?;
        }
        sql(tx.commit())
    }

    fn interrupt_agent_task(&mut self, id: TaskId) -> AgentResult<()> {
        let number = id_number(id)?;
        let tx = sql(self.connection.transaction())?;
        recover_interrupted(&tx, Some(number))?;
        sql(tx.commit())
    }

    fn list_agent_tasks(&self, before: Option<TaskId>, limit: u32) -> AgentResult<Vec<StoredTask>> {
        if !(1..=100).contains(&limit) {
            return Err(invalid());
        }
        let before = before.map(id_number).transpose()?;
        let mut statement = sql(self.connection.prepare(
            "SELECT t.id, t.state, t.created_at, t.updated_at, t.finalized, t.usage, t.error,
                    l.conversation_id, l.user_message_id, l.excluded_turns
             FROM agent_tasks t LEFT JOIN conversation_agent_turns l ON l.task_id = t.id
             WHERE (?1 IS NULL OR t.id < ?1) ORDER BY t.id DESC LIMIT ?2",
        ))?;
        let rows = sql(statement.query_map(params![before, limit], read_task))?;
        sql(rows.collect())
    }

    fn agent_task(&self, id: TaskId) -> AgentResult<StoredTaskDetail> {
        let number = id_number(id)?;
        let task = sql(self.connection.query_row(
            "SELECT t.id, t.state, t.created_at, t.updated_at, t.finalized, t.usage, t.error,
                    l.conversation_id, l.user_message_id, l.excluded_turns
             FROM agent_tasks t LEFT JOIN conversation_agent_turns l ON l.task_id = t.id
             WHERE t.id = ?1",
            [number],
            read_task,
        ))?;
        let mut statement = sql(self.connection.prepare(
            "SELECT sequence, created_at, event FROM agent_task_events WHERE task_id = ?1 ORDER BY sequence LIMIT 130",
        ))?;
        let rows = sql(statement.query_map([number], |r| {
            Ok(StoredTaskEvent {
                sequence: r.get(0)?,
                created_at: r.get(1)?,
                event: parse_json(&r.get::<_, String>(2)?)?,
            })
        }))?;
        let events = sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
        if events.len() > (MAX_EVENTS + 1) as usize {
            return Err(failed());
        }
        let mut statement = sql(self.connection.prepare(
            "SELECT operation_id, tool, status, error, created_at, updated_at FROM agent_executions
             WHERE task_id = ?1 ORDER BY created_at, operation_id LIMIT 6",
        ))?;
        let rows = sql(statement.query_map([number], |r| {
            Ok(StoredExecution {
                operation_id: r.get(0)?,
                tool: r.get(1)?,
                status: r.get(2)?,
                error: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        }))?;
        let executions = sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
        if executions.len() > 5 {
            return Err(failed());
        }
        let mut statement = sql(self.connection.prepare(
            "SELECT namespace, sequence, created_at, record FROM permission_audit
             WHERE task_id = ?1 ORDER BY namespace, sequence LIMIT 1025",
        ))?;
        let rows = sql(statement.query_map([number], |r| {
            Ok(StoredAudit {
                namespace: r.get(0)?,
                sequence: r.get(1)?,
                created_at: r.get(2)?,
                record: parse_json(&r.get::<_, String>(3)?)?,
            })
        }))?;
        let audit = sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
        if audit.len() > 1024 {
            return Err(failed());
        }
        Ok(StoredTaskDetail {
            task,
            events,
            executions,
            audit,
        })
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn valid_transition(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;
    !from.is_terminal()
        && (matches!(to, Cancelled | Failed | TimedOut | Interrupted)
            || matches!(
                (from, to),
                (Created, Planning)
                    | (Planning, Validating | Completed)
                    | (
                        Validating,
                        AwaitingAuthorization | Authorized | Denied | Planning
                    )
                    | (AwaitingAuthorization, Authorized | Denied)
                    | (Authorized, Executing | Denied)
                    | (Executing, Observing)
                    | (Observing, Planning)
            ))
}
