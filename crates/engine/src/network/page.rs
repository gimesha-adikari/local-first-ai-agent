//! The only constructor for public webpage requests. A research gesture
//! authorizes a bounded number of HTTPS GET requests to public addresses for
//! one session. Every hop, including each redirect, is a separate operation
//! with its own one-handoff grants, resolved and pinned by the transport to
//! validated public addresses. No credential, cookie, referrer or personal
//! header is ever attached.

use std::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use agent_core::{
    capability::{Locality, Retention, WEB_PAGE},
    classification::Labels,
    inference::Cancellation,
    intelligence::EgressRecord,
    research::{MAX_PAGE_BYTES, MAX_REDIRECTS, MAX_URL_CHARS},
};
use url::Url;

use super::{
    Destination, Endpoint, Failed, Failure, Method, NetworkBoundary, Outbound, RequestBinding,
    Route, sha256,
};
use crate::policy::{Connectivity, Denial, Operation, Permission, Policy, Session};

/// Hard transport bound for one page response. The reader stops earlier, at
/// [`MAX_PAGE_BYTES`], and aborts the transfer.
pub(super) const MAX_BODY: usize = 8 * 1024 * 1024;
/// Whole exchange for one hop.
pub(super) const TOTAL: Duration = Duration::from_secs(25);
/// Response head and idle reads.
pub(super) const HEAD: Duration = Duration::from_secs(15);
/// The destination identifier recorded for every public page request.
pub(crate) const DESTINATION: &str = "web/public-page";
const PURPOSE: &str = "web-page";

/// Names that are never public internet hosts.
const SPECIAL_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "lan",
    "home",
    "corp",
    "internal",
    "intranet",
    "private",
    "localdomain",
    "home.arpa",
    "arpa",
    "test",
    "example",
    "invalid",
    "onion",
    "alt",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicUrlError {
    Invalid,
    /// Only HTTPS pages are read in this version.
    InsecureScheme,
    Scheme,
    Credentials,
    /// Only the default HTTPS port is accepted.
    Port,
    /// An address literal, a local name or a reserved name.
    Host,
    TooLong,
}

impl fmt::Display for PublicUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Enter a complete https:// webpage address.",
            Self::InsecureScheme => "Only https:// webpages can be read.",
            Self::Scheme => "Only https:// webpages can be read.",
            Self::Credentials => "Remove the user name or password from the address.",
            Self::Port => "Only webpages on the standard HTTPS port can be read.",
            Self::Host => {
                "Only public websites can be read, not addresses, local names or reserved names."
            }
            Self::TooLong => "The webpage address is too long.",
        })
    }
}

/// A canonical public HTTPS URL: a public-looking DNS name on port 443,
/// without credentials or fragment. Resolution and address checks happen
/// later, in the transport, for every connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrl(Url);

impl PublicUrl {
    pub fn parse(input: &str) -> Result<Self, PublicUrlError> {
        let input = input.trim();
        if input.is_empty()
            || input.len() > MAX_URL_CHARS
            || input
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
            return Err(PublicUrlError::Invalid);
        }
        Self::from_url(Url::parse(input).map_err(|_| PublicUrlError::Invalid)?)
    }

    fn from_url(mut url: Url) -> Result<Self, PublicUrlError> {
        match url.scheme() {
            "https" => {}
            "http" => return Err(PublicUrlError::InsecureScheme),
            _ => return Err(PublicUrlError::Scheme),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PublicUrlError::Credentials);
        }
        if url.port().is_some() {
            return Err(PublicUrlError::Port);
        }
        let host = match url.host() {
            Some(url::Host::Domain(name)) => name.to_owned(),
            Some(_) => return Err(PublicUrlError::Host),
            None => return Err(PublicUrlError::Invalid),
        };
        let name = host.strip_suffix('.').unwrap_or(&host);
        if !public_name(name) {
            return Err(PublicUrlError::Host);
        }
        if name.len() != host.len() {
            url.set_host(Some(name))
                .map_err(|_| PublicUrlError::Invalid)?;
        }
        url.set_fragment(None);
        if url.as_str().len() > MAX_URL_CHARS {
            return Err(PublicUrlError::TooLong);
        }
        if !url.as_str().is_ascii() || !url.path().starts_with('/') {
            return Err(PublicUrlError::Invalid);
        }
        Ok(Self(url))
    }

    /// Resolve a redirect target against this URL and validate it as a new,
    /// independent public URL. A downgrade to http is refused.
    pub fn redirect(&self, location: &str) -> Result<Self, PublicUrlError> {
        if location.len() > MAX_URL_CHARS {
            return Err(PublicUrlError::TooLong);
        }
        Self::from_url(self.0.join(location).map_err(|_| PublicUrlError::Invalid)?)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    pub fn has_query(&self) -> bool {
        self.0.query().is_some()
    }

    /// The URL without its query string, for records and saved citations.
    pub fn without_query(&self) -> String {
        format!("https://{}{}", self.host(), self.0.path())
    }

    /// The request target: path and query.
    fn target(&self) -> String {
        self.0[url::Position::BeforePath..url::Position::AfterQuery].to_owned()
    }
}

fn public_name(name: &str) -> bool {
    if !super::endpoint::valid_domain(name) {
        return false;
    }
    let labels: Vec<&str> = name.split('.').collect();
    let Some(tld) = labels.last() else {
        return false;
    };
    labels.len() >= 2
        && (tld.bytes().all(|b| b.is_ascii_lowercase()) || tld.starts_with("xn--"))
        && !SPECIAL_SUFFIXES
            .iter()
            .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Binding {
    pub epoch: u64,
    target_sha256: [u8; 32],
}

pub(super) struct Target {
    pub path: String,
}

impl Target {
    pub(super) fn matches(&self, binding: &Binding) -> bool {
        sha256(self.path.as_bytes()) == binding.target_sha256
    }

    pub(super) fn recorded_path(&self) -> String {
        let path = self.path.split(['?', '#']).next().unwrap_or("/");
        let mut end = path.len().min(256);
        while !path.is_char_boundary(end) {
            end -= 1;
        }
        path[..end].to_owned()
    }
}

/// The trusted, bounded authorization for one research request. It owns the
/// policy session: cancellation, Local-only mode or dropping the consent ends
/// every request made under it.
pub(crate) struct ResearchConsent {
    session: Session,
    epoch: u64,
    expires: Instant,
    requests: AtomicUsize,
}

impl ResearchConsent {
    pub fn new(
        policy: &Policy,
        cancel: Cancellation,
        requests: usize,
        lifetime: Duration,
    ) -> Result<Self, Failure> {
        let epoch = policy.restriction_epoch();
        if policy.is_local_only() {
            return Err(Failure::Denied(Denial::Locality));
        }
        Ok(Self {
            session: Session::new(cancel),
            epoch,
            expires: Instant::now() + lifetime,
            requests: AtomicUsize::new(requests),
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn expires(&self) -> Instant {
        self.expires
    }

    pub fn check(&self, policy: &Policy) -> Result<(), Failure> {
        if self.session.cancellation().is_cancelled() {
            return Err(Failure::Cancelled);
        }
        if policy.is_local_only() || self.epoch != policy.restriction_epoch() {
            return Err(Failure::Denied(Denial::Locality));
        }
        if Instant::now() >= self.expires {
            return Err(Failure::TimedOut);
        }
        Ok(())
    }

    /// Consume one request from the disclosed budget.
    pub fn take(&self) -> Result<(), Failure> {
        self.requests
            .try_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                left.checked_sub(1)
            })
            .map(|_| ())
            .map_err(|_| Failure::Denied(Denial::Scope))
    }
}

impl Drop for ResearchConsent {
    fn drop(&mut self) {
        self.session.close();
    }
}

/// Why a page could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageError {
    Network(Failure),
    Url(PublicUrlError),
    Status(u16),
    Unsupported(String),
    TooManyRedirects,
}

pub(crate) struct FetchedPage {
    pub url: PublicUrl,
    pub content_type: String,
    pub body: Vec<u8>,
    /// The body was cut at [`MAX_PAGE_BYTES`].
    pub truncated: bool,
}

fn destination(url: &PublicUrl) -> Result<Destination, Failure> {
    let endpoint =
        Endpoint::parse(&format!("https://{}", url.host())).map_err(|_| Failure::Address)?;
    Destination::new(DESTINATION, endpoint, Locality::ThirdParty).map_err(|_| Failure::Address)
}

fn supported(content_type: Option<&str>) -> Result<String, PageError> {
    let value = content_type.unwrap_or_default().to_ascii_lowercase();
    let essence = value.split(';').next().unwrap_or_default().trim();
    if matches!(
        essence,
        "text/html" | "application/xhtml+xml" | "text/plain"
    ) {
        Ok(value)
    } else if essence.is_empty() {
        Err(PageError::Unsupported("an undeclared type".into()))
    } else {
        Err(PageError::Unsupported(essence.chars().take(64).collect()))
    }
}

impl NetworkBoundary {
    /// Read one public page under `consent`. `labels` are the classes of the
    /// URL itself: [`agent_core::classification::PUBLIC_WEB`] for a search
    /// result, plus the user's input class for an address they typed. Every
    /// attempted hop is reported to `record`, including refused ones.
    pub(crate) async fn fetch_page(
        &self,
        consent: &ResearchConsent,
        url: PublicUrl,
        labels: &Labels,
        mut record: impl FnMut(EgressRecord),
    ) -> Result<FetchedPage, PageError> {
        let mut url = url;
        for hop in 0..=MAX_REDIRECTS {
            consent.check(&self.policy).map_err(PageError::Network)?;
            consent.take().map_err(PageError::Network)?;
            #[cfg(not(test))]
            let destination = destination(&url).map_err(PageError::Network)?;
            #[cfg(test)]
            let destination = self.page_destination(&url).map_err(PageError::Network)?;
            let target = Target { path: url.target() };
            let binding = RequestBinding {
                method: Method::Get,
                route: Route::new("/"),
                body_sha256: sha256(&[]),
                body_len: 0,
                artifact: None,
                page: Some(Binding {
                    epoch: consent.epoch,
                    target_sha256: sha256(target.path.as_bytes()),
                }),
            };
            let outbound = Outbound {
                operation: Operation::new(
                    &consent.session,
                    WEB_PAGE,
                    PURPOSE,
                    labels.clone(),
                    Locality::ThirdParty,
                    Connectivity::ThroughBoundary,
                    Some(destination),
                    Some(binding),
                    Retention::ProviderDeclared,
                )
                .into(),
                body: Vec::new(),
                content_type: None,
                provider_retention: Retention::ProviderDeclared,
                artifact_target: None,
                page_target: Some(target),
                #[cfg(test)]
                query: None,
            };
            let grants = self
                .policy
                .grant(Permission::Process, outbound.operation(), consent.expires)
                .and_then(|process| {
                    match self.policy.grant(
                        Permission::Share,
                        outbound.operation(),
                        consent.expires,
                    ) {
                        Ok(share) => Ok((process, share)),
                        Err(denial) => {
                            let _ = self.policy.revoke(process);
                            Err(denial)
                        }
                    }
                });
            let (process, share) = match grants {
                Ok(grants) => grants,
                Err(denial) => {
                    record(outbound.record());
                    return Err(PageError::Network(Failure::from_denial(denial)));
                }
            };
            let sent = self.send(outbound, process, share).await;
            let revoke = || {
                let _ = self.policy.revoke(process);
                let _ = self.policy.revoke(share);
            };
            let mut stream = match sent {
                Ok(stream) => stream,
                Err(failed) => {
                    revoke();
                    let Failed {
                        failure,
                        record: attempt,
                    } = *failed;
                    record(attempt);
                    let Failure::Redirect { status, location } = failure else {
                        return Err(PageError::Network(failure));
                    };
                    if hop == MAX_REDIRECTS {
                        return Err(PageError::TooManyRedirects);
                    }
                    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                        return Err(PageError::Status(status));
                    }
                    let location = location.ok_or(PageError::Url(PublicUrlError::Invalid))?;
                    url = url.redirect(&location).map_err(PageError::Url)?;
                    continue;
                }
            };
            let status = stream.status;
            let checked = if (200..300).contains(&status) {
                supported(stream.content_type.as_deref())
            } else {
                Err(PageError::Status(status))
            };
            let content_type = match checked {
                Ok(content_type) => content_type,
                Err(error) => {
                    record(stream.finish().await);
                    revoke();
                    return Err(error);
                }
            };
            let mut body = Vec::new();
            let mut truncated = false;
            let mut failure = None;
            while let Some(piece) = stream.next().await {
                match piece {
                    Ok(piece) => {
                        let room = MAX_PAGE_BYTES - body.len();
                        if piece.len() >= room {
                            body.extend_from_slice(&piece[..room]);
                            // Whatever follows is not read: treat it as partial.
                            truncated = true;
                            break;
                        }
                        body.extend(piece);
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            let (attempt, live) = stream.finish_checked().await;
            record(attempt);
            revoke();
            if let Some(failure) = failure.or(live.err()) {
                return Err(PageError::Network(failure));
            }
            return Ok(FetchedPage {
                url,
                content_type,
                body,
                truncated,
            });
        }
        Err(PageError::TooManyRedirects)
    }

    #[cfg(test)]
    fn page_destination(&self, url: &PublicUrl) -> Result<Destination, Failure> {
        let mut destination = destination(url)?;
        if let Some(port) = *self.page_fixture.lock().unwrap() {
            destination.endpoint =
                Endpoint::parse(&format!("https://{}:{port}", url.host())).unwrap();
            destination.scope = super::NetworkScope::Loopback;
        }
        Ok(destination)
    }

    /// Route public pages to a loopback TLS fixture on `port`.
    #[cfg(test)]
    pub(crate) fn route_pages_to_fixture(&self, port: Option<u16>) {
        *self.page_fixture.lock().unwrap() = port;
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
