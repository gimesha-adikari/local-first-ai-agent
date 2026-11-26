-- Automatic settings follow the recommendation for the model and hardware on
-- every load. Custom settings are kept exactly as the user saved them.
ALTER TABLE models ADD COLUMN settings_mode TEXT NOT NULL DEFAULT 'automatic'
    CHECK (settings_mode IN ('automatic', 'custom'));

-- Sampled content identity (size and digest of the first and last MiB). Rows
-- with the same identity are the same weights at different paths. NULL until
-- the file has been inspected. Original files are never modified.
ALTER TABLE models ADD COLUMN content_key TEXT
    CHECK (content_key IS NULL OR length(content_key) BETWEEN 1 AND 128);

-- Remembered runtime choices. The embedding path stays in Rust and is
-- reverified by checksum before every restore.
CREATE TABLE model_runtime (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    chat_unloaded INTEGER NOT NULL DEFAULT 0 CHECK (chat_unloaded IN (0, 1)),
    embedding_path TEXT CHECK (embedding_path IS NULL OR length(embedding_path) > 0),
    embedding_restore INTEGER NOT NULL DEFAULT 0 CHECK (embedding_restore IN (0, 1))
) STRICT;
INSERT INTO model_runtime (id) VALUES (1);
