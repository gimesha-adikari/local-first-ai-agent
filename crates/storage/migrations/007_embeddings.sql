ALTER TABLE knowledge_documents ADD COLUMN revision INTEGER NOT NULL DEFAULT 1
    CHECK (revision > 0);
CREATE UNIQUE INDEX knowledge_document_revision ON knowledge_documents(id, revision);
CREATE UNIQUE INDEX knowledge_chunk_owner ON knowledge_chunks(document_id, id);

-- A row with all its windows is published together. The ID allows one active
-- generation per document while retaining the exact model/preprocessing key.
CREATE TABLE knowledge_embedding_indexes (
    document_id INTEGER PRIMARY KEY,
    revision INTEGER NOT NULL,
    model_fingerprint BLOB NOT NULL CHECK (length(model_fingerprint) = 32),
    preprocessing INTEGER NOT NULL CHECK (preprocessing > 0),
    window_count INTEGER NOT NULL CHECK (window_count BETWEEN 1 AND 4096),
    FOREIGN KEY (document_id, revision)
        REFERENCES knowledge_documents(id, revision) ON DELETE CASCADE
) STRICT;

CREATE TABLE knowledge_embedding_windows (
    document_id INTEGER NOT NULL REFERENCES knowledge_embedding_indexes(document_id)
        ON DELETE CASCADE,
    chunk_id INTEGER NOT NULL,
    token_start INTEGER NOT NULL CHECK (token_start >= 0),
    token_end INTEGER NOT NULL CHECK (token_end > token_start AND token_end <= token_start + 510),
    vector BLOB NOT NULL CHECK (length(vector) = 1536),
    PRIMARY KEY (document_id, chunk_id, token_start),
    FOREIGN KEY (document_id, chunk_id)
        REFERENCES knowledge_chunks(document_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
