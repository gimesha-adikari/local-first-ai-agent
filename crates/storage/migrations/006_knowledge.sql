CREATE TABLE knowledge_documents (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id BETWEEN 1 AND 9007199254740991),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    format TEXT NOT NULL CHECK (format IN ('txt', 'markdown', 'pdf')),
    fingerprint BLOB NOT NULL UNIQUE CHECK (length(fingerprint) = 32),
    file_bytes INTEGER NOT NULL CHECK (file_bytes > 0),
    text_bytes INTEGER NOT NULL CHECK (text_bytes BETWEEN 1 AND 1048576),
    chunk_count INTEGER NOT NULL CHECK (chunk_count BETWEEN 1 AND 1024),
    indexing_status TEXT NOT NULL CHECK (indexing_status IN ('ready', 'partial')),
    page_count INTEGER,
    pages_examined INTEGER,
    pages_without_text INTEGER,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    CHECK (CASE WHEN format = 'pdf' THEN
        page_count IS NOT NULL AND pages_examined IS NOT NULL AND pages_without_text IS NOT NULL
        AND pages_examined BETWEEN 1 AND min(100, page_count)
        AND pages_without_text BETWEEN 0 AND pages_examined - 1
        AND (indexing_status = 'partial') = (pages_examined < page_count OR pages_without_text > 0)
    ELSE
        page_count IS NULL AND pages_examined IS NULL AND pages_without_text IS NULL
        AND indexing_status = 'ready'
    END)
) STRICT;

CREATE TABLE knowledge_chunks (
    id INTEGER PRIMARY KEY,
    document_id INTEGER NOT NULL REFERENCES knowledge_documents(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    page INTEGER,
    start_line INTEGER,
    end_line INTEGER,
    CHECK (CASE WHEN page IS NULL THEN
        start_line IS NOT NULL AND end_line IS NOT NULL
        AND start_line >= 1 AND end_line >= start_line
    ELSE
        page BETWEEN 1 AND 100 AND start_line IS NULL AND end_line IS NULL
    END),
    UNIQUE (document_id, ordinal)
) STRICT;

-- The FTS table stores the text and matches chunk IDs by rowid. The storage
-- adapter maintains both tables in one transaction with trusted_schema off.
CREATE VIRTUAL TABLE knowledge_fts USING fts5(text, tokenize = 'unicode61');
INSERT INTO knowledge_fts(knowledge_fts, rank) VALUES ('secure-delete', 1);
