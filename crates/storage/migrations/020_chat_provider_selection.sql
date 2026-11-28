-- Preserve the effective Chat target. A previously suppressed provider
-- stays configured and must be selected again before it can receive a turn.
UPDATE intelligence_settings SET active_provider = NULL WHERE local_only = 1;
ALTER TABLE intelligence_settings DROP COLUMN local_only;
