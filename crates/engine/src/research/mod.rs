//! Web research: search provider configuration, the bounded single-pass
//! pipeline's network steps, transient result handling and validated source
//! views. The Chat turn that drives it lives with Chat orchestration.
//!
//! Separation of concerns:
//! - [`providers`] builds search requests and normalizes results;
//! - [`crate::network::page`] retrieves public pages under a scoped consent;
//! - [`extract`] turns untrusted HTML into bounded text;
//! - [`rank`] selects relevant evidence deterministically;
//! - Chat assembles context, runs inference and validates citations.

pub(crate) mod extract;
pub(crate) mod providers;
pub(crate) mod rank;

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use agent_core::{
    capability::{Retention, WEB_SEARCH},
    chat::MessageId,
    chat_context::{MAX_CHAT_SOURCES, citation_id, cited_slots},
    classification::{Classified, Labels, QUERY, USER_INPUT},
    inference::{ErrorCode, InferenceError, Result},
    intelligence::EgressRecord,
    research::{
        EvidenceKind, MAX_SEARCH_BYTES, ResearchMode, ResearchRecord, SearchProviderKind,
        SearchQuery, SearchResult, WebResearchSettings,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    Engine,
    credentials::{CredentialKey, CredentialStatus},
    database,
    network::{
        Failed, Failure, NetworkBoundary, Outbound,
        page::{PageError, PublicUrl, PublicUrlError, ResearchConsent},
    },
    policy::{Permission, Policy},
};

/// One explicitly authorized research request, sent with a Chat message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResearchOptions {
    pub mode: ResearchMode,
    /// The provider the user saw; it must still be the configured one.
    #[serde(default)]
    pub provider: Option<SearchProviderKind>,
    /// The user's explicit edit of the outgoing query. Omitted means the
    /// message itself is the query.
    #[serde(default)]
    pub query: Option<String>,
    /// Attachments whose recognized or extracted text the query was built
    /// from (visual-assisted search, Stage 04-E). Their classes are added to
    /// the query's labels; nothing of the attachments themselves is sent.
    #[serde(default)]
    pub query_attachments: Vec<String>,
    /// The outgoing derived query the user explicitly reviewed.
    #[serde(default)]
    pub reviewed_query: Option<String>,
    /// The public page to read in page mode.
    #[serde(default)]
    pub url: Option<String>,
    /// List results without asking any model.
    #[serde(default)]
    pub results_only: bool,
    /// Separate authorization to send the question and selected excerpts to
    /// the selected remote or self-hosted model provider for this request.
    #[serde(default)]
    pub share_with_provider: bool,
}

/// What the settings surface shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebResearchView {
    pub settings: WebResearchSettings,
    pub brave_key: Option<CredentialStatus>,
    /// The canonical SearXNG URL and its network class, when configured.
    pub searxng_origin: Option<String>,
    pub searxng_network: Option<&'static str>,
    pub searxng_error: Option<String>,
}

/// A source in citation-slot order, merged from the saved record and, while
/// this app session lasts, the transient details.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSourceView {
    pub citation_id: String,
    pub evidence: EvidenceKind,
    /// Included in the fitted model context.
    pub supplied: bool,
    /// The answer contains a valid citation marker for it.
    pub cited: bool,
    pub title: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub published: Option<String>,
    /// Evidence text; transient, never saved.
    pub excerpt: Option<String>,
    pub saved: bool,
    pub openable: bool,
}

/// A search result that was discovered but not supplied as a source.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredView {
    pub title: String,
    pub url: String,
    pub host: String,
    pub snippet: String,
    pub published: Option<String>,
    /// Retrieval was attempted for this result.
    pub attempted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchView {
    pub record: ResearchRecord,
    pub sources: Vec<WebSourceView>,
    pub discovered: Vec<DiscoveredView>,
    pub limitations: Vec<String>,
    pub query: Option<String>,
    /// Transient details from this app session are still available.
    pub transient: bool,
}

/// Details kept only in memory for the life of the app process.
#[derive(Debug, Clone, Default)]
pub(crate) struct Transient {
    pub query: Option<String>,
    pub sources: Vec<TransientSource>,
    pub discovered: Vec<DiscoveredView>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TransientSource {
    pub title: String,
    pub url: String,
    pub host: String,
    pub published: Option<String>,
    pub excerpt: String,
}

const TRANSIENT_ENTRIES: usize = 32;

type Entries = VecDeque<(MessageId, Arc<Transient>)>;

/// Bounded in-memory research details, keyed by assistant message.
#[derive(Default, Clone)]
pub(crate) struct TransientResults(Arc<Mutex<Entries>>);

impl TransientResults {
    pub fn put(&self, message: MessageId, details: Transient) {
        if let Ok(mut entries) = self.0.lock() {
            entries.retain(|(id, _)| *id != message);
            if entries.len() == TRANSIENT_ENTRIES {
                entries.pop_front();
            }
            entries.push_back((message, Arc::new(details)));
        }
    }

    pub fn get(&self, message: MessageId) -> Option<Arc<Transient>> {
        self.0
            .lock()
            .ok()?
            .iter()
            .find(|(id, _)| *id == message)
            .map(|(_, details)| Arc::clone(details))
    }

    /// Drop every transient detail, for example when a conversation is
    /// deleted.
    pub fn clear(&self) {
        if let Ok(mut entries) = self.0.lock() {
            entries.clear();
        }
    }
}

/// Which link the user asked to open.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "index")]
pub enum LinkTarget {
    Source(usize),
    Discovered(usize),
}

fn invalid(message: &str) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

/// Map a transport failure on a search request.
pub(crate) fn search_failure(provider: SearchProviderKind, failure: &Failure) -> InferenceError {
    let name = provider.label();
    match failure {
        Failure::Credential(crate::credentials::CredentialError::Missing) => InferenceError::new(
            ErrorCode::CredentialMissing,
            "Add your Brave Search API key in Settings → Web search.",
        ),
        Failure::Cancelled => InferenceError::new(ErrorCode::Cancelled, "Research was stopped."),
        Failure::Resolve | Failure::Connect | Failure::Io => InferenceError::new(
            ErrorCode::NetworkFailed,
            match provider {
                SearchProviderKind::Searxng => {
                    format!("Couldn't reach {name}. Check your connection or the instance address.")
                }
                _ => format!("Couldn't reach {name}. Check your connection and try again."),
            },
        ),
        Failure::Address => InferenceError::new(
            ErrorCode::PolicyDenied,
            format!(
                "{name} resolved to an address outside its allowed network, so nothing was sent."
            ),
        ),
        Failure::Redirect { status, .. } => InferenceError::new(
            ErrorCode::ProviderRejected,
            format!(
                "{name} redirected the search (HTTP {status}); redirects are not followed. Check the address."
            ),
        ),
        other => crate::providers::failure_error(other),
    }
}

/// A short, honest description of why a page wasn't used.
pub(crate) fn page_limitation(host: &str, error: &PageError) -> String {
    let reason = match error {
        PageError::Network(Failure::Address) => {
            "resolved to a private or disallowed network address, so it was not contacted".into()
        }
        PageError::Network(Failure::Denied(_)) => "was refused by the research policy".into(),
        PageError::Network(Failure::Cancelled) => "was not read because research stopped".into(),
        PageError::Network(Failure::TimedOut) => "did not respond in time".into(),
        PageError::Network(Failure::Tls) => {
            "has a TLS certificate that couldn't be verified".into()
        }
        PageError::Network(Failure::TooLarge) => "is larger than this app reads".into(),
        PageError::Network(Failure::Protocol) => "sent a malformed response".into(),
        PageError::Network(_) => "couldn't be reached".into(),
        PageError::Url(PublicUrlError::InsecureScheme) => {
            "redirected to an insecure http:// address, which isn't followed".into()
        }
        PageError::Url(error) => format!("redirected to an address that can't be read ({error})"),
        PageError::Status(401 | 403) => {
            "requires sign-in or blocks automated reading (access restricted)".into()
        }
        PageError::Status(402 | 451) => "is access-restricted".into(),
        PageError::Status(404 | 410) => "was not found".into(),
        PageError::Status(429) => "is rate-limiting requests".into(),
        PageError::Status(status) => format!("returned HTTP {status}"),
        PageError::Unsupported(kind) => format!("is {kind} content, which isn't read"),
        PageError::TooManyRedirects => "redirected too many times".into(),
    };
    format!("{host} {reason}.")
}

impl NetworkBoundary {
    /// Run one search under `consent`. Only the validated query is sent; it
    /// carries the classes of everything it was derived from (`sources`:
    /// the user's input, and recognized screen, camera or attachment text
    /// when the query came from them). Every attempt is reported.
    pub(crate) async fn search(
        &self,
        consent: &ResearchConsent,
        provider: SearchProviderKind,
        query: &SearchQuery,
        sources: &Labels,
        records: &mut Vec<EgressRecord>,
    ) -> Result<Vec<SearchResult>> {
        let policy = self.authority();
        consent
            .check()
            .map_err(|failure| search_failure(provider, &failure))?;
        consent
            .take()
            .map_err(|failure| search_failure(provider, &failure))?;
        let id = match provider {
            SearchProviderKind::Brave => providers::BRAVE_DESTINATION,
            SearchProviderKind::Searxng => providers::SEARXNG_DESTINATION,
            SearchProviderKind::Purili => providers::PURILI_DESTINATION,
        };
        let destination = policy.registered_destination(id).ok_or_else(|| {
            invalid(match provider {
                SearchProviderKind::Brave => "Brave Search is not available.",
                SearchProviderKind::Searxng => {
                    "Configure your SearXNG instance in Settings → Web search."
                }
                SearchProviderKind::Purili => "Purili is not available.",
            })
        })?;
        let request = providers::request(provider, query);
        let purpose = if sources.ids().any(|id| id != USER_INPUT.id) {
            crate::policy::VISUAL_SEARCH_PURPOSE
        } else {
            "web-search"
        };
        let outbound = Outbound::new(
            consent.session(),
            WEB_SEARCH,
            purpose,
            destination.locality(),
            destination,
            request.method,
            request.route,
            Classified::derived(request.body, [sources], QUERY),
            Retention::ProviderDeclared,
        )
        .with_content_type(request.content_type);
        let until = consent.expires();
        let process = policy
            .grant(Permission::Process, outbound.operation(), until)
            .map_err(|denial| search_failure(provider, &Failure::from_denial(denial)))?;
        let share = match policy.grant(Permission::Share, outbound.operation(), until) {
            Ok(share) => share,
            Err(denial) => {
                let _ = policy.revoke(process);
                return Err(search_failure(provider, &Failure::from_denial(denial)));
            }
        };
        let revoke = || {
            let _ = policy.revoke(process);
            let _ = policy.revoke(share);
        };
        let mut stream = match self.send(outbound, process, share).await {
            Ok(stream) => stream,
            Err(failed) => {
                revoke();
                let Failed { failure, record } = *failed;
                records.push(record);
                return Err(search_failure(provider, &failure));
            }
        };
        let mut body = Vec::new();
        let mut failure = None;
        while let Some(piece) = stream.next().await {
            match piece {
                Ok(piece) if body.len() + piece.len() <= MAX_SEARCH_BYTES => body.extend(piece),
                Ok(_) => {
                    failure = Some(Failure::TooLarge);
                    break;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let status = stream.status;
        let (record, live) = stream.finish_checked().await;
        records.push(record);
        revoke();
        if let Some(failure) = failure.or(live.err()) {
            return Err(search_failure(provider, &failure));
        }
        if !(200..300).contains(&status) {
            return Err(providers::error(provider, status, &body));
        }
        providers::results(provider, &body)
    }
}

/// One result of a visual-assisted search. Untrusted provider data, held
/// only by the panel session that asked for it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualSearchResult {
    pub title: String,
    pub url: String,
    pub host: String,
    pub snippet: String,
    pub published: Option<String>,
}

/// A visual-assisted text search (Stage 04-E): the reviewed query, the
/// results and the transport records of the one request that was made.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualSearch {
    pub provider: SearchProviderKind,
    pub provider_name: &'static str,
    pub query: String,
    pub results: Vec<VisualSearchResult>,
    pub egress: Vec<EgressRecord>,
}

/// A link from a search result, revalidated as a public HTTPS URL before
/// the host opens it.
pub fn validated_link(url: &str) -> Result<String> {
    PublicUrl::parse(url)
        .map(|url| url.as_str().to_owned())
        .map_err(|error| InferenceError::new(ErrorCode::InvalidConfig, error.to_string()))
}

/// Register the fixed Brave and Purili destinations and any configured
/// SearXNG instance. Registration grants nothing by itself.
pub(crate) fn register(policy: &Policy, settings: &WebResearchSettings) {
    #[cfg(not(test))]
    for destination in [
        providers::brave_destination(),
        providers::purili_destination(),
    ] {
        let _ = policy.register_destination(destination);
    }
    let _ = policy.unregister_destination(providers::SEARXNG_DESTINATION);
    if !settings.searxng_endpoint.trim().is_empty()
        && let Ok(destination) = providers::searxng_destination(&settings.searxng_endpoint)
    {
        let _ = policy.register_destination(destination);
    }
}

fn brave_key(policy: &Policy) -> Option<CredentialKey> {
    policy
        .registered_destination(providers::BRAVE_DESTINATION)?
        .credential()
        .map(|binding| binding.key().clone())
}

impl Engine {
    /// Search the web with a query the user reviewed and authorized for
    /// this one request. `sources` are the classes of what the query was
    /// built from (recognized screen or camera text, a model's description).
    /// Only the query text is sent: never an image, document, memory or
    /// conversation. One search, no page reads and no model.
    pub async fn visual_search(
        &self,
        query: &str,
        provider_seen: Option<SearchProviderKind>,
        sources: Labels,
        cancellation: agent_core::inference::Cancellation,
    ) -> Result<VisualSearch> {
        let settings = database(&self.database)?.web_research_settings()?;
        settings.validate()?;
        let provider = settings.provider.ok_or_else(|| {
            invalid("Choose a search provider in Settings → Web search to search the web.")
        })?;
        if provider_seen != Some(provider) {
            return Err(invalid(
                "The search provider changed. Review the search and authorize it again.",
            ));
        }
        let query = SearchQuery::new(query)?;
        let consent = ResearchConsent::new(cancellation, 1, std::time::Duration::from_secs(120));
        let mut egress = Vec::new();
        let results = self
            .boundary
            .search(&consent, provider, &query, &sources, &mut egress)
            .await;
        let results = results?
            .into_iter()
            .filter_map(|result| {
                let url = PublicUrl::parse(&result.url).ok()?;
                Some(VisualSearchResult {
                    host: url.host().to_owned(),
                    url: url.as_str().to_owned(),
                    title: result.title,
                    snippet: result.snippet,
                    published: result.published,
                })
            })
            .collect();
        Ok(VisualSearch {
            provider,
            provider_name: provider.label(),
            query: query.as_str().to_owned(),
            results,
            egress,
        })
    }

    pub async fn web_research(&self) -> Result<WebResearchView> {
        let settings = database(&self.database)?.web_research_settings()?;
        let brave_key = match brave_key(&self.policy) {
            Some(key) => Some(
                self.credentials
                    .status(&key)
                    .await
                    .unwrap_or(CredentialStatus::Unavailable),
            ),
            None => None,
        };
        let searxng = (!settings.searxng_endpoint.trim().is_empty())
            .then(|| providers::searxng_destination(&settings.searxng_endpoint));
        Ok(WebResearchView {
            searxng_origin: searxng
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|destination| destination.endpoint().display()),
            searxng_network: searxng
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|destination| destination.scope().label()),
            searxng_error: searxng
                .and_then(std::result::Result::err)
                .map(|error| error.to_string()),
            settings,
            brave_key,
        })
    }

    /// Validate, canonicalize and save search configuration.
    pub async fn save_web_research(
        &self,
        mut settings: WebResearchSettings,
    ) -> Result<WebResearchView> {
        settings.validate()?;
        let endpoint = settings.searxng_endpoint.trim().to_owned();
        settings.searxng_endpoint = if endpoint.is_empty() {
            String::new()
        } else {
            providers::searxng_destination(&endpoint)
                .map_err(|error| invalid(&error.to_string()))?
                .endpoint()
                .display()
        };
        {
            let _operation = self.reserve()?;
            database(&self.database)?.save_web_research_settings(&settings)?;
            register(&self.policy, &settings);
        }
        self.web_research().await
    }

    /// The secure-storage key for the Brave Search API key, bound to the
    /// Brave origin.
    pub fn brave_credential_key(&self) -> Result<CredentialKey> {
        brave_key(&self.policy).ok_or_else(|| invalid("Brave Search is not available."))
    }

    pub async fn store_brave_key(&self, secret: crate::credentials::Secret) -> Result<()> {
        let key = self.brave_credential_key()?;
        let _operation = self.reserve()?;
        self.credentials
            .store(&key, "Brave Search", secret)
            .await
            .map_err(|error| crate::providers::failure_error(&Failure::Credential(error)))
    }

    pub async fn delete_brave_key(&self) -> Result<()> {
        let key = self.brave_credential_key()?;
        let _operation = self.reserve()?;
        match self.credentials.delete(&key).await {
            Ok(()) | Err(crate::credentials::CredentialError::Missing) => Ok(()),
            Err(error) => Err(crate::providers::failure_error(&Failure::Credential(error))),
        }
    }

    /// Validated sources for one research message. Citations are recomputed
    /// from the saved answer and the fitted source count, so only markers
    /// for sources actually supplied to the model count.
    pub fn web_research_view(&self, message: MessageId) -> Result<Option<ResearchView>> {
        let Some((content, context)) = database(&self.database)?.research_message(message)? else {
            return Ok(None);
        };
        let Some(record) = context.research else {
            return Ok(None);
        };
        let supplied = context
            .fitted
            .map_or(0, |usage| usage.sources)
            .min(record.sources.len())
            .min(MAX_CHAT_SOURCES);
        let cited = cited_slots(&content, supplied);
        let transient = self.research_results.get(message);
        let sources = record
            .sources
            .iter()
            .enumerate()
            .map(|(slot, saved)| {
                let live = transient
                    .as_ref()
                    .and_then(|details| details.sources.get(slot));
                let url = live.map(|live| live.url.clone()).or(saved.url.clone());
                WebSourceView {
                    citation_id: citation_id(slot),
                    evidence: saved.evidence,
                    supplied: slot < supplied,
                    cited: slot < supplied && cited.get(slot).copied().unwrap_or(false),
                    title: live.map(|live| live.title.clone()).or(saved.title.clone()),
                    openable: url
                        .as_deref()
                        .is_some_and(|url| PublicUrl::parse(url).is_ok()),
                    url,
                    host: live.map(|live| live.host.clone()).or(saved.host.clone()),
                    published: live.and_then(|live| live.published.clone()),
                    excerpt: live.map(|live| live.excerpt.clone()),
                    saved: saved.saved,
                }
            })
            .collect();
        Ok(Some(ResearchView {
            sources,
            discovered: transient
                .as_ref()
                .map(|details| details.discovered.clone())
                .unwrap_or_default(),
            limitations: transient
                .as_ref()
                .map(|details| details.limitations.clone())
                .unwrap_or_default(),
            query: transient.as_ref().and_then(|details| details.query.clone()),
            transient: transient.is_some(),
            record,
        }))
    }

    /// The validated public URL behind a source or result card, taken only
    /// from trusted runtime records, never from the page or the model.
    pub fn web_research_link(&self, message: MessageId, target: LinkTarget) -> Result<String> {
        let view = self
            .web_research_view(message)?
            .ok_or_else(|| invalid("This message has no web sources."))?;
        let url = match target {
            LinkTarget::Source(index) => view
                .sources
                .get(index)
                .and_then(|source| source.url.clone()),
            LinkTarget::Discovered(index) => {
                view.discovered.get(index).map(|result| result.url.clone())
            }
        }
        .ok_or_else(|| invalid("This link is no longer available."))?;
        PublicUrl::parse(&url)
            .map(|url| url.as_str().to_owned())
            .map_err(|error| invalid(&error.to_string()))
    }
}
