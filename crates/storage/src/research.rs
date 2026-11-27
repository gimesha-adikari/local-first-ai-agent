//! Web research configuration. Search results, page text and excerpts are
//! never written here; research metadata saved with a message lives in its
//! attempt context.

use agent_core::{inference::Result, research::WebResearchSettings};
use rusqlite::{OptionalExtension, params};

use crate::{SqliteStore, sql_error, storage_error};

impl SqliteStore {
    pub fn web_research_settings(&self) -> Result<WebResearchSettings> {
        let saved: Option<String> = self
            .connection
            .query_row(
                "SELECT settings FROM web_research_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        match saved {
            Some(json) => serde_json::from_str(&json).map_err(|_| storage_error()),
            None => Ok(WebResearchSettings::default()),
        }
    }

    pub fn save_web_research_settings(&mut self, settings: &WebResearchSettings) -> Result<()> {
        let json = serde_json::to_string(settings).map_err(|_| storage_error())?;
        self.connection
            .execute(
                "INSERT INTO web_research_settings (id, settings) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET settings = excluded.settings",
                params![json],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    /// The saved answer and attempt context of a research message.
    pub fn research_message(
        &self,
        message: agent_core::chat::MessageId,
    ) -> Result<Option<(String, agent_core::chat_context::AttemptContext)>> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT m.content, x.metadata FROM messages m
                 JOIN chat_context_attempts x ON x.message_id = m.id
                 WHERE m.id = ?1 AND m.role = 'assistant'",
                [message],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        row.map(|(content, metadata)| {
            serde_json::from_str(&metadata)
                .map(|context| (content, context))
                .map_err(|_| storage_error())
        })
        .transpose()
    }

    /// Replace a research attempt's saved metadata before generation starts.
    pub fn save_attempt_context(
        &mut self,
        message: agent_core::chat::MessageId,
        context: &agent_core::chat_context::AttemptContext,
    ) -> Result<()> {
        crate::chat_context::save_context(&self.connection, message, context)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use agent_core::research::SearchProviderKind;

    use super::*;

    #[test]
    fn settings_default_and_round_trip_in_the_encrypted_profile() {
        let mut store = SqliteStore::open_test_fixture(Path::new(":memory:")).unwrap();
        assert_eq!(
            store.web_research_settings().unwrap(),
            WebResearchSettings::default()
        );
        let settings = WebResearchSettings {
            provider: Some(SearchProviderKind::Searxng),
            searxng_endpoint: "http://127.0.0.1:8888".into(),
            ..Default::default()
        };
        store.save_web_research_settings(&settings).unwrap();
        assert_eq!(store.web_research_settings().unwrap(), settings);
    }
}
