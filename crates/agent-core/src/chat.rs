//! Saved conversations, generation state, and storage and inference boundaries.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::inference::{
    ErrorCode, FinishReason, GenerationRequest, GenerationStats, InferenceError, ModelConfig,
    Result,
};

pub type ConversationId = i64;
pub type MessageId = i64;
pub type ModelId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    Streaming,
    Complete,
    Cancelled,
    Interrupted,
    Failed,
    Truncated,
}

impl GenerationState {
    pub fn from_output(reason: FinishReason, content: &str) -> Self {
        match reason {
            FinishReason::EndOfGeneration if !assistant_answer(content).trim().is_empty() => {
                Self::Complete
            }
            FinishReason::EndOfGeneration => Self::Failed,
            FinishReason::TokenLimit => Self::Truncated,
            FinishReason::Cancelled => Self::Cancelled,
        }
    }
}

pub(crate) fn assistant_answer(content: &str) -> &str {
    let trimmed = content.trim_start();
    if let Some(reasoning) = trimmed.strip_prefix("<think>") {
        return reasoning
            .split_once("</think>")
            .map_or("", |(_, answer)| answer);
    }
    if trimmed.starts_with("<th") && "<think>".starts_with(trimmed.trim_end()) {
        return "";
    }
    trimmed
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: ConversationId,
    pub title: String,
    pub system_prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: MessageId,
    pub position: i64,
    pub role: MessageRole,
    pub state: GenerationState,
    pub content: String,
    pub created_at: i64,
    pub stats: Option<GenerationStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Box<crate::chat_context::AttemptContext>>,
    /// How an ordinary attempt with selected documents was routed (Task 06-C).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_coverage: Option<Box<crate::chat_context::KnowledgeCoverage>>,
}

pub enum TurnAction {
    Send(String),
    Regenerate,
}

#[derive(Debug)]
pub struct PendingTurn {
    pub user: Option<Message>,
    pub assistant: Message,
    pub history: ChatHistory,
    pub prompt: String,
}

#[derive(Debug, Clone, Default)]
pub struct ChatHistory {
    pub system: String,
    pub turns: Vec<CompletedTurn>,
}

#[derive(Debug, Clone)]
pub struct CompletedTurn {
    pub user: String,
    pub assistant: String,
}

#[derive(Debug, Clone, Copy)]
pub struct PromptMessage<'a> {
    pub role: MessageRole,
    pub content: &'a str,
}

/// Older turns enter context only when their latest assistant attempt completed.
pub fn prepare_history(conversation: &Conversation) -> Result<(ChatHistory, String)> {
    let latest = conversation
        .messages
        .iter()
        .rposition(|message| message.role == MessageRole::User)
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::InvalidConfig,
                "Send a message before regenerating.",
            )
        })?;
    let mut turns = Vec::new();
    let mut user: Option<&Message> = None;
    let mut assistant: Option<&Message> = None;
    for message in &conversation.messages[..latest] {
        match message.role {
            MessageRole::User => {
                remember_turn(&mut turns, user.take(), assistant.take());
                user = Some(message);
            }
            MessageRole::Assistant => assistant = Some(message),
            MessageRole::System => {}
        }
    }
    remember_turn(&mut turns, user, assistant);
    Ok((
        ChatHistory {
            system: conversation.system_prompt.clone(),
            turns,
        },
        conversation.messages[latest].content.clone(),
    ))
}

fn remember_turn(
    turns: &mut Vec<CompletedTurn>,
    user: Option<&Message>,
    assistant: Option<&Message>,
) {
    if let (Some(user), Some(assistant)) = (user, assistant)
        && assistant.state == GenerationState::Complete
        && !assistant_answer(&assistant.content).trim().is_empty()
    {
        turns.push(CompletedTurn {
            user: user.content.clone(),
            assistant: assistant.content.clone(),
        });
    }
}

const MAX_FORMATTED_SOURCE_BYTES: usize = 512 * 1024;

/// Pick a contiguous suffix of complete turns using the formatted token count.
pub fn fit_context<T>(
    history: &ChatHistory,
    latest: &str,
    context: u32,
    output: u32,
    mut measure: impl FnMut(&[PromptMessage<'_>]) -> Result<(T, usize)>,
) -> Result<(T, usize)> {
    let available = context.saturating_sub(output) as usize;
    let mut bytes = history.system.len().saturating_add(latest.len());
    let mut candidates = 0_usize;
    for turn in history.turns.iter().rev() {
        bytes = bytes
            .saturating_add(turn.user.len())
            .saturating_add(turn.assistant.len());
        if bytes > MAX_FORMATTED_SOURCE_BYTES {
            break;
        }
        candidates += 1;
    }
    let mut with_turns = |count| {
        let mut messages = Vec::with_capacity(2 * count + 2);
        if !history.system.trim().is_empty() {
            messages.push(PromptMessage {
                role: MessageRole::System,
                content: &history.system,
            });
        }
        for turn in &history.turns[history.turns.len() - count..] {
            messages.push(PromptMessage {
                role: MessageRole::User,
                content: &turn.user,
            });
            messages.push(PromptMessage {
                role: MessageRole::Assistant,
                content: &turn.assistant,
            });
        }
        messages.push(PromptMessage {
            role: MessageRole::User,
            content: latest,
        });
        measure(&messages)
    };

    let (mut best, minimum) = with_turns(0)?;
    if minimum == 0 || minimum > available {
        return Err(InferenceError::new(
            ErrorCode::ContextLimit,
            "The system instructions and latest message do not fit with the requested output budget.",
        ));
    }

    let mut kept = 0;
    let mut ceiling = candidates;
    while kept < ceiling {
        let count = kept + (ceiling - kept).div_ceil(2);
        let (value, used) = with_turns(count)?;
        if used > 0 && used <= available {
            kept = count;
            best = value;
        } else {
            ceiling = count - 1;
        }
    }
    Ok((best, history.turns.len() - kept))
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    #[serde(flatten)]
    pub model: ModelConfig,
    pub temperature: f32,
    pub max_tokens: u32,
    pub top_p: f32,
    pub top_k: i32,
    pub enable_thinking: bool,
}

impl Default for ModelSettings {
    fn default() -> Self {
        let sample = GenerationRequest::new("Settings");
        Self {
            model: ModelConfig::default(),
            temperature: sample.temperature,
            max_tokens: sample.max_tokens,
            top_p: sample.top_p,
            top_k: sample.top_k,
            enable_thinking: sample.enable_thinking,
        }
    }
}

impl ModelSettings {
    pub fn recommended(context_limit: u32) -> Self {
        let mut settings = Self::default();
        settings.model.context_length = context_limit.min(8192);
        settings.max_tokens = settings.model.context_length / 2;
        settings
    }

    pub fn validate(self) -> Result<()> {
        self.model.validate()?;
        self.request("Settings".into(), ChatHistory::default())
            .validate()?;
        if self.max_tokens >= self.model.context_length {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "The output budget must be smaller than the context length.",
            ));
        }
        Ok(())
    }

    pub fn request(self, prompt: String, history: ChatHistory) -> GenerationRequest {
        GenerationRequest {
            prompt,
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            seed: None,
            enable_thinking: self.enable_thinking,
            history,
            chat: None,
            context: None,
            context_observer: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub appearance: Appearance,
    pub defaults: ModelSettings,
    pub enter_to_send: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            defaults: ModelSettings::default(),
            enter_to_send: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReference {
    pub id: ModelId,
    pub file_name: String,
    pub settings: ModelSettings,
    /// Registered from the verified application resource, not imported.
    pub bundled: bool,
    /// Settings follow the recommendation on every load until the user saves
    /// custom settings.
    pub automatic_settings: bool,
}

pub struct SavedModel {
    pub reference: ModelReference,
    pub path: PathBuf,
}

pub fn validate_title(title: &str) -> Result<&str> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 100 || trimmed.chars().any(char::is_control)
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Enter a title of 1–100 characters.",
        ));
    }
    Ok(trimmed)
}

pub fn validate_system_prompt(prompt: &str) -> Result<()> {
    if prompt.len() > 16 * 1024 || prompt.contains('\0') {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "System instructions must fit in 16 KiB and contain no NUL characters.",
        ));
    }
    Ok(())
}

pub trait ChatStore {
    fn list(&self, query: &str) -> Result<Vec<ConversationSummary>>;
    fn get(&self, id: ConversationId) -> Result<Conversation>;
    fn create(&mut self) -> Result<Conversation>;
    fn rename(&mut self, id: ConversationId, title: &str) -> Result<()>;
    fn set_system_prompt(&mut self, id: ConversationId, prompt: &str) -> Result<()>;
    fn delete(&mut self, id: ConversationId) -> Result<()>;
    fn begin(&mut self, id: ConversationId, action: TurnAction) -> Result<PendingTurn>;
    fn checkpoint(&mut self, message_id: MessageId, content: &str) -> Result<()>;
    fn finish(
        &mut self,
        message_id: MessageId,
        content: &str,
        state: GenerationState,
        stats: Option<&GenerationStats>,
    ) -> Result<()>;
    fn preferences(&self) -> Result<Preferences>;
    fn save_preferences(&mut self, preferences: &Preferences) -> Result<()>;
    fn recent_models(&self) -> Result<Vec<ModelReference>>;
    fn model(&self, id: ModelId) -> Result<SavedModel>;
    fn remember_model(
        &mut self,
        path: &Path,
        name: &str,
        settings: ModelSettings,
    ) -> Result<ModelReference>;
    fn save_model_settings(&mut self, id: ModelId, settings: ModelSettings) -> Result<()>;
    fn mark_model_used(&mut self, id: ModelId) -> Result<()>;
}
