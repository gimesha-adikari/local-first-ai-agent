-- Web research configuration (Stage 04-C). Search provider selection and
-- non-secret endpoint settings only; API keys live in secure storage.
-- Raw search results and page text are never stored.
CREATE TABLE web_research_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    settings TEXT NOT NULL CHECK (json_valid(settings))
) STRICT;
