-- Attachments the user explicitly chose to keep with a conversation
-- (Stage 04-D). Extracted text with its coverage, and for images only a
-- validated, metadata-stripped representation. Original document files are
-- never copied. Temporary attachments are never written here.
CREATE TABLE chat_attachments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 33),
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 320),
    kind TEXT NOT NULL CHECK (kind IN ('text', 'markdown', 'pdf', 'png', 'jpeg')),
    file_bytes INTEGER NOT NULL CHECK (file_bytes > 0),
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    pages TEXT CHECK (pages IS NULL OR json_valid(pages)),
    passages TEXT NOT NULL CHECK (json_valid(passages)),
    image BLOB,
    image_width INTEGER CHECK (image_width IS NULL OR image_width > 0),
    image_height INTEGER CHECK (image_height IS NULL OR image_height > 0),
    recognition TEXT,
    created_at INTEGER NOT NULL CHECK (created_at >= 0)
) STRICT;

CREATE INDEX chat_attachments_by_conversation ON chat_attachments(conversation_id, id);
