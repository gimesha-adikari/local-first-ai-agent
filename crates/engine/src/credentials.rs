//! Host interface for provider credentials. Values exist only in trusted
//! Rust memory, wrapped so they are zeroed on drop and never formatted,
//! serialized or cloned. The engine reads a credential only at the
//! transport edge, for a destination bound to the same provider and origin.

use std::{fmt, future::Future, pin::Pin};

use serde::Serialize;
use zeroize::Zeroizing;

/// Non-secret lookup attributes: the provider and the canonical origin the
/// credential may be sent to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialKey {
    pub provider: String,
    pub origin: String,
}

/// Largest accepted credential.
pub const MAX_SECRET_BYTES: usize = 4096;

/// A credential value. It has no `Debug` content, `Display`, `Clone` or
/// serialization, and its memory is zeroed when dropped.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Accepts visible ASCII only, which keeps the value safe to place in a
    /// header and rejects pasted line breaks.
    pub fn new(value: Zeroizing<String>) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty()
            || trimmed.len() > MAX_SECRET_BYTES
            || !trimmed.bytes().all(|b| b.is_ascii_graphic())
        {
            return None;
        }
        if trimmed.len() == value.len() {
            return Some(Self(value));
        }
        Some(Self(Zeroizing::new(trimmed.to_owned())))
    }

    /// Expose the value to the trusted transport edge or secure storage.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialError {
    Missing,
    Locked,
    /// No secure storage service, or it refused the request.
    Unavailable,
    TimedOut,
    /// The stored value is not a usable credential.
    Invalid,
}

impl CredentialError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Missing => "No API key is stored for this provider.",
            Self::Locked => "Secure storage is locked. Unlock your keyring and try again.",
            Self::Unavailable => {
                "Secure storage is unavailable, so cloud providers are off. Local models still work."
            }
            Self::TimedOut => "Secure storage did not respond in time.",
            Self::Invalid => "The stored API key is not valid. Enter it again.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    Stored,
    Missing,
    Locked,
    Unavailable,
}

pub type CredentialFuture<T> = Pin<Box<dyn Future<Output = Result<T, CredentialError>> + Send>>;

/// Implemented by the host over operating-system secure storage. Each
/// operation must be bounded by the host and fail closed.
pub trait CredentialSource: Send + Sync + 'static {
    fn read(&self, key: &CredentialKey) -> CredentialFuture<Secret>;
    fn status(&self, key: &CredentialKey) -> CredentialFuture<CredentialStatus>;
    fn delete(&self, key: &CredentialKey) -> CredentialFuture<()>;
}

/// Used when the host provides no secure storage: every provider that
/// needs a credential stays off.
pub struct NoSecureStorage;

impl CredentialSource for NoSecureStorage {
    fn read(&self, _key: &CredentialKey) -> CredentialFuture<Secret> {
        Box::pin(async { Err(CredentialError::Unavailable) })
    }

    fn status(&self, _key: &CredentialKey) -> CredentialFuture<CredentialStatus> {
        Box::pin(async { Ok(CredentialStatus::Unavailable) })
    }

    fn delete(&self, _key: &CredentialKey) -> CredentialFuture<()> {
        Box::pin(async { Err(CredentialError::Unavailable) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_validated_and_never_formatted() {
        let secret = Secret::new(Zeroizing::new("  sk-test-123  ".to_owned())).unwrap();
        assert_eq!(secret.expose(), "sk-test-123");
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        for invalid in ["", "   ", "two words", "line\nbreak", "tab\tkey", "naïve"] {
            assert!(
                Secret::new(Zeroizing::new(invalid.to_owned())).is_none(),
                "{invalid:?}"
            );
        }
        assert!(Secret::new(Zeroizing::new("x".repeat(MAX_SECRET_BYTES + 1))).is_none());
    }
}
