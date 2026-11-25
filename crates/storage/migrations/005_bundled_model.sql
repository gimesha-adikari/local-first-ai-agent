-- Identify an application-bundled model so its row survives resource path
-- changes. User-imported rows keep NULL and are never rewritten.
ALTER TABLE models ADD COLUMN bundled_resource TEXT
    CHECK (bundled_resource IS NULL OR length(bundled_resource) > 0);

CREATE UNIQUE INDEX models_by_bundled_resource
    ON models(bundled_resource) WHERE bundled_resource IS NOT NULL;
