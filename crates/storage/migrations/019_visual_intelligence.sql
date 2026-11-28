-- Stage 04-E: camera provenance for kept attachments and the shared visual
-- input settings (OCR languages and the last translation target). No image,
-- recognized text or query is stored here.
ALTER TABLE chat_attachments
    ADD COLUMN origin TEXT NOT NULL DEFAULT 'file' CHECK (origin IN ('file', 'camera'));

CREATE TABLE visual_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    settings TEXT NOT NULL CHECK (json_valid(settings))
) STRICT;
