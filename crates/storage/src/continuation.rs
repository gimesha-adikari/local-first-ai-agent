//! Preserve a bounded native replay seed alongside a truncated Chat attempt.
use agent_core::{
    chat::{
        ConversationId, GenerationState, MessageId, MessageRole, ModelId, ModelSettings,
        PendingTurn, prepare_history,
    },
    inference::{
        AssistantReplay, ErrorCode, FinishReason, GenerationStats, InferenceError,
        MAX_CHAT_ATTEMPT_TOKENS, Result,
    },
};
use rusqlite::{Connection, OptionalExtension, params};

use super::{SqliteStore, active_generation, read_conversation, sql_error, storage_error};

const MAX_CONTINUATIONS: u32 = 3;

pub struct ContinuationPlan {
    pub turn: PendingTurn,
    pub settings: ModelSettings,
    pub replay: AssistantReplay,
    pub previous_stats: GenerationStats,
}

pub(super) fn remember_attempt(
    connection: &Connection,
    message_id: MessageId,
    model: ModelId,
    settings: ModelSettings,
) -> Result<()> {
    let encoded = serde_json::to_string(&settings).map_err(|_| storage_error())?;
    connection
        .execute(
            "INSERT INTO chat_attempt_replay(message_id,model_id,settings) VALUES(?1,?2,?3)",
            params![message_id, model, encoded],
        )
        .map_err(sql_error)?;
    Ok(())
}

fn ineligible(message: &'static str) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

fn plan(
    connection: &Connection,
    id: ConversationId,
    message_id: MessageId,
    model: ModelId,
) -> Result<(ContinuationPlan, u32)> {
    let conversation = read_conversation(connection, id)?;
    let mut assistant = conversation
        .messages
        .last()
        .filter(|message| {
            message.id == message_id
                && message.role == MessageRole::Assistant
                && message.state == GenerationState::Truncated
        })
        .cloned()
        .ok_or_else(|| ineligible("Only the latest truncated Chat response can be continued."))?;
    let Some(previous_stats) = assistant
        .stats
        .take()
        .filter(|stats| stats.reason == FinishReason::TokenLimit)
    else {
        return Err(ineligible("This response has no saved token-limit state."));
    };
    let stored = connection.query_row(
        "SELECT model_id,settings,replay,continuation_count FROM chat_attempt_replay
         WHERE message_id=?1 AND replay IS NOT NULL",
        [message_id], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?,
            r.get::<_,String>(2)?, r.get::<_,u32>(3)?)),
    ).optional().map_err(sql_error)?.ok_or_else(|| ineligible(
        "This response has no native continuation state. Regenerate it to start a continuable response.",
    ))?;
    if stored.0 != model {
        return Err(ineligible(
            "Load the original model to continue this response.",
        ));
    }
    let remaining = MAX_CONTINUATIONS.saturating_sub(stored.3);
    if remaining == 0 {
        return Err(ineligible(
            "This response reached its three-continuation limit.",
        ));
    }
    let settings: ModelSettings = serde_json::from_str(&stored.1).map_err(|_| storage_error())?;
    let replay: AssistantReplay = serde_json::from_str(&stored.2).map_err(|_| storage_error())?;
    if replay.generated.len() >= MAX_CHAT_ATTEMPT_TOKENS {
        return Err(ineligible(
            "This response reached its total generation limit.",
        ));
    }
    let (mut history, prompt) = prepare_history(&conversation)?;
    if replay.excluded_turns > history.turns.len() {
        return Err(ineligible(
            "The original conversation context is unavailable.",
        ));
    }
    history.turns.drain(..replay.excluded_turns);
    assistant.state = GenerationState::Streaming;
    Ok((
        ContinuationPlan {
            turn: PendingTurn {
                user: None,
                assistant,
                history,
                prompt,
            },
            settings,
            replay,
            previous_stats,
        },
        remaining,
    ))
}

impl SqliteStore {
    pub fn continuation_remaining(
        &self,
        id: ConversationId,
        message: MessageId,
        model: ModelId,
    ) -> Result<u32> {
        plan(&self.connection, id, message, model).map(|(_, remaining)| remaining)
    }

    pub fn begin_continuation(
        &mut self,
        id: ConversationId,
        message: MessageId,
        model: ModelId,
    ) -> Result<ContinuationPlan> {
        let tx = self.connection.transaction().map_err(sql_error)?;
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE state='streaming')
                OR EXISTS(SELECT 1 FROM agent_tasks WHERE finalized=0)",
                [],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if active {
            return Err(active_generation());
        }
        let (plan, _) = plan(&tx, id, message, model)?;
        tx.execute(
            "UPDATE messages SET state='streaming', stats=NULL WHERE id=?1",
            [message],
        )
        .map_err(sql_error)?;
        tx.execute("UPDATE chat_attempt_replay SET continuation_count=continuation_count+1 WHERE message_id=?1", [message]).map_err(sql_error)?;
        tx.commit().map_err(sql_error)?;
        Ok(plan)
    }
}
