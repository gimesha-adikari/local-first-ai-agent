//! Shared visual input settings (Stage 04-E). Only language choices are
//! kept; images, recognized text and queries are never written here.

use agent_core::{inference::Result, visual::VisualSettings};
use rusqlite::{OptionalExtension, params};

use crate::{SqliteStore, sql_error, storage_error};

impl SqliteStore {
    pub fn visual_settings(&self) -> Result<VisualSettings> {
        let saved: Option<String> = self
            .connection
            .query_row(
                "SELECT settings FROM visual_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        match saved {
            Some(json) => serde_json::from_str(&json).map_err(|_| storage_error()),
            None => Ok(VisualSettings::default()),
        }
    }

    pub fn save_visual_settings(&mut self, settings: &VisualSettings) -> Result<()> {
        settings.validate()?;
        let json = serde_json::to_string(settings).map_err(|_| storage_error())?;
        self.connection
            .execute(
                "INSERT INTO visual_settings (id, settings) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET settings = excluded.settings",
                params![json],
            )
            .map_err(sql_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_settings_default_validate_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("visual-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut db = SqliteStore::open_test_fixture(&dir.join("chat.sqlite3")).unwrap();
        assert_eq!(db.visual_settings().unwrap(), VisualSettings::default());
        let settings = VisualSettings {
            ocr_languages: vec!["eng".into(), "ara".into()],
            translation_target: "Sinhala".into(),
        };
        db.save_visual_settings(&settings).unwrap();
        assert_eq!(db.visual_settings().unwrap(), settings);
        let invalid = VisualSettings {
            ocr_languages: vec!["../eng".into()],
            ..Default::default()
        };
        assert!(db.save_visual_settings(&invalid).is_err());
        assert_eq!(db.visual_settings().unwrap(), settings);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema_eighteen_attachments_upgrade_as_files_and_settings_start_empty() {
        use crate::attachments::{RetainedAttachment, StoredImage, StoredPassage};
        use agent_core::attachment::{AttachmentKind, AttachmentOrigin, PassageSource};
        let dir = std::env::temp_dir().join(format!("visual-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.sqlite3");
        let snapshot = |conversation| RetainedAttachment {
            public_id: format!("a{}", "ab".repeat(16)),
            conversation_id: conversation,
            name: "photo.png".into(),
            kind: AttachmentKind::Png,
            origin: AttachmentOrigin::File,
            file_bytes: 64,
            fingerprint: [7; 32],
            pages: None,
            passages: vec![StoredPassage {
                source: PassageSource::ImageText,
                text: "Synthetic recognized text.".into(),
            }],
            image: Some(StoredImage {
                bytes: b"\x89PNG\r\n\x1a\nsynthetic".to_vec(),
                width: 4,
                height: 4,
            }),
            recognition: None,
        };
        let conversation = {
            let mut db = SqliteStore::open_test_fixture(&path).unwrap();
            let conversation = agent_core::chat::ChatStore::create(&mut db).unwrap().id;
            db.retain_attachment(&snapshot(conversation)).unwrap();
            db.connection
                .execute_batch(
                    "ALTER TABLE chat_attachments DROP COLUMN origin;
                     ALTER TABLE intelligence_settings ADD COLUMN local_only INTEGER NOT NULL DEFAULT 1 CHECK (local_only IN (0, 1));
                     DROP TABLE visual_settings; PRAGMA user_version = 18;",
                )
                .unwrap();
            conversation
        };
        let db = SqliteStore::open_test_fixture(&path).unwrap();
        assert_eq!(
            db.retained_attachments(conversation).unwrap(),
            vec![snapshot(conversation)]
        );
        assert_eq!(db.visual_settings().unwrap(), VisualSettings::default());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
