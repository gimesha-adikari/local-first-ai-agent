//! Configured providers: canonical registration with the policy kernel,
//! credential binding, capability advertisement, discovery and the trusted
//! consent rules for remote operations.

pub(crate) mod openai;

use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

use agent_core::{
    capability::{
        DISCOVER, GENERATE, IMAGE, Languages, Locality, Offer, Resources, Retention, TEXT,
        UnitPrice, Usage,
    },
    classification::{Classified, DataClasses, PROVIDER_CONTROL},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    provider::{IntelligenceSettings, ProviderId, ProviderRetention, ProviderSettings},
};
use serde::Serialize;
use storage::providers::{SavedProvider, UsageSummary};

use crate::{
    Engine,
    credentials::{CredentialError, CredentialKey, CredentialStatus},
    database,
    network::{Destination, Endpoint, Failure, Method, Outbound, Scheme},
    policy::{Denial, GrantId, Operation, Permission, Policy, Session},
};

/// The destination identifier for a provider.
pub(crate) fn destination_id(key: &str) -> String {
    format!("provider/{key}")
}

fn endpoint_error(error: crate::network::EndpointError) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, error.to_string())
}

/// Canonicalize the endpoint and derive the registered destination.
pub(crate) fn destination(key: &str, settings: &ProviderSettings) -> Result<Destination> {
    let endpoint = Endpoint::parse(&settings.endpoint).map_err(endpoint_error)?;
    let destination =
        Destination::new(&destination_id(key), endpoint, settings.locality.locality())
            .map_err(endpoint_error)?;
    // A key on unencrypted HTTP could be read by anyone on the network path.
    // Only this computer's loopback is exempt.
    if settings.requires_key
        && destination.endpoint().scheme() == Scheme::Http
        && *destination.scope() != crate::network::NetworkScope::Loopback
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Use https for a server on your network that needs an API key, so the key is never sent unencrypted.",
        ));
    }
    Ok(if settings.requires_key {
        destination.with_credential(key)
    } else {
        destination
    })
}

/// What a configured provider offers for Chat. Built only from the user's
/// configuration; nothing is assumed about an unknown server.
pub(crate) fn offer(saved: &SavedProvider) -> Offer {
    let settings = &saved.settings;
    Offer {
        capability: GENERATE,
        provider: Cow::Owned(format!("openai-compatible/{}", saved.key)),
        locality: settings.locality.locality(),
        input_modalities: if settings.image_input {
            &[TEXT, IMAGE]
        } else {
            &[TEXT]
        },
        output_modalities: &[TEXT],
        input_languages: Languages::Unspecified,
        output_languages: Languages::Unspecified,
        streaming: true,
        structured_output: false,
        resources: Resources {
            cpu_threads: None,
            weight_bytes: None,
            context: Some(Usage {
                unit: "token",
                amount: settings.context_tokens.into(),
            }),
        },
        usage_units: &["input-token", "output-token"],
        unit_prices: settings
            .pricing
            .iter()
            .flat_map(|pricing| {
                [
                    UnitPrice {
                        unit: "input-token",
                        currency: pricing.currency.clone(),
                        micros_per_million: pricing.input_micros_per_million,
                    },
                    UnitPrice {
                        unit: "output-token",
                        currency: pricing.currency.clone(),
                        micros_per_million: pricing.output_micros_per_million,
                    },
                ]
            })
            .collect(),
        provider_retention: settings.retention.retention(),
    }
}

/// The trusted consent rule for remote operations: the payload may carry
/// only the classes the user enabled for this provider, and the provider's
/// retention must be disclosed. It mints one-operation grants.
pub(crate) fn consent(
    policy: &Policy,
    settings: &ProviderSettings,
    operation: &Operation,
    until: Instant,
) -> std::result::Result<(GrantId, GrantId), Denial> {
    if settings.retention == ProviderRetention::Unknown {
        return Err(Denial::Scope);
    }
    let allowed = settings.sharing.allowed_classes();
    let registry = DataClasses::new(allowed.iter().copied());
    if operation
        .labels()
        .ids()
        .any(|id| registry.sensitivity(id).is_none())
    {
        return Err(Denial::MissingGrant);
    }
    let process = policy.grant(Permission::Process, operation, until)?;
    match policy.grant(Permission::Share, operation, until) {
        Ok(share) => Ok((process, share)),
        Err(denial) => {
            let _ = policy.revoke(process);
            Err(denial)
        }
    }
}

/// Map a transport failure onto the normalized error contract.
pub(crate) fn failure_error(failure: &Failure) -> InferenceError {
    let (code, message): (ErrorCode, Cow<'static, str>) = match failure {
        Failure::Cancelled => (ErrorCode::Cancelled, "The request was cancelled.".into()),
        Failure::Denied(Denial::Locality) => (
            ErrorCode::PolicyDenied,
            "Local-only mode is on, so nothing was sent.".into(),
        ),
        Failure::Denied(Denial::Revoked | Denial::Expired) => (
            ErrorCode::PolicyDenied,
            "The sharing authorization ended, so the response was discarded.".into(),
        ),
        Failure::Denied(_) => (
            ErrorCode::PolicyDenied,
            "The intelligence policy denied this request.".into(),
        ),
        Failure::Credential(CredentialError::Missing) => (
            ErrorCode::CredentialMissing,
            CredentialError::Missing.message().into(),
        ),
        Failure::Credential(error) => (ErrorCode::CredentialUnavailable, error.message().into()),
        Failure::Resolve => (
            ErrorCode::NetworkFailed,
            "The provider's host name could not be resolved.".into(),
        ),
        Failure::Address => (
            ErrorCode::PolicyDenied,
            "The provider resolved to an address outside its allowed network, so nothing was sent."
                .into(),
        ),
        Failure::Connect => (
            ErrorCode::NetworkFailed,
            "Could not connect to the provider.".into(),
        ),
        Failure::Tls => (
            ErrorCode::NetworkFailed,
            "The provider's TLS certificate could not be verified.".into(),
        ),
        Failure::TimedOut => (ErrorCode::TimedOut, "The provider did not respond in time.".into()),
        Failure::TooLarge => (
            ErrorCode::ProviderInvalidResponse,
            "The provider's response exceeded this app's size limits.".into(),
        ),
        Failure::Protocol => (
            ErrorCode::ProviderInvalidResponse,
            "The provider sent a malformed HTTP response.".into(),
        ),
        Failure::Io => (
            ErrorCode::NetworkFailed,
            "The connection to the provider failed.".into(),
        ),
        Failure::Redirect { status, .. } => (
            ErrorCode::ProviderRejected,
            format!("The provider redirected the request (HTTP {status}); redirects are not followed. Check the URL.").into(),
        ),
        Failure::Stopped => (
            ErrorCode::NetworkFailed,
            "The network boundary stopped.".into(),
        ),
    };
    InferenceError::new(code, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    /// The user must add an API key.
    NeedsCredential,
    /// Secure storage is unavailable or locked: fail closed.
    CredentialUnavailable,
    /// Provider retention is not disclosed.
    NeedsRetention,
    /// Local-only mode blocks all providers.
    LocalOnly,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub id: ProviderId,
    pub settings: ProviderSettings,
    pub origin: String,
    pub network: &'static str,
    pub encrypted: bool,
    pub credential: Option<CredentialStatus>,
    pub period: String,
    pub usage: UsageSummary,
    pub modalities: Vec<&'static str>,
    pub readiness: Readiness,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTest {
    pub status: u16,
    pub models: Vec<String>,
    /// Whether the configured model appears in the provider's list. `None`
    /// when the provider returned no list.
    pub model_listed: Option<bool>,
    pub elapsed_ms: u64,
}

impl Engine {
    /// Register saved providers and apply the saved selection. Invalid
    /// rows stay listed but are never registered.
    pub(crate) fn restore_providers(&self) -> Result<()> {
        let (providers, settings) = {
            let db = database(&self.database)?;
            (db.providers()?, db.intelligence_settings()?)
        };
        for saved in providers {
            if let Ok(destination) = destination(&saved.key, &saved.settings) {
                let _ = self.policy.register_destination(destination);
            }
        }
        self.policy.set_local_only(settings.local_only);
        Ok(())
    }

    pub fn intelligence_settings(&self) -> Result<IntelligenceSettings> {
        database(&self.database)?.intelligence_settings()
    }

    /// Change local-only mode or the active provider. Turning local-only
    /// mode on denies in-flight remote work at its next liveness check.
    pub fn set_intelligence_settings(&self, settings: IntelligenceSettings) -> Result<()> {
        let _operation = self.reserve()?;
        database(&self.database)?.save_intelligence_settings(settings)?;
        self.policy.set_local_only(settings.local_only);
        Ok(())
    }

    fn saved_provider(&self, id: ProviderId) -> Result<SavedProvider> {
        database(&self.database)?.provider(id)
    }

    /// The non-secret secure-storage key for a provider's credential.
    pub fn credential_key(&self, id: ProviderId) -> Result<CredentialKey> {
        let saved = self.saved_provider(id)?;
        destination(&saved.key, &saved.settings)?
            .credential()
            .map(|binding| binding.key().clone())
            .ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::InvalidConfig,
                    "This provider is configured without an API key.",
                )
            })
    }

    /// Validate, canonicalize and save a provider, then register its
    /// destination. A changed origin drops the credential bound to the old
    /// origin, so a key is never sent to a different server.
    pub async fn save_provider(
        &self,
        id: Option<ProviderId>,
        mut settings: ProviderSettings,
    ) -> Result<ProviderView> {
        settings.validate()?;
        let endpoint = Endpoint::parse(&settings.endpoint).map_err(endpoint_error)?;
        endpoint
            .scope_for(settings.locality.locality())
            .map_err(endpoint_error)?;
        settings.endpoint = endpoint.display();
        settings.name = settings.name.trim().to_owned();
        settings.model = settings.model.trim().to_owned();
        settings.retention_summary = settings.retention_summary.trim().to_owned();
        let previous = id.map(|id| self.saved_provider(id)).transpose()?;
        let saved = {
            let _operation = self.reserve()?;
            database(&self.database)?.save_provider(id, &settings)?
        };
        let destination = destination(&saved.key, &saved.settings)?;
        self.policy
            .register_destination(destination.clone())
            .map_err(crate::chat_session::policy_error)?;
        if let Some(previous) = previous
            && let Ok(old) = self::destination(&previous.key, &previous.settings)
            && let Some(old) = old.credential()
            && destination.credential().map(|binding| binding.key()) != Some(old.key())
        {
            let _ = self.credentials.delete(old.key()).await;
        }
        self.provider_view(saved).await
    }

    pub async fn delete_provider(&self, id: ProviderId) -> Result<()> {
        let saved = self.saved_provider(id)?;
        let _ = self
            .policy
            .unregister_destination(&destination_id(&saved.key));
        if let Ok(destination) = destination(&saved.key, &saved.settings)
            && let Some(binding) = destination.credential()
        {
            match self.credentials.delete(binding.key()).await {
                Ok(()) | Err(CredentialError::Missing) => {}
                Err(error) => {
                    return Err(InferenceError::new(
                        ErrorCode::CredentialUnavailable,
                        format!(
                            "The provider's API key could not be removed from secure storage. {}",
                            error.message()
                        ),
                    ));
                }
            }
        }
        let _operation = self.reserve()?;
        database(&self.database)?.delete_provider(id)
    }

    async fn provider_view(&self, saved: SavedProvider) -> Result<ProviderView> {
        let destination = destination(&saved.key, &saved.settings)?;
        let credential = match destination.credential() {
            Some(binding) => Some(
                self.credentials
                    .status(binding.key())
                    .await
                    .unwrap_or(CredentialStatus::Unavailable),
            ),
            None => None,
        };
        let period = crate::usage::current_period();
        let (usage, intelligence) = {
            let db = database(&self.database)?;
            (
                db.usage_summary(saved.id, &period)?,
                db.intelligence_settings()?,
            )
        };
        let readiness = if intelligence.local_only {
            Readiness::LocalOnly
        } else if saved.settings.retention == ProviderRetention::Unknown {
            Readiness::NeedsRetention
        } else {
            match credential {
                None | Some(CredentialStatus::Stored) => Readiness::Ready,
                Some(CredentialStatus::Missing) => Readiness::NeedsCredential,
                Some(CredentialStatus::Locked | CredentialStatus::Unavailable) => {
                    Readiness::CredentialUnavailable
                }
            }
        };
        let offer = offer(&saved);
        Ok(ProviderView {
            id: saved.id,
            origin: destination.endpoint().origin(),
            network: destination.scope().label(),
            encrypted: destination.endpoint().scheme() == Scheme::Https,
            credential,
            period,
            usage,
            modalities: offer.input_modalities.to_vec(),
            readiness,
            active: intelligence.active_provider == Some(saved.id),
            settings: saved.settings,
        })
    }

    pub async fn list_providers(&self) -> Result<Vec<ProviderView>> {
        let providers = database(&self.database)?.providers()?;
        let mut views = Vec::with_capacity(providers.len());
        for saved in providers {
            views.push(self.provider_view(saved).await?);
        }
        Ok(views)
    }

    pub async fn provider(&self, id: ProviderId) -> Result<ProviderView> {
        let saved = self.saved_provider(id)?;
        self.provider_view(saved).await
    }

    /// List the provider's models through the network boundary. This is an
    /// explicit user action and sends no personal data.
    pub async fn test_provider(&self, id: ProviderId) -> Result<ProviderTest> {
        let saved = self.saved_provider(id)?;
        let destination = destination(&saved.key, &saved.settings)?;
        let session = Session::new(Cancellation::default());
        let request = Outbound::new(
            &session,
            DISCOVER,
            "provider-discovery",
            destination.locality(),
            destination,
            Method::Get,
            openai::MODELS,
            Classified::from_source(Vec::new(), PROVIDER_CONTROL),
            Retention::None,
        );
        let until = Instant::now() + Duration::from_secs(30);
        let process = self
            .policy
            .grant(Permission::Process, request.operation(), until)
            .map_err(discovery_denied)?;
        let share = self
            .policy
            .grant(Permission::Share, request.operation(), until)
            .map_err(discovery_denied)?;
        let started = Instant::now();
        let mut streaming = self
            .boundary
            .send(request, process, share)
            .await
            .map_err(|failed| failure_error(&failed.failure))?;
        let mut body = Vec::new();
        while let Some(piece) = streaming.next().await {
            let piece = piece.map_err(|failure| failure_error(&failure))?;
            if body.len() + piece.len() > openai::MAX_JSON_BYTES {
                return Err(InferenceError::new(
                    ErrorCode::ProviderInvalidResponse,
                    "The provider's model list is too large.",
                ));
            }
            body.extend(piece);
        }
        let status = streaming.status;
        streaming.finish().await;
        session.close();
        if !(200..300).contains(&status) {
            return Err(openai::error_response(status, &body));
        }
        let models = openai::model_ids(&body).unwrap_or_default();
        let model_listed = (!models.is_empty()).then(|| models.contains(&saved.settings.model));
        Ok(ProviderTest {
            status,
            models,
            model_listed,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    }
}

pub(crate) fn remote_target(
    db: &std::sync::Mutex<storage::SqliteStore>,
    settings: IntelligenceSettings,
) -> Result<Option<(SavedProvider, Destination)>> {
    let Some(id) = settings.remote() else {
        return Ok(None);
    };
    let saved = database(db)?.provider(id)?;
    let destination = destination(&saved.key, &saved.settings)?;
    Ok(Some((saved, destination)))
}

fn discovery_denied(denial: Denial) -> InferenceError {
    failure_error(&Failure::from_denial(denial))
}

/// Locality of a destination as shown to people.
pub fn locality_label(locality: Locality) -> &'static str {
    match locality {
        Locality::OnDevice => "on this device",
        Locality::PairedDevice => "paired device",
        Locality::SelfHosted => "self-hosted",
        Locality::ThirdParty => "cloud",
    }
}
