//! Provider credentials in the freedesktop Secret Service, over an
//! encrypted (Diffie-Hellman) session on the local session bus, and native
//! masked entry owned by Rust. Credential values never cross Tauri IPC or
//! reach the webview, SQLite or logs. Every operation is bounded and fails
//! closed; there is no file or memory-only fallback.

use std::{cell::RefCell, collections::HashMap, future::Future, time::Duration};

use engine::credentials::{
    CredentialError, CredentialFuture, CredentialKey, CredentialSource, CredentialStatus, Secret,
};
use secret_service::{EncryptionType, Error, SecretService};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

const APPLICATION: &str = "io.github.gimesha-adikari.localfirstaiagent";
const KIND: &str = "provider-credential";
/// Bound for each secure-storage operation that needs no user interaction.
const TIMEOUT: Duration = Duration::from_secs(3);
/// Bound for storing, which may wait for the keyring to accept a new item.
const STORE_TIMEOUT: Duration = Duration::from_secs(10);

fn attributes(key: &CredentialKey) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", APPLICATION),
        ("kind", KIND),
        ("provider", key.provider.as_str()),
        ("origin", key.origin.as_str()),
    ])
}

fn map_error(error: Error) -> CredentialError {
    match error {
        Error::Locked | Error::Prompt | Error::PromptDisconnected => CredentialError::Locked,
        Error::NoResult => CredentialError::Missing,
        Error::Crypto(_) | Error::Zbus(_) | Error::ZbusFdo(_) | Error::Unavailable => {
            CredentialError::Unavailable
        }
        _ => CredentialError::Unavailable,
    }
}

/// Only a session bus on a local socket is acceptable. A TCP bus address
/// would be an outbound connection outside the network boundary.
fn local_session_bus() -> bool {
    match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        Err(_) => std::env::var_os("XDG_RUNTIME_DIR").is_some(),
        Ok(address) => address
            .split(';')
            .filter(|entry| !entry.is_empty())
            .all(|entry| entry.starts_with("unix:")),
    }
}

async fn connect() -> Result<SecretService<'static>, CredentialError> {
    if !local_session_bus() {
        return Err(CredentialError::Unavailable);
    }
    SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(map_error)
}

/// Run a secure-storage operation on the host runtime with a time bound.
fn bounded<T: Send + 'static>(
    limit: Duration,
    operation: impl Future<Output = Result<T, CredentialError>> + Send + 'static,
) -> CredentialFuture<T> {
    let task = tauri::async_runtime::spawn(async move {
        tokio::time::timeout(limit, operation)
            .await
            .unwrap_or(Err(CredentialError::TimedOut))
    });
    Box::pin(async move { task.await.unwrap_or(Err(CredentialError::Unavailable)) })
}

async fn read(key: CredentialKey) -> Result<Secret, CredentialError> {
    let service = connect().await?;
    let found = service
        .search_items(attributes(&key))
        .await
        .map_err(map_error)?;
    if found.unlocked.is_empty() && !found.locked.is_empty() {
        return Err(CredentialError::Locked);
    }
    let item = found.unlocked.first().ok_or(CredentialError::Missing)?;
    let bytes = Zeroizing::new(item.get_secret().await.map_err(map_error)?);
    let text = std::str::from_utf8(&bytes).map_err(|_| CredentialError::Invalid)?;
    Secret::new(Zeroizing::new(text.to_owned())).ok_or(CredentialError::Invalid)
}

async fn status(key: CredentialKey) -> Result<CredentialStatus, CredentialError> {
    let service = match connect().await {
        Ok(service) => service,
        Err(CredentialError::Locked) => return Ok(CredentialStatus::Locked),
        Err(_) => return Ok(CredentialStatus::Unavailable),
    };
    let found = match service.search_items(attributes(&key)).await {
        Ok(found) => found,
        Err(Error::Locked) => return Ok(CredentialStatus::Locked),
        Err(_) => return Ok(CredentialStatus::Unavailable),
    };
    Ok(if !found.unlocked.is_empty() {
        CredentialStatus::Stored
    } else if !found.locked.is_empty() {
        CredentialStatus::Locked
    } else {
        CredentialStatus::Missing
    })
}

async fn delete(key: CredentialKey) -> Result<(), CredentialError> {
    let service = connect().await?;
    let found = service
        .search_items(attributes(&key))
        .await
        .map_err(map_error)?;
    if !found.locked.is_empty() {
        return Err(CredentialError::Locked);
    }
    for item in &found.unlocked {
        item.delete().await.map_err(map_error)?;
    }
    Ok(())
}

async fn store(key: CredentialKey, label: String, secret: Secret) -> Result<(), CredentialError> {
    let service = connect().await?;
    let collection = service.get_default_collection().await.map_err(map_error)?;
    if collection.is_locked().await.map_err(map_error)? {
        return Err(CredentialError::Locked);
    }
    collection
        .create_item(
            &label,
            attributes(&key),
            secret.expose().as_bytes(),
            true,
            "text/plain",
        )
        .await
        .map_err(map_error)?;
    Ok(())
}

/// Credentials in the user's Secret Service keyring.
pub struct SecretServiceStore;

impl CredentialSource for SecretServiceStore {
    fn read(&self, key: &CredentialKey) -> CredentialFuture<Secret> {
        bounded(TIMEOUT, read(key.clone()))
    }

    fn status(&self, key: &CredentialKey) -> CredentialFuture<CredentialStatus> {
        bounded(TIMEOUT, status(key.clone()))
    }

    fn delete(&self, key: &CredentialKey) -> CredentialFuture<()> {
        let key = key.clone();
        Box::pin(async move {
            tokio::time::timeout(TIMEOUT, delete(key))
                .await
                .unwrap_or(Err(CredentialError::TimedOut))
        })
    }

    fn store(
        &self,
        key: &CredentialKey,
        provider_name: &str,
        secret: Secret,
    ) -> CredentialFuture<()> {
        let label = format!("Local-first AI agent: {provider_name} API key");
        let key = key.clone();
        Box::pin(async move {
            tokio::time::timeout(STORE_TIMEOUT, store(key, label, secret))
                .await
                .unwrap_or(Err(CredentialError::TimedOut))
        })
    }
}

/// Ask for a credential in a native, modal GTK dialog with masked input.
/// Returns `None` when the user cancels. The value goes straight from the
/// GTK entry into zeroizing Rust memory and is never sent to the webview.
pub async fn prompt(
    app: &AppHandle,
    provider_name: String,
) -> Result<Option<Zeroizing<String>>, ()> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    let window = app.get_webview_window("main");
    app.run_on_main_thread(move || {
        use gtk::prelude::*;
        let parent = window.and_then(|window| window.gtk_window().ok());
        let dialog = gtk::Dialog::with_buttons(
            Some("Enter API key"),
            parent.as_ref(),
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            &[
                ("Cancel", gtk::ResponseType::Cancel),
                ("Save in keyring", gtk::ResponseType::Accept),
            ],
        );
        dialog.set_default_response(gtk::ResponseType::Accept);
        dialog.set_resizable(false);
        let content = dialog.content_area();
        content.set_spacing(10);
        content.set_margin_top(16);
        content.set_margin_bottom(8);
        content.set_margin_start(16);
        content.set_margin_end(16);
        let label = gtk::Label::new(Some(&format!(
            "Enter the API key for {provider_name}.\n\nIt is saved in your system keyring and is never shown in the app window."
        )));
        label.set_line_wrap(true);
        label.set_max_width_chars(48);
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_visibility(false);
        entry.set_invisible_char(Some('•'));
        entry.set_input_purpose(gtk::InputPurpose::Password);
        entry.set_max_length(engine::credentials::MAX_SECRET_BYTES as i32);
        entry.set_activates_default(true);
        entry.set_width_chars(40);
        content.add(&label);
        content.add(&entry);
        let reply = RefCell::new(Some(reply));
        dialog.connect_response(move |dialog, response| {
            let value = (response == gtk::ResponseType::Accept)
                .then(|| Zeroizing::new(entry.text().to_string()));
            // Clear the entry buffer before the widget is destroyed.
            entry.set_text("");
            if let Some(reply) = reply.borrow_mut().take() {
                let _ = reply.send(value);
            }
            dialog.close();
        });
        dialog.show_all();
    })
    .map_err(|_| ())?;
    answer.await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_bind_provider_and_origin_only() {
        let key = CredentialKey {
            provider: "p1-abc".into(),
            origin: "https://api.example.com:443".into(),
        };
        let attributes = attributes(&key);
        assert_eq!(attributes.len(), 4);
        assert_eq!(attributes["origin"], "https://api.example.com:443");
        assert_eq!(attributes["provider"], "p1-abc");
        assert!(attributes.values().all(|value| !value.contains("sk-")));
    }

    /// Store, read, replace and delete a synthetic credential in the Secret
    /// Service of the current session bus. Run only inside an isolated
    /// `dbus-run-session` with a temporary, unlocked keyring.
    #[test]
    #[ignore = "uses the session bus Secret Service; run in an isolated D-Bus session with a temporary keyring"]
    fn secret_service_round_trip_in_isolated_session() {
        assert!(
            std::env::var("AGENT_ISOLATED_KEYRING").is_ok_and(|value| value == "1"),
            "set AGENT_ISOLATED_KEYRING=1 inside an isolated session"
        );
        let key = CredentialKey {
            provider: "acceptance-provider".into(),
            origin: "https://api.example.com:443".into(),
        };
        let other = CredentialKey {
            origin: "https://other.example.com:443".into(),
            ..key.clone()
        };
        let store = SecretServiceStore;
        tauri::async_runtime::block_on(async {
            let _ = store.delete(&key).await;
            assert_eq!(store.status(&key).await, Ok(CredentialStatus::Missing));
            let first = Secret::new(Zeroizing::new("sk-synthetic-first".into())).unwrap();
            store.store(&key, "Acceptance", first).await.unwrap();
            assert_eq!(store.status(&key).await, Ok(CredentialStatus::Stored));
            assert_eq!(
                store.read(&key).await.unwrap().expose(),
                "sk-synthetic-first"
            );
            // Bound to the exact origin.
            assert_eq!(
                store.read(&other).await.unwrap_err(),
                CredentialError::Missing
            );
            let second = Secret::new(Zeroizing::new("sk-synthetic-second".into())).unwrap();
            store.store(&key, "Acceptance", second).await.unwrap();
            assert_eq!(
                store.read(&key).await.unwrap().expose(),
                "sk-synthetic-second"
            );
            store.delete(&key).await.unwrap();
            assert_eq!(store.status(&key).await, Ok(CredentialStatus::Missing));
            assert_eq!(
                store.read(&key).await.unwrap_err(),
                CredentialError::Missing
            );
        });
    }

    /// Without a Secret Service on the bus every operation fails closed.
    #[test]
    #[ignore = "run in an isolated D-Bus session that has no Secret Service"]
    fn missing_secret_service_fails_closed() {
        assert!(std::env::var("AGENT_NO_KEYRING").is_ok_and(|value| value == "1"));
        let key = CredentialKey {
            provider: "acceptance-provider".into(),
            origin: "https://api.example.com:443".into(),
        };
        let store = SecretServiceStore;
        tauri::async_runtime::block_on(async {
            assert_eq!(store.status(&key).await, Ok(CredentialStatus::Unavailable));
            assert!(matches!(
                store.read(&key).await.unwrap_err(),
                CredentialError::Unavailable | CredentialError::TimedOut
            ));
            let secret = Secret::new(Zeroizing::new("sk-synthetic".into())).unwrap();
            assert!(store.store(&key, "Acceptance", secret).await.is_err());
        });
    }

    #[test]
    fn only_local_session_buses_are_used() {
        // SAFETY: test-only environment changes.
        unsafe {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus");
            assert!(local_session_bus());
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "tcp:host=example.com,port=1234");
            assert!(!local_session_bus());
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/run/user/1000/bus;tcp:host=10.0.0.1,port=5",
            );
            assert!(!local_session_bus());
            std::env::remove_var("DBUS_SESSION_BUS_ADDRESS");
        }
    }
}
