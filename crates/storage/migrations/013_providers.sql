-- Remote and self-hosted providers. No credential is stored here; secure
-- storage holds it under the provider key and canonical origin.
CREATE TABLE providers (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) BETWEEN 1 AND 64),
    settings TEXT NOT NULL CHECK (length(settings) BETWEEN 2 AND 16384),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Which intelligence answers Chat. Existing profiles stay local-only.
CREATE TABLE intelligence_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    local_only INTEGER NOT NULL DEFAULT 1 CHECK (local_only IN (0, 1)),
    active_provider INTEGER REFERENCES providers(id) ON DELETE SET NULL
) STRICT;
INSERT INTO intelligence_settings (id) VALUES (1);

-- Usage ledger. A reservation is written before admission and settled
-- from the transport record and the provider's usage report afterwards.
CREATE TABLE provider_usage (
    id INTEGER PRIMARY KEY,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    period TEXT NOT NULL CHECK (length(period) = 7),
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('reserved', 'settled')),
    reserved_tokens INTEGER NOT NULL CHECK (reserved_tokens >= 0),
    reserved_cost INTEGER CHECK (reserved_cost IS NULL OR reserved_cost >= 0),
    transmission TEXT CHECK (transmission IS NULL OR transmission IN ('unsent', 'sent', 'unknown')),
    input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
    output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
    usage_basis TEXT CHECK (usage_basis IS NULL OR usage_basis IN ('provider_reported', 'estimated', 'none')),
    charged_tokens INTEGER CHECK (charged_tokens IS NULL OR charged_tokens >= 0),
    charged_cost INTEGER CHECK (charged_cost IS NULL OR charged_cost >= 0)
) STRICT;
CREATE INDEX provider_usage_period ON provider_usage(provider_id, period);

-- Verified local model imports: curated manifest identity or an explicit
-- unverified marker. NULL artifact means not matched to the manifest.
ALTER TABLE models ADD COLUMN artifact_key TEXT
    CHECK (artifact_key IS NULL OR length(artifact_key) BETWEEN 1 AND 128);

-- Rows already verified by full digest match manifest version 1.
UPDATE models SET artifact_key = 'lfm2.5-1.2b-instruct-q4_k_m'
    WHERE content_sha256 = 'b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5';
UPDATE models SET artifact_key = 'bge-small-en-v1.5-f16'
    WHERE content_sha256 = 'f0b2fef971e8366438bfd2d9aefea1b0115919389448806d290237f638bae999';
