-- Routing coverage of ordinary attempts (no document evidence). Kept apart from
-- chat_context_attempts so the answer stays ordinary Chat for history and Continue.
CREATE TABLE chat_route_coverage (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    metadata TEXT NOT NULL
) STRICT;

-- Full SHA-256 of the model file. The sampled content_key only nominates
-- candidates; rows are consolidated only when these digests match.
ALTER TABLE models ADD COLUMN content_sha256 TEXT
    CHECK (content_sha256 IS NULL OR length(content_sha256) = 64);

-- The last Chat model loaded successfully, restored at launch unless the user
-- explicitly unloaded it.
ALTER TABLE model_runtime ADD COLUMN last_model_id INTEGER;
