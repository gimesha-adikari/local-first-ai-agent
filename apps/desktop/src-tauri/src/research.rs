//! Main-window IPC for web research: search provider settings, the Brave
//! Search key (entered natively, never through the webview), validated
//! source views and opening a validated link in the system browser on an
//! explicit click. No command accepts a URL to open: links are resolved from
//! the engine's trusted research records.

use agent_core::{
    chat::MessageId,
    inference::{ErrorCode, InferenceError, Result},
    knowledge::MAX_DOCUMENT_ID,
    research::{MAX_SEARCH_RESULTS, WebResearchSettings},
};
use engine::{
    credentials::Secret,
    research::{LinkTarget, ResearchView, WebResearchView},
};
use tauri::{AppHandle, State};

use crate::{commands::Desktop, providers::EntryOutcome};

fn valid_message(id: MessageId) -> Result<MessageId> {
    if (1..=MAX_DOCUMENT_ID).contains(&id) {
        Ok(id)
    } else {
        Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Enter a valid assistant message ID.",
        ))
    }
}

#[tauri::command]
pub async fn web_research_settings(state: State<'_, Desktop>) -> Result<WebResearchView> {
    state.engine.web_research().await
}

#[tauri::command]
pub async fn web_research_save(
    state: State<'_, Desktop>,
    settings: WebResearchSettings,
) -> Result<WebResearchView> {
    state.engine.save_web_research(settings).await
}

/// Open the native masked entry for the Brave Search API key. Only the
/// outcome is returned.
#[tauri::command]
pub async fn web_research_enter_key(
    app: AppHandle,
    state: State<'_, Desktop>,
) -> Result<EntryOutcome> {
    state.engine.brave_credential_key()?;
    let entered = crate::credentials::prompt(&app, "Brave Search".to_owned())
        .await
        .map_err(|_| {
            InferenceError::new(
                ErrorCode::PickerFailed,
                "The key entry dialog could not open.",
            )
        })?;
    let Some(value) = entered else {
        return Ok(EntryOutcome::Cancelled);
    };
    let secret = Secret::new(value).ok_or_else(|| {
        InferenceError::new(
            ErrorCode::InvalidConfig,
            "API keys contain visible characters only, without spaces or line breaks.",
        )
    })?;
    state.engine.store_brave_key(secret).await?;
    Ok(EntryOutcome::Stored)
}

#[tauri::command]
pub async fn web_research_delete_key(state: State<'_, Desktop>) -> Result<()> {
    state.engine.delete_brave_key().await
}

#[tauri::command]
pub fn chat_web_research(
    state: State<'_, Desktop>,
    message_id: MessageId,
) -> Result<Option<ResearchView>> {
    state.engine.web_research_view(valid_message(message_id)?)
}

/// Open a source or result link in the user's default browser. The link
/// comes from the engine's records and is revalidated as public HTTPS.
#[tauri::command]
pub async fn web_research_open(
    app: AppHandle,
    state: State<'_, Desktop>,
    message_id: MessageId,
    target: LinkTarget,
) -> Result<()> {
    let index = match target {
        LinkTarget::Source(index) | LinkTarget::Discovered(index) => index,
    };
    if index >= MAX_SEARCH_RESULTS {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Choose a listed source.",
        ));
    }
    let url = state
        .engine
        .web_research_link(valid_message(message_id)?, target)?;
    let (reply, opened) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let launched =
            gtk::gio::AppInfo::launch_default_for_uri(&url, None::<&gtk::gio::AppLaunchContext>);
        let _ = reply.send(launched.is_ok());
    })
    .map_err(|_| open_failed())?;
    match tokio::time::timeout(std::time::Duration::from_secs(10), opened).await {
        Ok(Ok(true)) => Ok(()),
        _ => Err(open_failed()),
    }
}

fn open_failed() -> InferenceError {
    InferenceError::new(
        ErrorCode::NotFound,
        "The link couldn't be opened in your browser.",
    )
}
