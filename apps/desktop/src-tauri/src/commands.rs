//! Main-window IPC for conversations, models and Chat. Orchestration lives in
//! the engine; these commands validate input, open native dialogs and adapt
//! events to IPC channels.

use std::sync::Arc;

use crate::agent::AgentSession;
use agent_core::{
    chat::{
        Conversation, ConversationId, ConversationSummary, Message, MessageId, ModelId,
        ModelReference, ModelSettings, Preferences,
    },
    inference::{ErrorCode, InferenceError, ModelInfo, Result},
};
use engine::{
    Bootstrap, Engine,
    chat::{ChatEvent, ChatEventSink, ChatEvents, ChatOptions},
    models::RecommendedChatSettings,
};
use inference::Status;
use tauri::{State, ipc::Channel};

pub struct Desktop {
    pub engine: Arc<Engine>,
    pub agent: Arc<AgentSession>,
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.agent.shutdown(self.engine.worker());
    }
}

/// Delivers engine Chat events over the invoking window's IPC channel.
struct IpcChat(Channel<ChatEvent>);

impl ChatEventSink for IpcChat {
    fn send(&self, event: ChatEvent) {
        let _ = self.0.send(event);
    }
}

fn chat_events(channel: Channel<ChatEvent>) -> ChatEvents {
    Arc::new(IpcChat(channel))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopBootstrap {
    #[serde(flatten)]
    engine: Bootstrap,
    plaintext_backup: Option<String>,
}

#[tauri::command]
pub fn bootstrap(
    state: State<'_, Desktop>,
    database: State<'_, crate::database_startup::DatabaseAccess>,
) -> Result<DesktopBootstrap> {
    Ok(DesktopBootstrap {
        engine: state.engine.bootstrap()?,
        plaintext_backup: database.plaintext_backup(),
    })
}

#[tauri::command]
pub fn list_conversations(
    state: State<'_, Desktop>,
    query: &str,
) -> Result<Vec<ConversationSummary>> {
    if query.len() > 1024 || query.contains('\0') {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "The search is too long or invalid.",
        ));
    }
    state.engine.list_conversations(query.trim())
}

#[tauri::command]
pub fn get_conversation(state: State<'_, Desktop>, id: ConversationId) -> Result<Conversation> {
    state.engine.get_conversation(id)
}

#[tauri::command]
pub fn create_conversation(state: State<'_, Desktop>) -> Result<Conversation> {
    state.engine.create_conversation()
}

#[tauri::command]
pub fn rename_conversation(
    state: State<'_, Desktop>,
    id: ConversationId,
    title: &str,
) -> Result<()> {
    state.engine.rename_conversation(id, title)
}

#[tauri::command]
pub fn set_system_prompt(
    state: State<'_, Desktop>,
    id: ConversationId,
    system_prompt: &str,
) -> Result<()> {
    state.engine.set_system_prompt(id, system_prompt)
}

#[tauri::command]
pub fn delete_conversation(state: State<'_, Desktop>, id: ConversationId) -> Result<()> {
    state.engine.delete_conversation(id)?;
    state.agent.forget_conversation(id);
    Ok(())
}

#[tauri::command]
pub fn save_preferences(state: State<'_, Desktop>, preferences: Preferences) -> Result<()> {
    state.engine.save_preferences(&preferences)
}

#[tauri::command]
pub async fn choose_model(state: State<'_, Desktop>) -> Result<Option<ModelReference>> {
    let operation = state.engine.reserve()?;
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Choose a GGUF model")
        .add_filter("GGUF models", &["gguf"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    state
        .engine
        .register_model(&operation, file.path().to_path_buf())
        .await
        .map(Some)
}

/// Listed models after collapsing copies of the same weights.
#[tauri::command]
pub fn list_models(state: State<'_, Desktop>) -> Result<Vec<ModelReference>> {
    state.engine.list_models()
}

#[tauri::command]
pub fn inference_status(state: State<'_, Desktop>) -> Result<Status> {
    state.engine.inference_status()
}

#[tauri::command]
pub async fn load_model(
    state: State<'_, Desktop>,
    model_id: ModelId,
    settings: ModelSettings,
) -> Result<ModelInfo> {
    crate::bundled_model::check_retained_model(&state, model_id).await?;
    state.engine.load_model(model_id, settings).await
}

#[tauri::command]
pub fn save_model_settings(
    state: State<'_, Desktop>,
    model_id: ModelId,
    settings: ModelSettings,
) -> Result<()> {
    state.engine.save_model_settings(model_id, settings)
}

#[tauri::command]
pub async fn reset_model_settings(
    state: State<'_, Desktop>,
    model_id: ModelId,
) -> Result<ModelReference> {
    state.engine.reset_model_settings(model_id).await
}

#[tauri::command]
pub async fn unload_model(state: State<'_, Desktop>) -> Result<()> {
    state.engine.unload_model().await
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Desktop>,
    id: ConversationId,
    content: String,
    options: Option<ChatOptions>,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    state
        .engine
        .send_message(
            id,
            content,
            options.unwrap_or_default(),
            chat_events(events),
        )
        .await
}

#[tauri::command]
pub async fn regenerate(
    state: State<'_, Desktop>,
    id: ConversationId,
    options: Option<ChatOptions>,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    state
        .engine
        .regenerate(id, options.unwrap_or_default(), chat_events(events))
        .await
}

#[tauri::command]
pub fn recommended_chat_settings(state: State<'_, Desktop>) -> Result<RecommendedChatSettings> {
    state.engine.recommended_chat_settings()
}

#[tauri::command]
pub fn chat_continuation_remaining(
    state: State<'_, Desktop>,
    id: ConversationId,
    message_id: MessageId,
) -> Result<u32> {
    state.engine.continuation_remaining(id, message_id)
}

#[tauri::command]
pub async fn continue_chat(
    state: State<'_, Desktop>,
    id: ConversationId,
    message_id: MessageId,
    max_tokens: Option<u32>,
    events: Channel<ChatEvent>,
) -> Result<Message> {
    state
        .engine
        .continue_chat(id, message_id, max_tokens, chat_events(events))
        .await
}

#[tauri::command]
pub fn stop_generation(state: State<'_, Desktop>) -> Result<bool> {
    if state.agent.is_active() {
        return Err(InferenceError::new(
            ErrorCode::Busy,
            "Cancel the active agent task from Agent mode.",
        ));
    }
    state.engine.stop_chat()
}
