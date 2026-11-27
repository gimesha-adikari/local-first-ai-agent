use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agent_core::{
    chat::{
        ChatStore, Conversation, ConversationId, ConversationSummary, GenerationState, Message,
        MessageId, MessageRole, ModelId, ModelReference, ModelSettings, PendingTurn, Preferences,
        SavedModel, TurnAction, prepare_history, validate_system_prompt, validate_title,
    },
    classification::{Labels, USER_INPUT},
    inference::{ErrorCode, GenerationRequest, GenerationStats, InferenceError, Result},
    intelligence::Receipt,
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

mod agent;
pub mod chat_context;
mod continuation;
mod conversation;
mod intelligence;
pub mod knowledge;
pub mod memory;
pub mod models;
pub mod profile;
pub mod providers;
pub mod semantic;
pub use continuation::ContinuationPlan;
pub use conversation::{AgentTurnLink, ConversationTurn, TurnKind};
#[cfg(test)]
mod agent_tests;

const SCHEMA_VERSION: i64 = 15;
const MODEL_COLUMNS: &str = "id, file_name, settings, bundled_resource IS NOT NULL, settings_mode = 'automatic', artifact_key";
const NEW_TITLE: &str = "New conversation";

/// Local storage for one app instance. Exclusive locking protects crash recovery
/// from marking another live instance's pending response as interrupted.
pub struct SqliteStore {
    connection: Connection,
    // Held until the SQLite connection has released its own lock.
    _profile_lock: Option<std::sync::Arc<std::fs::File>>,
}

impl SqliteStore {
    /// Opens an unencrypted synthetic fixture. Production profiles use `profile::Profile`.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn open_test_fixture(path: &Path) -> Result<Self> {
        Self::initialize(Connection::open(path).map_err(sql_error)?)
    }

    fn initialize(mut connection: Connection) -> Result<Self> {
        connection
            .busy_timeout(Duration::from_millis(250))
            .map_err(sql_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA trusted_schema = OFF;
                 PRAGMA secure_delete = ON;
                 PRAGMA temp_store = MEMORY;
                 PRAGMA locking_mode = EXCLUSIVE;",
            )
            .map_err(sql_error)?;

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Exclusive)
            .map_err(sql_error)?;
        let version = transaction
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(sql_error)?;
        if !(0..=SCHEMA_VERSION).contains(&version) {
            return Err(InferenceError::new(
                ErrorCode::Storage,
                "This chat database uses an unsupported schema version.",
            ));
        }
        if version == 0 {
            transaction
                .execute_batch(include_str!("../migrations/001_chat.sql"))
                .map_err(sql_error)?;
            let preferences = Preferences {
                defaults: ModelSettings::recommended(8192),
                ..Preferences::default()
            };
            let encoded = serde_json::to_string(&preferences).map_err(|_| storage_error())?;
            transaction
                .execute(
                    "INSERT INTO application_preferences (id, preferences) VALUES (1, ?1)",
                    [encoded],
                )
                .map_err(sql_error)?;
        }
        if version < 2 {
            transaction
                .execute_batch(include_str!("../migrations/002_agent.sql"))
                .map_err(sql_error)?;
        }
        if version < 3 {
            transaction
                .execute_batch(include_str!(
                    "../migrations/003_conversation_agent_turns.sql"
                ))
                .map_err(sql_error)?;
        }
        if version < 4 {
            transaction
                .execute_batch(include_str!("../migrations/004_agent_turn_status.sql"))
                .map_err(sql_error)?;
            transaction
                .execute_batch(include_str!("../migrations/004_chat_continuation.sql"))
                .map_err(sql_error)?;
        }
        if version < 5 {
            transaction
                .execute_batch(include_str!("../migrations/005_bundled_model.sql"))
                .map_err(sql_error)?;
        }
        if version < 6 {
            transaction
                .execute_batch(include_str!("../migrations/006_knowledge.sql"))
                .map_err(sql_error)?;
        }
        if version < 7 {
            transaction
                .execute_batch(include_str!("../migrations/007_embeddings.sql"))
                .map_err(sql_error)?;
        }
        if version < 8 {
            transaction
                .execute_batch(include_str!("../migrations/008_chat_context.sql"))
                .map_err(sql_error)?;
        }
        if version < 9 {
            transaction
                .execute_batch(include_str!("../migrations/009_attempt_evidence.sql"))
                .map_err(sql_error)?;
        }
        if version < 10 {
            transaction
                .execute_batch(include_str!("../migrations/010_model_management.sql"))
                .map_err(sql_error)?;
            models::classify_existing_settings(&transaction)?;
        }
        if version < 11 {
            transaction
                .execute_batch(include_str!("../migrations/011_routing_and_identity.sql"))
                .map_err(sql_error)?;
        }
        if version < 12 {
            transaction
                .execute_batch(include_str!("../migrations/012_intelligence.sql"))
                .map_err(sql_error)?;
        }
        if version < 13 {
            transaction
                .execute_batch(include_str!("../migrations/013_providers.sql"))
                .map_err(sql_error)?;
        }
        if version < 14 {
            transaction
                .execute_batch(include_str!("../migrations/014_provider_deletion.sql"))
                .map_err(sql_error)?;
        }
        if version < 15 {
            transaction
                .execute_batch(include_str!("../migrations/015_glance_memory.sql"))
                .map_err(sql_error)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(sql_error)?;
        }
        agent::recover_interrupted(&transaction, None).map_err(|_| storage_error())?;
        providers::settle_interrupted(&transaction)?;
        chat_context::recover_citations(&transaction)?;
        transaction
            .execute(
                "UPDATE messages SET state = 'interrupted' WHERE state = 'streaming'",
                [],
            )
            .map_err(sql_error)?;
        transaction
            .execute(
                "UPDATE chat_attempt_replay SET replay=NULL WHERE replay IS NOT NULL
             AND message_id IN (SELECT id FROM messages WHERE state <> 'truncated')",
                [],
            )
            .map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(Self {
            connection,
            _profile_lock: None,
        })
    }
}

/// A listed model with its saved path.
#[derive(Debug, Clone)]
pub struct SavedModelRow {
    pub reference: ModelReference,
    pub path: PathBuf,
}

/// Result of registering the application-bundled model.
#[derive(Debug, Clone)]
pub struct BundledRegistration {
    pub reference: ModelReference,
    /// True when no user-imported model exists, so the bundled model is the
    /// only choice and may be loaded automatically.
    pub only_model: bool,
}

impl SqliteStore {
    /// Record the bundled model without changing user-imported rows, existing
    /// settings or recent-use order. A new row is listed after earlier models.
    pub fn register_bundled_model(
        &mut self,
        resource: &str,
        path: &Path,
        name: &str,
        settings: ModelSettings,
    ) -> Result<BundledRegistration> {
        settings.validate()?;
        let path = path
            .to_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::InvalidPath,
                    "The bundled model path could not be saved as Unicode.",
                )
            })?;
        if resource.is_empty() || name.is_empty() {
            return Err(InferenceError::new(
                ErrorCode::InvalidPath,
                "The bundled model identity is invalid.",
            ));
        }
        let encoded = serde_json::to_string(&settings).map_err(|_| storage_error())?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let previous: Option<(ModelId, String)> = transaction
            .query_row(
                "SELECT id, path FROM models WHERE bundled_resource = ?1",
                [resource],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let at_path: Option<ModelId> = transaction
            .query_row("SELECT id FROM models WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .optional()
            .map_err(sql_error)?;
        let id = match (previous, at_path) {
            (Some((id, old)), _) if old == path => id,
            // The same file is already listed (for example a manual import).
            // Mark that row instead of creating a duplicate.
            (previous, Some(existing)) => {
                if let Some((id, _)) = previous {
                    transaction
                        .execute(
                            "UPDATE models SET bundled_resource = NULL WHERE id = ?1",
                            [id],
                        )
                        .map_err(sql_error)?;
                }
                transaction
                    .execute(
                        "UPDATE models SET bundled_resource = ?1 WHERE id = ?2",
                        params![resource, existing],
                    )
                    .map_err(sql_error)?;
                existing
            }
            // The resource moved (reinstall or relocated bundle).
            (Some((id, _)), None) => {
                transaction
                    .execute(
                        "UPDATE models SET path = ?1, file_name = ?2 WHERE id = ?3",
                        params![path, name, id],
                    )
                    .map_err(sql_error)?;
                id
            }
            (None, None) => transaction
                .query_row(
                    "INSERT INTO models (path, file_name, settings, last_used_at, bundled_resource)
                     VALUES (?1, ?2, ?3, 0, ?4) RETURNING id",
                    params![path, name, encoded, resource],
                    |row| row.get(0),
                )
                .map_err(sql_error)?,
        };
        let others: i64 = transaction
            .query_row("SELECT count(*) FROM models WHERE id <> ?1", [id], |row| {
                row.get(0)
            })
            .map_err(sql_error)?;
        let reference = transaction
            .query_row(
                &format!("SELECT {MODEL_COLUMNS} FROM models WHERE id = ?1"),
                [id],
                model_reference,
            )
            .map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(BundledRegistration {
            reference,
            only_model: others == 0,
        })
    }

    pub fn finish_chat_with_sources(
        &mut self,
        message_id: MessageId,
        content: &str,
        state: GenerationState,
        stats: Option<&GenerationStats>,
        candidates: &[knowledge::KnowledgeHit],
    ) -> Result<()> {
        self.finish_chat_inner(message_id, content, state, stats, candidates, None)
    }

    pub fn finish_intelligence_chat(
        &mut self,
        message_id: MessageId,
        content: &str,
        state: GenerationState,
        stats: Option<&GenerationStats>,
        receipt: &Receipt,
    ) -> Result<()> {
        self.finish_chat_inner(message_id, content, state, stats, &[], Some(receipt))
    }

    fn finish_chat_inner(
        &mut self,
        message_id: MessageId,
        content: &str,
        state: GenerationState,
        stats: Option<&GenerationStats>,
        candidates: &[knowledge::KnowledgeHit],
        receipt: Option<&Receipt>,
    ) -> Result<()> {
        if state == GenerationState::Streaming {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Completing generation requires a final state.",
            ));
        }
        let replay = stats.and_then(|stats| stats.assistant_replay.as_ref());
        let replay = if state == GenerationState::Truncated {
            replay
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| storage_error())?
        } else {
            None
        };
        let stats_usage = stats.and_then(|stats| stats.context_usage);
        let stats = stats
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| storage_error())?;
        let transaction = self.connection.transaction().map_err(sql_error)?;
        if let Some(receipt) = receipt {
            intelligence::save_classification(&transaction, message_id, &receipt.output_labels)?;
            intelligence::save_receipt(&transaction, message_id, receipt)?;
        }
        let conversation: ConversationId = transaction
            .query_row(
                "UPDATE messages SET content = ?1, state = ?2, stats = ?3
                 WHERE id = ?4 AND role = 'assistant' AND state = 'streaming'
                 RETURNING conversation_id",
                params![content, stored_state(state), stats, message_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(inactive_generation)?;
        transaction
            .execute(
                "UPDATE chat_attempt_replay SET replay=?1 WHERE message_id=?2",
                params![replay, message_id],
            )
            .map_err(sql_error)?;
        if let Some(usage) = stats_usage.filter(|_| !candidates.is_empty()) {
            chat_context::record_evidence(&transaction, message_id, candidates, usage.sources)?;
        }
        chat_context::mark_citations(&transaction, message_id, content)?;
        touch_conversation(&transaction, conversation, unix_ms()?)?;
        transaction.commit().map_err(sql_error)
    }

    pub fn chat_regeneration_prompt(&self, id: ConversationId) -> Result<String> {
        self.connection.query_row(
            "SELECT m.content, EXISTS(SELECT 1 FROM conversation_agent_turns l WHERE l.user_message_id=m.id)
             FROM messages m WHERE m.conversation_id=?1 AND m.role='user'
             ORDER BY m.position DESC LIMIT 1",
            [id],
            |row| Ok((row.get(0)?, row.get::<_, bool>(1)?)),
        ).optional().map_err(sql_error)?.map_or_else(
            || Err(InferenceError::new(ErrorCode::InvalidConfig, "Send a message before regenerating.")),
            |(prompt, agent)| if agent {
                Err(InferenceError::new(ErrorCode::InvalidConfig, "Send a new Chat message after an Agent turn."))
            } else {
                Ok(prompt)
            },
        )
    }

    pub fn begin_chat(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
    ) -> Result<PendingTurn> {
        self.begin_chat_inner(id, action, model_name, None, None, None)
    }

    pub fn begin_chat_attempt(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
        model_id: ModelId,
        settings: ModelSettings,
    ) -> Result<PendingTurn> {
        self.begin_chat_inner(
            id,
            action,
            model_name,
            Some((model_id, settings)),
            None,
            None,
        )
    }

    /// An ordinary attempt that also records how a request with selected
    /// documents was routed. The answer stays ordinary for history and Continue.
    pub fn begin_routed_chat_attempt(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
        model_id: ModelId,
        settings: ModelSettings,
        route: &agent_core::chat_context::KnowledgeCoverage,
    ) -> Result<PendingTurn> {
        self.begin_chat_inner(
            id,
            action,
            model_name,
            Some((model_id, settings)),
            None,
            Some(route),
        )
    }

    /// A routed attempt answered by a remote provider. It has no local
    /// model recipe, so it can never be continued with exact local replay.
    pub fn begin_routed_remote_chat(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
        route: &agent_core::chat_context::KnowledgeCoverage,
    ) -> Result<PendingTurn> {
        self.begin_chat_inner(id, action, model_name, None, None, Some(route))
    }

    pub fn begin_contextual_chat_attempt(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
    ) -> Result<PendingTurn> {
        self.begin_chat_with_context(id, action, model_name, Default::default())
    }

    pub fn begin_chat_with_context(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
        context: agent_core::chat_context::AttemptContext,
    ) -> Result<PendingTurn> {
        self.begin_chat_inner(id, action, model_name, None, Some(context), None)
    }

    fn begin_chat_inner(
        &mut self,
        id: ConversationId,
        action: TurnAction,
        model_name: Option<&str>,
        recipe: Option<(ModelId, ModelSettings)>,
        context: Option<agent_core::chat_context::AttemptContext>,
        route: Option<&agent_core::chat_context::KnowledgeCoverage>,
    ) -> Result<PendingTurn> {
        let now = unix_ms()?;
        let transaction = self.connection.transaction().map_err(sql_error)?;
        let generating: bool = transaction
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM messages WHERE state = 'streaming')
                    OR EXISTS (SELECT 1 FROM agent_tasks WHERE finalized = 0)",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if generating {
            return Err(active_generation());
        }
        let mut conversation = read_conversation(&transaction, id)?;
        let first_message = conversation.messages.is_empty();
        let user = match action {
            TurnAction::Send(content) => {
                GenerationRequest::new(&content).validate()?;
                let user = insert_message(
                    &transaction,
                    id,
                    next_position(&conversation)?,
                    MessageRole::User,
                    GenerationState::Complete,
                    content,
                    now,
                )?;
                conversation.messages.push(user.clone());
                Some(user)
            }
            TurnAction::Regenerate => {
                if let Some(user) = conversation
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == MessageRole::User)
                {
                    let agent_turn: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM conversation_agent_turns WHERE user_message_id = ?1)",
                        [user.id], |row| row.get(0),
                    ).map_err(sql_error)?;
                    if agent_turn {
                        return Err(InferenceError::new(
                            ErrorCode::InvalidConfig,
                            "Send a new Chat message after an Agent turn.",
                        ));
                    }
                }
                None
            }
        };
        chat_context::exclude_contextual_answers(&transaction, &mut conversation)?;
        let (history, prompt) = prepare_history(&conversation)?;
        let prompt_source = intelligence::prompt_source(&conversation)?;
        let mut assistant = insert_message(
            &transaction,
            id,
            next_position(&conversation)?,
            MessageRole::Assistant,
            GenerationState::Streaming,
            String::new(),
            now,
        )?;
        if let Some(name) = model_name {
            transaction
                .execute(
                    "UPDATE messages SET model_name = ?1 WHERE id = ?2",
                    params![name, assistant.id],
                )
                .map_err(sql_error)?;
        }
        if let Some(context) = context {
            chat_context::save_context(&transaction, assistant.id, &context)?;
            assistant.context = Some(Box::new(context));
        }
        if let Some((model_id, settings)) = recipe {
            continuation::remember_attempt(&transaction, assistant.id, model_id, settings)?;
        }
        if let Some(route) = route {
            let metadata = serde_json::to_string(route).map_err(|_| storage_error())?;
            transaction
                .execute(
                    "INSERT INTO chat_route_coverage (message_id, metadata) VALUES (?1, ?2)",
                    params![assistant.id, metadata],
                )
                .map_err(sql_error)?;
            assistant.route_coverage = Some(Box::new(route.clone()));
        }
        if first_message && conversation.title == NEW_TITLE {
            title_from_prompt(&transaction, id, &prompt)?;
        }
        touch_conversation(&transaction, id, now)?;
        transaction.commit().map_err(sql_error)?;
        Ok(PendingTurn {
            user,
            assistant,
            history,
            prompt,
            prompt_source,
        })
    }
}

fn title_from_prompt(connection: &Connection, id: ConversationId, prompt: &str) -> Result<()> {
    let preview: String = prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .take(60)
        .collect();
    if !preview.is_empty() {
        connection
            .execute(
                "UPDATE conversations SET title = ?1 WHERE id = ?2",
                params![preview, id],
            )
            .map_err(sql_error)?;
    }
    Ok(())
}

impl ChatStore for SqliteStore {
    fn list(&self, query: &str) -> Result<Vec<ConversationSummary>> {
        let query = query.trim();
        if query.len() > 4096 || query.contains('\0') {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Enter a search term of at most 4 KiB without NUL characters.",
            ));
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT c.id, c.title, c.updated_at FROM conversations c
                 WHERE ?1 = '' OR instr(lower(c.title), lower(?1)) > 0
                    OR EXISTS (
                       SELECT 1 FROM messages m
                       WHERE m.conversation_id = c.id AND instr(lower(m.content), lower(?1)) > 0
                    )
                 ORDER BY c.updated_at DESC, c.id DESC",
            )
            .map_err(sql_error)?;
        statement
            .query_map([query], |row| {
                Ok(ConversationSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }

    fn get(&self, id: ConversationId) -> Result<Conversation> {
        read_conversation(&self.connection, id)
    }

    fn create(&mut self) -> Result<Conversation> {
        let now = unix_ms()?;
        let id = self
            .connection
            .query_row(
                "INSERT INTO conversations (title, created_at, updated_at)
                 VALUES (?1, ?2, ?2) RETURNING id",
                params![NEW_TITLE, now],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        Ok(Conversation {
            id,
            title: NEW_TITLE.into(),
            system_prompt: String::new(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        })
    }

    fn rename(&mut self, id: ConversationId, title: &str) -> Result<()> {
        let title = validate_title(title)?;
        updated(self.connection.execute(
            "UPDATE conversations SET title = ?1, updated_at = max(updated_at, ?2)
                 WHERE id = ?3",
            params![title, unix_ms()?, id],
        ))
    }

    fn set_system_prompt(&mut self, id: ConversationId, prompt: &str) -> Result<()> {
        validate_system_prompt(prompt)?;
        updated(self.connection.execute(
            "UPDATE conversations SET system_prompt = ?1, updated_at = max(updated_at, ?2)
                 WHERE id = ?3",
            params![prompt, unix_ms()?, id],
        ))
    }

    fn delete(&mut self, id: ConversationId) -> Result<()> {
        self.delete_with_agent_turns(id)
    }

    fn begin(&mut self, id: ConversationId, action: TurnAction) -> Result<PendingTurn> {
        self.begin_chat(id, action, None)
    }

    fn checkpoint(&mut self, message_id: MessageId, content: &str) -> Result<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE messages SET content = ?1
                 WHERE id = ?2 AND role = 'assistant' AND state = 'streaming'",
                params![content, message_id],
            )
            .map_err(sql_error)?;
        if changed == 0 {
            return Err(inactive_generation());
        }
        Ok(())
    }

    fn finish(
        &mut self,
        message_id: MessageId,
        content: &str,
        state: GenerationState,
        stats: Option<&GenerationStats>,
    ) -> Result<()> {
        self.finish_chat_with_sources(message_id, content, state, stats, &[])
    }

    fn preferences(&self) -> Result<Preferences> {
        let saved = self
            .connection
            .query_row(
                "SELECT preferences FROM application_preferences WHERE id = 1",
                [],
                |row| json_column(row, 0),
            )
            .optional()
            .map_err(sql_error)?;
        Ok(saved.unwrap_or_default())
    }

    fn save_preferences(&mut self, preferences: &Preferences) -> Result<()> {
        preferences.defaults.validate()?;
        let encoded = serde_json::to_string(preferences).map_err(|_| storage_error())?;
        self.connection
            .execute(
                "INSERT INTO application_preferences (id, preferences) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET preferences = excluded.preferences",
                [encoded],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    fn recent_models(&self) -> Result<Vec<ModelReference>> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {MODEL_COLUMNS} FROM models m WHERE {} ORDER BY last_used_at DESC, id DESC",
                models::NOT_DUPLICATE
            ))
            .map_err(sql_error)?;
        statement
            .query_map([], model_reference)
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }

    fn model(&self, id: ModelId) -> Result<SavedModel> {
        self.connection
            .query_row(
                &format!("SELECT {MODEL_COLUMNS}, path FROM models WHERE id = ?1"),
                [id],
                |row| {
                    Ok(SavedModel {
                        reference: model_reference(row)?,
                        path: PathBuf::from(row.get::<_, String>(6)?),
                    })
                },
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(not_found)
    }

    fn remember_model(
        &mut self,
        path: &Path,
        name: &str,
        settings: ModelSettings,
    ) -> Result<ModelReference> {
        settings.validate()?;
        let path = path
            .to_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::InvalidPath,
                    "The model path could not be saved as Unicode.",
                )
            })?;
        let encoded = serde_json::to_string(&settings).map_err(|_| storage_error())?;
        self.connection
            .query_row(
                "INSERT INTO models (path, file_name, settings, last_used_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                    file_name = excluded.file_name,
                    last_used_at = max(models.last_used_at, excluded.last_used_at)
                 RETURNING id, file_name, settings, bundled_resource IS NOT NULL,
                    settings_mode = 'automatic', artifact_key",
                params![path, name, encoded, unix_ms()?],
                model_reference,
            )
            .map_err(sql_error)
    }

    fn save_model_settings(&mut self, id: ModelId, settings: ModelSettings) -> Result<()> {
        settings.validate()?;
        let encoded = serde_json::to_string(&settings).map_err(|_| storage_error())?;
        updated(self.connection.execute(
            "UPDATE models SET settings = ?1 WHERE id = ?2",
            params![encoded, id],
        ))
    }

    fn mark_model_used(&mut self, id: ModelId) -> Result<()> {
        updated(self.connection.execute(
            "UPDATE models SET last_used_at = max(last_used_at, ?1) WHERE id = ?2",
            params![unix_ms()?, id],
        ))
    }
}

fn read_conversation(connection: &Connection, id: ConversationId) -> Result<Conversation> {
    let mut conversation = connection
        .query_row(
            "SELECT title, system_prompt, created_at, updated_at FROM conversations WHERE id = ?1",
            [id],
            |row| {
                Ok(Conversation {
                    id,
                    title: row.get(0)?,
                    system_prompt: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    messages: Vec::new(),
                })
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(not_found)?;
    let mut statement = connection
        .prepare(
            "SELECT m.id, m.position, m.role, m.state, m.content, m.created_at, m.stats,
                    CASE WHEN x.message_id IS NOT NULL THEN coalesce(x.metadata,'{}') END, r.metadata,
                    m.classification,
                    EXISTS(SELECT 1 FROM conversation_agent_turns l WHERE l.assistant_message_id=m.id)
             FROM messages m LEFT JOIN chat_context_attempts x ON x.message_id=m.id
             LEFT JOIN chat_route_coverage r ON r.message_id=m.id
             WHERE m.conversation_id = ?1 ORDER BY m.position",
        )
        .map_err(sql_error)?;
    conversation.messages = statement
        .query_map([id], read_message)
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)?;
    Ok(conversation)
}

fn read_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    let role = match row.get::<_, String>(2)?.as_str() {
        "system" => MessageRole::System,
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let state = match row.get::<_, String>(3)?.as_str() {
        "streaming" => GenerationState::Streaming,
        "complete" => GenerationState::Complete,
        "cancelled" => GenerationState::Cancelled,
        "interrupted" => GenerationState::Interrupted,
        "failed" => GenerationState::Failed,
        "truncated" => GenerationState::Truncated,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let stats = row
        .get::<_, Option<String>>(6)?
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(Message {
        id: row.get(0)?,
        position: row.get(1)?,
        role,
        state,
        content: row.get(4)?,
        labels: intelligence::classify_message(
            row.get::<_, Option<String>>(9)?.as_deref(),
            role,
            row.get::<_, Option<String>>(7)?.is_some(),
            row.get(10)?,
        )?,
        created_at: row.get(5)?,
        stats,
        context: row
            .get::<_, Option<String>>(7)?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        route_coverage: row
            .get::<_, Option<String>>(8)
            .ok()
            .flatten()
            .as_deref()
            .and_then(|metadata| serde_json::from_str(metadata).ok()),
    })
}

fn next_position(conversation: &Conversation) -> Result<i64> {
    conversation.messages.last().map_or(Ok(0), |message| {
        message.position.checked_add(1).ok_or_else(storage_error)
    })
}

fn insert_message(
    connection: &Connection,
    conversation_id: ConversationId,
    position: i64,
    role: MessageRole,
    state: GenerationState,
    content: String,
    created_at: i64,
) -> Result<Message> {
    let labels = if role == MessageRole::User {
        Labels::from_source(USER_INPUT)
    } else {
        Labels::default()
    };
    let classification = (!labels.is_empty())
        .then(|| serde_json::to_string(&labels))
        .transpose()
        .map_err(|_| storage_error())?;
    let id = connection
        .query_row(
            "INSERT INTO messages (conversation_id, position, role, state, content, created_at, classification)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
            params![
                conversation_id,
                position,
                role.as_str(),
                stored_state(state),
                content,
                created_at,
                classification
            ],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    Ok(Message {
        id,
        position,
        role,
        state,
        content,
        labels,
        created_at,
        stats: None,
        context: None,
        route_coverage: None,
    })
}

fn stored_state(state: GenerationState) -> &'static str {
    match state {
        GenerationState::Streaming => "streaming",
        GenerationState::Complete => "complete",
        GenerationState::Cancelled => "cancelled",
        GenerationState::Interrupted => "interrupted",
        GenerationState::Failed => "failed",
        GenerationState::Truncated => "truncated",
    }
}

fn touch_conversation(connection: &Connection, id: ConversationId, now: i64) -> Result<()> {
    updated(connection.execute(
        "UPDATE conversations SET updated_at = max(updated_at, ?1) WHERE id = ?2",
        params![now, id],
    ))
}

fn model_reference(row: &Row<'_>) -> rusqlite::Result<ModelReference> {
    Ok(ModelReference {
        id: row.get(0)?,
        file_name: row.get(1)?,
        settings: json_column(row, 2)?,
        bundled: row.get(3)?,
        automatic_settings: row.get(4)?,
        artifact: row.get(5)?,
    })
}

fn json_column<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let encoded: String = row.get(index)?;
    serde_json::from_str(&encoded).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn unix_ms() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| storage_error())?;
    i64::try_from(elapsed.as_millis()).map_err(|_| storage_error())
}

fn updated(rows: rusqlite::Result<usize>) -> Result<()> {
    if rows.map_err(sql_error)? == 0 {
        return Err(not_found());
    }
    Ok(())
}

fn sql_error(error: rusqlite::Error) -> InferenceError {
    if matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    ) {
        return InferenceError::new(
            ErrorCode::Busy,
            "The chat database is busy. Close any other app instance and try again.",
        );
    }
    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
        return InferenceError::new(ErrorCode::Storage, "The database disk is full.");
    }
    storage_error()
}

fn storage_error() -> InferenceError {
    InferenceError::new(
        ErrorCode::Storage,
        "The local chat database operation failed.",
    )
}

fn not_found() -> InferenceError {
    InferenceError::new(
        ErrorCode::NotFound,
        "The requested local item no longer exists.",
    )
}

fn active_generation() -> InferenceError {
    InferenceError::new(
        ErrorCode::Busy,
        "Finish or stop the current response first.",
    )
}

fn inactive_generation() -> InferenceError {
    InferenceError::new(ErrorCode::NotFound, "The response is no longer generating.")
}

#[cfg(test)]
mod tests;
