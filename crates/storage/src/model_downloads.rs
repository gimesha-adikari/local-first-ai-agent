//! Persistent local download state and atomic model registration.

use std::path::Path;

use agent_core::{
    chat::{ModelId, ModelSettings},
    inference::Result,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{SqliteStore, sql_error, storage_error, unix_ms};

#[derive(Debug)]
pub struct DownloadRecord {
    pub phase: String,
    pub representation: Option<(String, String)>,
    pub error: Option<String>,
}

pub(crate) fn settle_interrupted(connection: &Connection) -> Result<()> {
    connection
        .execute(
            "UPDATE model_downloads SET phase = 'interrupted' WHERE phase = 'transferring'",
            [],
        )
        .map_err(sql_error)?;
    Ok(())
}

impl SqliteStore {
    pub fn model_download(&self, key: &str, sha256: &str) -> Result<Option<DownloadRecord>> {
        self.connection
            .query_row(
                "SELECT phase, representation, etag, error FROM model_downloads
             WHERE artifact_key = ?1 AND sha256 = ?2",
                params![key, sha256],
                |row| {
                    Ok(DownloadRecord {
                        phase: row.get(0)?,
                        representation: row.get::<_, Option<String>>(1)?.zip(row.get(2)?),
                        error: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(sql_error)
    }

    pub fn begin_model_download(&mut self, key: &str, sha256: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO model_downloads(artifact_key, sha256, phase, authorized_at)
             VALUES (?1, ?2, 'transferring', ?3)
             ON CONFLICT(artifact_key) DO UPDATE SET
               representation = CASE WHEN sha256 = excluded.sha256 THEN representation END,
               etag = CASE WHEN sha256 = excluded.sha256 THEN etag END,
               sha256 = excluded.sha256, phase = excluded.phase, authorized_at = excluded.authorized_at,
               destination = NULL, verified_at = NULL, error = NULL",
            params![key, sha256, unix_ms()?],
        ).map_err(sql_error)?;
        Ok(())
    }

    pub fn set_model_download_destination(&mut self, key: &str, host: &str) -> Result<()> {
        self.connection
            .execute(
                "UPDATE model_downloads SET destination = ?2 WHERE artifact_key = ?1",
                params![key, host],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn set_model_download_representation(
        &mut self,
        key: &str,
        value: Option<(&str, &str)>,
    ) -> Result<()> {
        let (fingerprint, etag) = value.unzip();
        self.connection
            .execute(
                "UPDATE model_downloads SET representation = ?2, etag = ?3 WHERE artifact_key = ?1",
                params![key, fingerprint, etag],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn set_model_download_phase(
        &mut self,
        key: &str,
        phase: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.connection
            .execute(
                "UPDATE model_downloads SET phase = ?2, error = ?3 WHERE artifact_key = ?1",
                params![key, phase, error],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn model_at_path(&self, path: &Path) -> Result<Option<ModelId>> {
        self.connection
            .query_row(
                "SELECT id FROM models WHERE path = ?1",
                [path.to_str().ok_or_else(storage_error)?],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)
    }

    /// Register an inspected, installed artifact and commit its journal entry
    /// together. A relocated missing row keeps its id and settings.
    pub fn finish_model_download(
        &mut self,
        identity: (&str, &str),
        chat: Option<ManagedChatModel<'_>>,
    ) -> Result<()> {
        let (artifact, digest) = identity;
        let tx = self.connection.transaction().map_err(sql_error)?;
        if let Some(chat) = chat {
            let path = chat.path.to_str().ok_or_else(storage_error)?;
            if let Some(id) = chat.reuse {
                tx.execute(
                    "UPDATE model_runtime SET chat_unloaded = 1 WHERE last_model_id = ?1
                     AND EXISTS (SELECT 1 FROM models WHERE id = ?1 AND path <> ?2)",
                    params![id, path],
                )
                .map_err(sql_error)?;
                tx.execute(
                    "UPDATE models SET path = ?2, file_name = ?3, bundled_resource = NULL
                     WHERE id = ?1",
                    params![id, path, chat.file_name],
                )
                .map_err(sql_error)?;
            }
            let settings = serde_json::to_string(&chat.settings).map_err(|_| storage_error())?;
            tx.execute(
                "INSERT INTO models(path, file_name, settings, last_used_at, content_key, content_sha256, artifact_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET file_name = excluded.file_name,
                   content_key = excluded.content_key, content_sha256 = excluded.content_sha256,
                   artifact_key = excluded.artifact_key",
                params![path, chat.file_name, settings, unix_ms()?, chat.content_key, digest, artifact],
            ).map_err(sql_error)?;
        }
        crate::updated(tx.execute(
            "UPDATE model_downloads SET phase = 'installed', verified_at = ?3, error = NULL,
               representation = NULL, etag = NULL WHERE artifact_key = ?1 AND sha256 = ?2",
            params![artifact, digest, unix_ms()?],
        ))?;
        tx.commit().map_err(sql_error)
    }

    /// Close runtime restore references before removing the managed file.
    /// Historical attempts and custom model settings keep their existing row.
    pub fn remove_model_download(&mut self, key: &str, path: &Path) -> Result<()> {
        let tx = self.connection.transaction().map_err(sql_error)?;
        let path = path.to_str().ok_or_else(storage_error)?;
        tx.execute(
            "UPDATE model_runtime SET chat_unloaded = 1
             WHERE last_model_id IN (SELECT id FROM models WHERE path = ?1)",
            [path],
        )
        .map_err(sql_error)?;
        tx.execute(
            "UPDATE model_runtime SET embedding_path = NULL, embedding_restore = 0 WHERE embedding_path = ?1",
            [path],
        ).map_err(sql_error)?;
        tx.execute(
            "UPDATE model_downloads SET phase = 'removed', representation = NULL, etag = NULL, error = NULL
             WHERE artifact_key = ?1", [key],
        ).map_err(sql_error)?;
        tx.commit().map_err(sql_error)
    }
}

pub struct ManagedChatModel<'a> {
    pub path: &'a Path,
    pub file_name: &'a str,
    pub settings: ModelSettings,
    pub content_key: &'a str,
    pub reuse: Option<ModelId>,
}
