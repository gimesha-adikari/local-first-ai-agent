-- Schema 8 already identified contextual attempts and stored cited passages.
-- Older attempts retain their references and have unknown historical coverage.
ALTER TABLE chat_context_attempts ADD COLUMN metadata TEXT;
ALTER TABLE chat_source_references ADD COLUMN cited INTEGER NOT NULL DEFAULT 1
    CHECK (cited IN (0,1));
