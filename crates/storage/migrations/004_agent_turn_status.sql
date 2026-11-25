ALTER TABLE conversation_agent_turns ADD COLUMN excluded_turns INTEGER
    CHECK (excluded_turns >= 0);
