//! Main-window IPC for providers, credentials and the intelligence mode.
//! Commands carry configuration and status only. Credential values are
//! entered natively and go straight to secure storage; no command accepts
//! or returns one.

use agent_core::{
    inference::{ErrorCode, InferenceError, Result},
    provider::{IntelligenceSettings, ProviderId, ProviderSettings},
};
use engine::{
    credentials::{CredentialStatus, Secret},
    providers::{ProviderTest, ProviderView},
};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::Desktop;

fn valid_id(id: ProviderId) -> Result<ProviderId> {
    if (1..=i64::from(u32::MAX)).contains(&id) {
        Ok(id)
    } else {
        Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Choose a valid provider.",
        ))
    }
}

#[tauri::command]
pub async fn provider_list(state: State<'_, Desktop>) -> Result<Vec<ProviderView>> {
    state.engine.list_providers().await
}

#[tauri::command]
pub async fn provider_save(
    state: State<'_, Desktop>,
    id: Option<ProviderId>,
    settings: ProviderSettings,
) -> Result<ProviderView> {
    let id = id.map(valid_id).transpose()?;
    state.engine.save_provider(id, settings).await
}

#[tauri::command]
pub async fn provider_delete(state: State<'_, Desktop>, id: ProviderId) -> Result<()> {
    state.engine.delete_provider(valid_id(id)?).await
}

#[tauri::command]
pub async fn provider_test(state: State<'_, Desktop>, id: ProviderId) -> Result<ProviderTest> {
    state.engine.test_provider(valid_id(id)?).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryOutcome {
    Stored,
    Cancelled,
}

/// Open the native masked entry for a provider's API key and store the
/// value in the Secret Service. Only the outcome is returned.
#[tauri::command]
pub async fn provider_enter_credential(
    app: AppHandle,
    state: State<'_, Desktop>,
    id: ProviderId,
) -> Result<EntryOutcome> {
    let id = valid_id(id)?;
    let edit = state.engine.edit_provider_credential(id)?;
    let entered = crate::credentials::prompt(&app, edit.provider_name().to_owned())
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
    edit.store(secret).await?;
    Ok(EntryOutcome::Stored)
}

#[tauri::command]
pub async fn provider_delete_credential(
    state: State<'_, Desktop>,
    id: ProviderId,
) -> Result<CredentialStatus> {
    state
        .engine
        .edit_provider_credential(valid_id(id)?)?
        .delete()
        .await?;
    Ok(CredentialStatus::Missing)
}

#[tauri::command]
pub fn intelligence_settings(state: State<'_, Desktop>) -> Result<IntelligenceSettings> {
    state.engine.intelligence_settings()
}

#[tauri::command]
pub fn set_intelligence_settings(
    state: State<'_, Desktop>,
    settings: IntelligenceSettings,
) -> Result<()> {
    if let Some(id) = settings.active_provider {
        valid_id(id)?;
    }
    state.engine.set_intelligence_settings(settings)
}
