-- NULL marks a record from a writer that has no fitted classification.
-- Resolve legacy provenance using the existing role and context records.
ALTER TABLE messages ADD COLUMN classification TEXT;

CREATE TABLE intelligence_receipts (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    receipt TEXT NOT NULL
) STRICT;

CREATE INDEX intelligence_receipts_message ON intelligence_receipts(message_id);
