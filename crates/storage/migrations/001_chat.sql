CREATE TABLE conversations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 100),
    system_prompt TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at)
) STRICT;

CREATE INDEX conversations_by_activity
    ON conversations(updated_at DESC, id DESC);

CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    role TEXT NOT NULL CHECK (role IN ('system', 'user', 'assistant')),
    state TEXT NOT NULL CHECK (state IN (
        'streaming', 'complete', 'cancelled', 'interrupted', 'failed', 'truncated'
    )),
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    stats TEXT,
    CHECK (role = 'assistant' OR (state = 'complete' AND stats IS NULL)),
    CHECK (state <> 'streaming' OR stats IS NULL),
    UNIQUE (conversation_id, position)
) STRICT;

CREATE UNIQUE INDEX messages_single_generation
    ON messages(state) WHERE state = 'streaming';

CREATE TABLE models (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE CHECK (length(path) > 0),
    file_name TEXT NOT NULL CHECK (length(file_name) > 0),
    settings TEXT NOT NULL,
    last_used_at INTEGER NOT NULL CHECK (last_used_at >= 0)
) STRICT;

CREATE INDEX models_by_last_use
    ON models(last_used_at DESC, id DESC);

CREATE TABLE application_preferences (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    preferences TEXT NOT NULL
) STRICT;
