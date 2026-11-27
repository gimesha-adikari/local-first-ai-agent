//! Native database keyring adapter. Key identities are separate from providers
//! and scoped to the canonical profile path. The local session uses Secret
//! Service's encrypted transport, as for provider credentials.
use std::{collections::HashMap, path::Path};

use engine::credentials::CredentialError;
use sha2::{Digest, Sha256};
use storage::profile::DatabaseKey;
use zeroize::Zeroizing;

use crate::credentials::{APPLICATION, STORE_TIMEOUT, TIMEOUT, connect, map_error};

pub trait DatabaseKeyring: Send + Sync {
    fn read(&self, path: &Path) -> Result<DatabaseKey, CredentialError>;
    fn save(&self, path: &Path, key: &DatabaseKey) -> Result<(), CredentialError>;
}

pub struct LinuxKeyring;

fn profile_identity(path: &Path) -> String {
    format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()))
}

fn attributes(profile: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", APPLICATION),
        ("kind", "database-key"),
        ("profile", profile),
    ])
}

impl DatabaseKeyring for LinuxKeyring {
    fn read(&self, path: &Path) -> Result<DatabaseKey, CredentialError> {
        let profile = profile_identity(path);
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(TIMEOUT, async {
                let service = connect().await?;
                let found = service
                    .search_items(attributes(&profile))
                    .await
                    .map_err(map_error)?;
                if !found.locked.is_empty() {
                    return Err(CredentialError::Locked);
                }
                let item = match found.unlocked.as_slice() {
                    [] => return Err(CredentialError::Missing),
                    [item] => item,
                    _ => return Err(CredentialError::Invalid),
                };
                let bytes = Zeroizing::new(item.get_secret().await.map_err(map_error)?);
                DatabaseKey::from_bytes(&bytes).ok_or(CredentialError::Invalid)
            })
            .await
            .unwrap_or(Err(CredentialError::TimedOut))
        })
    }

    fn save(&self, path: &Path, key: &DatabaseKey) -> Result<(), CredentialError> {
        let profile = profile_identity(path);
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(STORE_TIMEOUT, async {
                let service = connect().await?;
                let found = service
                    .search_items(attributes(&profile))
                    .await
                    .map_err(map_error)?;
                if !found.locked.is_empty() {
                    return Err(CredentialError::Locked);
                }
                match found.unlocked.as_slice() {
                    [item] => {
                        return item
                            .set_secret(key.as_bytes(), "application/octet-stream")
                            .await
                            .map_err(map_error);
                    }
                    [] => {}
                    _ => return Err(CredentialError::Invalid),
                }
                let collection = service.get_default_collection().await.map_err(|error| {
                    match map_error(error) {
                        CredentialError::Missing => CredentialError::Unavailable,
                        other => other,
                    }
                })?;
                match service.get_collection_by_alias("session").await {
                    Ok(session) if session.collection_path == collection.collection_path => {
                        return Err(CredentialError::Unavailable);
                    }
                    Ok(_) | Err(secret_service::Error::NoResult) => {}
                    Err(error) => return Err(map_error(error)),
                }
                if collection.is_locked().await.map_err(map_error)? {
                    return Err(CredentialError::Locked);
                }
                collection
                    .create_item(
                        "Local-first AI Agent: database encryption key",
                        attributes(&profile),
                        key.as_bytes(),
                        false,
                        "application/octet-stream",
                    )
                    .await
                    .map_err(map_error)?;
                Ok(())
            })
            .await
            .unwrap_or(Err(CredentialError::TimedOut))
        })
    }
}
