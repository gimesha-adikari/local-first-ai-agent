-- Public artifacts live in private application storage. This local journal
-- stays with the encrypted profile; URL queries never enter it.
CREATE TABLE model_downloads (
    artifact_key TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    phase TEXT NOT NULL CHECK (phase IN (
        'transferring', 'ready', 'interrupted', 'failed', 'incompatible', 'installed', 'removed'
    )),
    authorized_at INTEGER NOT NULL,
    verified_at INTEGER,
    destination TEXT,
    representation TEXT,
    etag TEXT,
    error TEXT,
    CHECK ((representation IS NULL) = (etag IS NULL))
) STRICT;
