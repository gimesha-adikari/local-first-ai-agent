-- Keep the original binding available for retry until secure storage and
-- SQLite both confirm removal, including after an interrupted shutdown.
ALTER TABLE providers ADD COLUMN deletion_pending INTEGER NOT NULL DEFAULT 0
    CHECK (deletion_pending IN (0, 1));

-- One obsolete origin can be retained per provider while key cleanup is retried.
ALTER TABLE providers ADD COLUMN obsolete_credential_origin TEXT;
