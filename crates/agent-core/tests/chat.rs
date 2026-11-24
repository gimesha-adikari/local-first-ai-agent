use agent_core::{
    chat::{
        ChatHistory, CompletedTurn, Conversation, GenerationState, Message, MessageRole,
        PromptMessage, fit_context, prepare_history,
    },
    inference::ErrorCode,
};

#[test]
fn keeps_system_and_latest_user_with_the_newest_complete_turns() {
    let history = ChatHistory {
        system: "system".into(),
        turns: (1..=3)
            .map(|i| CompletedTurn {
                user: format!("user {i}"),
                assistant: format!("assistant {i}"),
            })
            .collect(),
    };
    let tokenize = |messages: &[PromptMessage<'_>]| {
        let text = messages
            .iter()
            .map(|m| m.content)
            .collect::<Vec<_>>()
            .join("|");
        // Simulate two delimiter tokens per message and a generation prompt.
        Ok((
            text,
            messages
                .iter()
                .map(|m| m.content.split_whitespace().count() + 2)
                .sum::<usize>()
                + 1,
        ))
    };
    let (text, omitted) = fit_context(&history, "latest", 21, 6, tokenize).unwrap();
    assert_eq!(text, "system|user 3|assistant 3|latest");
    assert_eq!(omitted, 2);
    let error = fit_context(&history, "latest", 12, 6, tokenize).unwrap_err();
    assert_eq!(error.code, ErrorCode::ContextLimit);
}

fn message(position: i64, role: MessageRole, state: GenerationState, content: &str) -> Message {
    Message {
        id: position + 1,
        position,
        role,
        state,
        content: content.into(),
        created_at: 0,
        stats: None,
    }
}

#[test]
fn history_uses_latest_attempt_and_excludes_incomplete_turns() {
    use GenerationState::{Cancelled, Complete, Streaming};
    use MessageRole::{Assistant, User};
    let conversation = Conversation {
        id: 1,
        title: "test".into(),
        system_prompt: "system".into(),
        created_at: 0,
        updated_at: 0,
        messages: vec![
            message(0, User, Complete, "first"),
            message(1, Assistant, Complete, "old answer"),
            message(2, Assistant, Complete, "replacement"),
            message(3, User, Complete, "second"),
            message(4, Assistant, Cancelled, "partial"),
            message(5, User, Complete, "latest"),
            message(6, Assistant, Complete, "excluded previous attempt"),
            message(7, Assistant, Streaming, ""),
        ],
    };
    let (history, prompt) = prepare_history(&conversation).unwrap();
    assert_eq!(history.system, "system");
    assert_eq!(history.turns.len(), 1);
    assert_eq!(history.turns[0].assistant, "replacement");
    assert_eq!(prompt, "latest");
}
