CREATE TABLE chat_attempt_replay (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    model_id INTEGER NOT NULL REFERENCES models(id),
    settings TEXT NOT NULL,
    continuation_count INTEGER NOT NULL DEFAULT 0 CHECK (continuation_count BETWEEN 0 AND 3),
    replay TEXT CHECK (replay IS NULL OR length(replay) <= 131072)
) STRICT;
