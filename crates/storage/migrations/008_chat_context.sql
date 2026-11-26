-- Marks answers that can contain document or personal-memory data. Their
-- transcripts remain readable, but are not reused as model context.
CREATE TABLE chat_context_attempts (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE
) STRICT;

-- Deliberately independent of library deletion and revision replacement.
-- No document excerpts are copied into this table.
CREATE TABLE chat_source_references (
    message_id INTEGER NOT NULL REFERENCES chat_context_attempts(message_id) ON DELETE CASCADE,
    slot INTEGER NOT NULL CHECK (slot BETWEEN 0 AND 3),
    document_id INTEGER NOT NULL CHECK (document_id > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    chunk_ordinal INTEGER NOT NULL CHECK (chunk_ordinal >= 0),
    document_name TEXT NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY (message_id, slot)
) STRICT, WITHOUT ROWID;

CREATE TABLE memory_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
) STRICT;
INSERT INTO memory_settings VALUES (1, 0);

CREATE TABLE personal_memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id BETWEEN 1 AND 9007199254740991),
    content TEXT NOT NULL CHECK (length(CAST(content AS BLOB)) BETWEEN 1 AND 1024),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at)
) STRICT;
