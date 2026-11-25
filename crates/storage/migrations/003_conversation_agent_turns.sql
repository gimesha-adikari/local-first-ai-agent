ALTER TABLE messages ADD COLUMN model_name TEXT;

CREATE UNIQUE INDEX messages_by_conversation_and_id ON messages(conversation_id, id);

CREATE TABLE conversation_agent_turns (
    task_id INTEGER PRIMARY KEY REFERENCES agent_tasks(id) ON DELETE CASCADE,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_message_id INTEGER NOT NULL UNIQUE,
    assistant_message_id INTEGER NOT NULL UNIQUE,
    task_timeout_ms INTEGER NOT NULL CHECK (task_timeout_ms BETWEEN 1000 AND 600000),
    CHECK (user_message_id < assistant_message_id),
    FOREIGN KEY (conversation_id, user_message_id) REFERENCES messages(conversation_id, id) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id, assistant_message_id) REFERENCES messages(conversation_id, id) ON DELETE CASCADE
) STRICT;

CREATE INDEX agent_turns_by_conversation
    ON conversation_agent_turns(conversation_id, user_message_id);
