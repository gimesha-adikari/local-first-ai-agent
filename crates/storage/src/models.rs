//! Model identity, automatic settings and remembered runtime choices.

use std::path::{Path, PathBuf};

use agent_core::{
    chat::{ModelId, ModelReference, ModelSettings},
    inference::{ErrorCode, InferenceError, Result},
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{MODEL_COLUMNS, SqliteStore, model_reference, sql_error, storage_error, updated};

/// A row is listed unless another row has the same full file digest and is
/// preferred: the bundled row first, then the most recently used.
pub(crate) const NOT_DUPLICATE: &str = "NOT EXISTS (SELECT 1 FROM models o
    WHERE o.id <> m.id AND o.content_sha256 = m.content_sha256 AND (
        (o.bundled_resource IS NOT NULL) > (m.bundled_resource IS NOT NULL)
        OR ((o.bundled_resource IS NOT NULL) = (m.bundled_resource IS NOT NULL)
            AND (o.last_used_at > m.last_used_at
                OR (o.last_used_at = m.last_used_at AND o.id > m.id)))))";

/// Contexts the recommendation can choose. Saved settings equal to one of
/// these recommendations were never customized.
const RECOMMENDED_CONTEXTS: [u32; 3] = [2048, 4096, 8192];

pub fn is_recommended(settings: ModelSettings) -> bool {
    RECOMMENDED_CONTEXTS.iter().any(|&context| {
        let mut recommended = ModelSettings::recommended(context);
        recommended.enable_thinking = settings.enable_thinking;
        recommended == settings
    })
}

/// Schema 10 upgrade: rows that still hold a recommendation become automatic.
pub(crate) fn classify_existing_settings(connection: &Connection) -> Result<()> {
    let mut stmt = connection
        .prepare("SELECT id, settings FROM models")
        .map_err(sql_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, ModelId>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    for (id, encoded) in rows {
        // Settings that cannot be read as current settings are kept as custom.
        let recommended = serde_json::from_str::<ModelSettings>(&encoded).is_ok_and(is_recommended);
        let mode = if recommended { "automatic" } else { "custom" };
        connection
            .execute(
                "UPDATE models SET settings_mode = ?1 WHERE id = ?2",
                params![mode, id],
            )
            .map_err(sql_error)?;
    }
    Ok(())
}

impl SqliteStore {
    /// Rows whose content identity is unknown, with their saved paths.
    pub fn models_without_content_key(&self) -> Result<Vec<(ModelId, PathBuf)>> {
        let mut stmt = self
            .connection
            .prepare("SELECT id, path FROM models WHERE content_key IS NULL ORDER BY id")
            .map_err(sql_error)?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?)))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)
    }

    pub fn set_model_content_key(&mut self, id: ModelId, key: &str) -> Result<()> {
        validate_key(key)?;
        updated(self.connection.execute(
            "UPDATE models SET content_key = ?1 WHERE id = ?2",
            params![key, id],
        ))
    }

    /// Rows with this sampled identity: candidates for the same weights.
    pub fn models_with_content_key(
        &self,
        key: &str,
    ) -> Result<Vec<(ModelId, PathBuf, Option<String>)>> {
        validate_key(key)?;
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, path, content_sha256 FROM models WHERE content_key = ?1 ORDER BY id",
            )
            .map_err(sql_error)?;
        stmt.query_map([key], |row| {
            Ok((
                row.get(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
                row.get(2)?,
            ))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)
    }

    /// Rows without a full digest whose sampled identity matches another row.
    pub fn duplicate_candidates(&self) -> Result<Vec<(ModelId, PathBuf)>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT m.id, m.path FROM models m WHERE m.content_sha256 IS NULL AND m.content_key IS NOT NULL
                 AND EXISTS (SELECT 1 FROM models o WHERE o.id <> m.id AND o.content_key = m.content_key)
                 ORDER BY m.id",
            )
            .map_err(sql_error)?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?)))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)
    }

    pub fn set_model_sha256(&mut self, id: ModelId, sha256: &str) -> Result<()> {
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "The model digest is invalid.",
            ));
        }
        updated(self.connection.execute(
            "UPDATE models SET content_sha256 = ?1 WHERE id = ?2",
            params![sha256.to_ascii_lowercase(), id],
        ))
    }

    /// The listed row whose file has this full digest, if any.
    pub fn model_with_sha256(&self, sha256: &str) -> Result<Option<ModelReference>> {
        self.connection
            .query_row(
                &format!(
                    "SELECT {MODEL_COLUMNS} FROM models m WHERE content_sha256 = ?1 AND {NOT_DUPLICATE}"
                ),
                [sha256.to_ascii_lowercase()],
                model_reference,
            )
            .optional()
            .map_err(sql_error)
    }

    /// Remember the last Chat model that loaded successfully.
    pub fn set_last_chat_model(&mut self, id: ModelId) -> Result<()> {
        updated(self.connection.execute(
            "UPDATE model_runtime SET last_model_id = ?1, chat_unloaded = 0 WHERE id = 1",
            [id],
        ))
    }

    /// The listed model to restore at launch: the last loaded model, or the
    /// listed row that replaced it, unless the user explicitly unloaded.
    pub fn chat_model_to_restore(&self) -> Result<Option<crate::SavedModelRow>> {
        let row: Option<(Option<ModelId>, bool)> = self
            .connection
            .query_row(
                "SELECT last_model_id, chat_unloaded FROM model_runtime WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((Some(id), false)) = row else {
            return Ok(None);
        };
        // A hidden copy restores through the listed row with the same digest.
        let listed: Option<ModelId> = self
            .connection
            .query_row(
                &format!(
                    "SELECT m.id FROM models m WHERE {NOT_DUPLICATE} AND (m.id = ?1 OR (m.content_sha256 IS NOT NULL
                     AND m.content_sha256 = (SELECT content_sha256 FROM models WHERE id = ?1)))
                     ORDER BY m.id = ?1 DESC LIMIT 1"
                ),
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        listed
            .map(|id| {
                self.connection
                    .query_row(
                        &format!("SELECT {MODEL_COLUMNS}, path FROM models WHERE id = ?1"),
                        [id],
                        |row| {
                            Ok(crate::SavedModelRow {
                                reference: model_reference(row)?,
                                path: PathBuf::from(row.get::<_, String>(5)?),
                            })
                        },
                    )
                    .map_err(sql_error)
            })
            .transpose()
    }

    /// Collapse rows whose files have the same full digest into the preferred row. The
    /// hidden rows, their files and earlier attempts that reference them stay
    /// intact. When the preferred row is automatic, it adopts the most recently
    /// used custom settings of its duplicates. Returns the hidden row count.
    pub fn merge_duplicate_models(&mut self) -> Result<usize> {
        let tx = self.connection.transaction().map_err(sql_error)?;
        let survivors = {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT m.id, m.content_sha256, m.settings_mode = 'automatic' FROM models m
                     WHERE m.content_sha256 IS NOT NULL AND {NOT_DUPLICATE}
                     AND EXISTS (SELECT 1 FROM models o WHERE o.id <> m.id AND o.content_sha256 = m.content_sha256)"
                ))
                .map_err(sql_error)?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, ModelId>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
        };
        let mut hidden = 0;
        for (id, key, automatic) in survivors {
            hidden += tx
                .query_row(
                    "SELECT count(*) FROM models WHERE content_sha256 = ?1 AND id <> ?2",
                    params![key, id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sql_error)? as usize;
            if automatic {
                let custom: Option<String> = tx
                    .query_row(
                        "SELECT settings FROM models WHERE content_sha256 = ?1 AND id <> ?2
                         AND settings_mode = 'custom' ORDER BY last_used_at DESC, id DESC LIMIT 1",
                        params![key, id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(sql_error)?;
                if let Some(settings) = custom {
                    tx.execute(
                        "UPDATE models SET settings = ?1, settings_mode = 'custom' WHERE id = ?2",
                        params![settings, id],
                    )
                    .map_err(sql_error)?;
                }
            }
            tx.execute(
                "UPDATE models SET last_used_at =
                    (SELECT max(last_used_at) FROM models WHERE content_sha256 = ?1) WHERE id = ?2",
                params![key, id],
            )
            .map_err(sql_error)?;
        }
        tx.commit().map_err(sql_error)?;
        Ok(hidden)
    }

    /// Save settings and whether they follow the recommendation.
    pub fn set_model_settings(
        &mut self,
        id: ModelId,
        settings: ModelSettings,
        automatic: bool,
    ) -> Result<()> {
        settings.validate()?;
        let encoded = serde_json::to_string(&settings).map_err(|_| storage_error())?;
        updated(self.connection.execute(
            "UPDATE models SET settings = ?1, settings_mode = ?2 WHERE id = ?3",
            params![encoded, if automatic { "automatic" } else { "custom" }, id],
        ))
    }

    /// Whether the user explicitly unloaded the Chat model and has not loaded
    /// one since. The bundled model is then not loaded automatically.
    pub fn chat_model_unloaded(&self) -> Result<bool> {
        self.runtime_flag("chat_unloaded")
    }

    pub fn set_chat_model_unloaded(&mut self, unloaded: bool) -> Result<()> {
        updated(self.connection.execute(
            "UPDATE model_runtime SET chat_unloaded = ?1 WHERE id = 1",
            [unloaded],
        ))
    }

    /// The remembered embedding model path and whether to restore it at launch.
    pub fn embedding_model_choice(&self) -> Result<Option<(PathBuf, bool)>> {
        self.connection
            .query_row(
                "SELECT embedding_path, embedding_restore FROM model_runtime WHERE id = 1",
                [],
                |row| {
                    Ok(row
                        .get::<_, Option<String>>(0)?
                        .map(|path| (PathBuf::from(path), row.get::<_, bool>(1).unwrap_or(false))))
                },
            )
            .map_err(sql_error)
    }

    /// Remember an explicitly chosen and verified embedding model.
    pub fn remember_embedding_model(&mut self, path: &Path) -> Result<()> {
        let path = path
            .to_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::InvalidPath,
                    "The embedding model path could not be saved as Unicode.",
                )
            })?;
        updated(self.connection.execute(
            "UPDATE model_runtime SET embedding_path = ?1, embedding_restore = 1 WHERE id = 1",
            [path],
        ))
    }

    /// Keep the remembered path but do not restore it at the next launch.
    pub fn set_embedding_restore(&mut self, restore: bool) -> Result<()> {
        updated(self.connection.execute(
            "UPDATE model_runtime SET embedding_restore = ?1 WHERE id = 1",
            [restore],
        ))
    }

    pub fn forget_embedding_model(&mut self) -> Result<()> {
        updated(self.connection.execute(
            "UPDATE model_runtime SET embedding_path = NULL, embedding_restore = 0 WHERE id = 1",
            [],
        ))
    }

    fn runtime_flag(&self, column: &'static str) -> Result<bool> {
        self.connection
            .query_row(
                &format!("SELECT {column} FROM model_runtime WHERE id = 1"),
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 128 || !key.is_ascii() {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "The model identity is invalid.",
        ));
    }
    Ok(())
}
