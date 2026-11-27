//! Application-controlled egress. Provider adapters build immutable,
//! labelled requests and parse responses; they receive no transport handle,
//! socket, credential or redeemable permit. Every connection goes through
//! [`NetworkBoundary`], which checks the policy kernel before resolution,
//! again under the revocation lock immediately before the first request
//! byte, and while the request and response are in flight.

mod address;
pub(crate) mod artifact;
pub mod endpoint;
mod http;
pub(crate) mod page;
mod transport;

use std::{
    future::Future,
    io,
    net::IpAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_core::{
    capability::{CapabilityId, Locality, Retention},
    classification::{Classified, Labels},
    intelligence::{EgressRecord, Transmission},
};
use sha2::{Digest, Sha256};

pub use address::NetworkScope;
pub use endpoint::{Endpoint, EndpointError, Host, Scheme};

use crate::{
    credentials::{CredentialError, CredentialKey, CredentialSource},
    policy::{Connectivity, Denial, GrantId, Operation, Policy, Session},
};

/// How a bound credential is presented. Each provider documents exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialHeader {
    /// `Authorization: Bearer <key>`, as OpenAI-compatible servers expect.
    Bearer,
    /// `X-Subscription-Token: <key>`, the Brave Search API header.
    SubscriptionToken,
}

/// A provider credential bound to one provider and one canonical origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialBinding {
    key: CredentialKey,
    header: CredentialHeader,
}

impl CredentialBinding {
    pub fn key(&self) -> &CredentialKey {
        &self.key
    }

    pub fn header(&self) -> CredentialHeader {
        self.header
    }
}

/// A destination registered by the trusted provider configuration. Fields
/// are private: a value is constructed only from a canonical endpoint whose
/// scope agrees with its locality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    id: Arc<str>,
    endpoint: Endpoint,
    locality: Locality,
    scope: NetworkScope,
    credential: Option<CredentialBinding>,
}

impl Destination {
    pub fn new(id: &str, endpoint: Endpoint, locality: Locality) -> Result<Self, EndpointError> {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(EndpointError::Invalid);
        }
        let scope = endpoint.scope_for(locality)?;
        Ok(Self {
            id: Arc::from(id),
            endpoint,
            locality,
            scope,
            credential: None,
        })
    }

    /// Bind a credential stored for `provider` at this destination's exact
    /// origin. The transport attaches it only to connections to that origin.
    pub fn with_credential(self, provider: &str) -> Self {
        self.with_credential_header(provider, CredentialHeader::Bearer)
    }

    /// Bind a credential presented in `header`. The binding is to this exact
    /// origin, so the key can never reach a different server.
    pub fn with_credential_header(mut self, provider: &str, header: CredentialHeader) -> Self {
        self.credential = Some(CredentialBinding {
            key: CredentialKey {
                provider: provider.to_owned(),
                origin: self.endpoint.origin(),
            },
            header,
        });
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn shared_id(&self) -> Arc<str> {
        Arc::clone(&self.id)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn locality(&self) -> Locality {
        self.locality
    }

    pub fn scope(&self) -> &NetworkScope {
        &self.scope
    }

    pub fn credential(&self) -> Option<&CredentialBinding> {
        self.credential.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// A fixed route chosen by an adapter from its own constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route(&'static str);

impl Route {
    pub const fn new(path: &'static str) -> Self {
        Self(path)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }

    fn is_valid(self) -> bool {
        self.0.starts_with('/')
            && self.0.len() <= 128
            && !self.0.contains("//")
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._/".contains(&b))
            && !self
                .0
                .split('/')
                .any(|segment| segment == "." || segment == "..")
    }
}

/// The exact request an operation authorizes: method, route and the digest
/// of the immutable body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBinding {
    method: Method,
    route: Route,
    body_sha256: [u8; 32],
    body_len: usize,
    artifact: Option<artifact::Binding>,
    page: Option<page::Binding>,
}

impl RequestBinding {
    pub(crate) fn artifact_epoch(&self) -> Option<u64> {
        self.artifact.as_ref().map(|artifact| artifact.epoch)
    }

    pub(crate) fn page_epoch(&self) -> Option<u64> {
        self.page.as_ref().map(|page| page.epoch)
    }

    pub(crate) fn is_bodiless_get(&self) -> bool {
        self.method == Method::Get && self.body_len == 0
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.route.is_valid() && self.body_len <= MAX_REQUEST_BODY
    }
}

/// Upper bound for one request body.
pub const MAX_REQUEST_BODY: usize = 6 * 1024 * 1024;

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// An immutable labelled request. The body cannot be read or changed after
/// construction; the policy operation binds its digest.
pub struct Outbound {
    operation: Arc<Operation>,
    body: Vec<u8>,
    content_type: Option<&'static str>,
    provider_retention: Retention,
    artifact_target: Option<artifact::Target>,
    page_target: Option<page::Target>,
}

impl Outbound {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: &Session,
        capability: CapabilityId,
        purpose: &'static str,
        locality: Locality,
        destination: Destination,
        method: Method,
        route: Route,
        body: Classified<Vec<u8>>,
        provider_retention: Retention,
    ) -> Self {
        let (body, labels) = body.into_parts();
        let request = RequestBinding {
            method,
            route,
            body_sha256: sha256(&body),
            body_len: body.len(),
            artifact: None,
            page: None,
        };
        Self {
            operation: Arc::new(Operation::new(
                session,
                capability,
                purpose,
                labels,
                locality,
                Connectivity::ThroughBoundary,
                Some(destination),
                Some(request),
                provider_retention,
            )),
            content_type: (!body.is_empty()).then_some("application/json"),
            body,
            provider_retention,
            artifact_target: None,
            page_target: None,
        }
    }

    /// Send a non-empty body with this media type instead of JSON. The body
    /// digest stays bound by the operation.
    pub fn with_content_type(mut self, content_type: &'static str) -> Self {
        if !self.body.is_empty() {
            self.content_type = Some(content_type);
        }
        self
    }

    pub fn operation(&self) -> &Operation {
        &self.operation
    }

    pub fn labels(&self) -> &Labels {
        self.operation.labels()
    }

    fn destination(&self) -> &Destination {
        self.operation
            .destination()
            .expect("an outbound request always has a destination")
    }

    fn binding(&self) -> &RequestBinding {
        self.operation
            .request_binding()
            .expect("an outbound request always has a binding")
    }

    fn record(&self) -> EgressRecord {
        let destination = self.destination();
        let binding = self.binding();
        EgressRecord {
            destination: destination.id().to_owned(),
            origin: destination.endpoint().origin(),
            method: binding.method.as_str().to_owned(),
            route: match (&binding.artifact, &self.page_target) {
                (Some(artifact), _) => format!("artifact/{}", artifact.key),
                // The path only: a page's query string can be personal.
                (None, Some(target)) => target.recorded_path(),
                (None, None) => binding.route.as_str().to_owned(),
            },
            body_sha256: hex(&binding.body_sha256),
            body_bytes: binding.body_len as u64,
            credential: false,
            locality: destination.locality(),
            provider_retention: self.provider_retention,
            transmission: Transmission::Unsent,
            labels: self.operation.labels().clone(),
            authorization: Vec::new(),
        }
    }
}

/// Why an exchange produced no usable response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Denied(Denial),
    Credential(CredentialError),
    Resolve,
    /// A resolved address is outside the destination's scope.
    Address,
    Connect,
    Tls,
    TimedOut,
    Cancelled,
    /// A request, header or response bound was exceeded.
    TooLarge,
    /// The response could not be parsed as HTTP/1.1.
    Protocol,
    Io,
    /// A redirect is returned as data and never followed. The location is
    /// untrusted and bounded.
    Redirect {
        status: u16,
        location: Option<String>,
    },
    /// The transport worker stopped.
    Stopped,
}

impl Failure {
    pub(crate) fn from_denial(denial: Denial) -> Self {
        if denial == Denial::Cancelled {
            Self::Cancelled
        } else {
            Self::Denied(denial)
        }
    }
}

/// Bounds for one exchange.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub resolve: Duration,
    pub connect: Duration,
    pub handshake: Duration,
    pub credential: Duration,
    /// From the last request byte to the response head.
    pub head: Duration,
    /// Between response reads.
    pub idle: Duration,
    /// The whole exchange, including queueing.
    pub total: Duration,
    pub max_head: usize,
    pub max_body: usize,
    /// Largest piece delivered to the adapter.
    pub chunk: usize,
    /// Pieces buffered between the transport and the adapter.
    pub queue: usize,
    /// Exchanges in flight at once; others wait in the queue.
    pub concurrency: usize,
    /// How often in-flight work rechecks cancellation and grant liveness.
    pub tick: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            resolve: Duration::from_secs(5),
            connect: Duration::from_secs(10),
            handshake: Duration::from_secs(10),
            credential: Duration::from_secs(5),
            head: Duration::from_secs(90),
            idle: Duration::from_secs(90),
            total: Duration::from_secs(600),
            max_head: 64 * 1024,
            max_body: 16 * 1024 * 1024,
            chunk: 16 * 1024,
            queue: 8,
            concurrency: 4,
            tick: Duration::from_millis(10),
        }
    }
}

pub type ResolveFuture = Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;

/// Name resolution. The production resolver uses the system resolver on
/// the transport worker; tests substitute fixed answers.
pub trait Resolver: Send + Sync + 'static {
    fn resolve(&self, host: &str, port: u16) -> ResolveFuture;
}

pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> ResolveFuture {
        let host = host.to_owned();
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), port))
                .await?
                .map(|address| address.ip())
                .collect())
        })
    }
}

/// Trust anchors for TLS. Production uses the operating system's store.
pub struct TrustAnchors(Arc<rustls::RootCertStore>);

impl TrustAnchors {
    /// The system certificate store. Unreadable entries are skipped; an
    /// empty store makes every TLS connection fail.
    pub fn system() -> Self {
        let mut roots = rustls::RootCertStore::empty();
        let loaded = rustls_native_certs::load_native_certs();
        let _ = roots.add_parsable_certificates(loaded.certs);
        Self(Arc::new(roots))
    }

    #[cfg(test)]
    pub(crate) fn from_der(certificates: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        for der in certificates {
            roots
                .add(rustls::pki_types::CertificateDer::from(der))
                .expect("fixture root");
        }
        Self(Arc::new(roots))
    }
}

pub struct BoundaryConfig {
    pub resolver: Arc<dyn Resolver>,
    pub anchors: TrustAnchors,
    pub limits: Limits,
}

impl Default for BoundaryConfig {
    fn default() -> Self {
        Self {
            resolver: Arc::new(SystemResolver),
            anchors: TrustAnchors::system(),
            limits: Limits::default(),
        }
    }
}

/// A failed exchange and its truthful transport record.
#[derive(Debug)]
pub struct Failed {
    pub failure: Failure,
    pub record: EgressRecord,
}

/// The single network boundary. The transport worker starts on the first
/// admitted request, so local-only operation never creates it.
pub struct NetworkBoundary {
    policy: Arc<Policy>,
    credentials: Arc<dyn CredentialSource>,
    config: Arc<transport::Config>,
    worker: Mutex<Option<Arc<transport::Worker>>>,
    #[cfg(test)]
    page_fixture: Mutex<Option<u16>>,
}

impl NetworkBoundary {
    pub fn new(
        policy: Arc<Policy>,
        credentials: Arc<dyn CredentialSource>,
        config: BoundaryConfig,
    ) -> Self {
        Self {
            policy,
            credentials,
            config: Arc::new(transport::Config::new(config)),
            worker: Mutex::new(None),
            #[cfg(test)]
            page_fixture: Mutex::new(None),
        }
    }

    /// Kept with trusted consent handling. Provider adapters do not receive it.
    pub fn authority(&self) -> &Arc<Policy> {
        &self.policy
    }

    /// Whether the transport worker has ever started.
    pub fn started(&self) -> bool {
        self.worker.lock().is_ok_and(|worker| worker.is_some())
    }

    /// Connections the transport opened, for verification.
    pub fn connections(&self) -> u64 {
        self.config.connections()
    }

    fn worker(&self) -> Result<Arc<transport::Worker>, Failure> {
        let mut slot = self.worker.lock().map_err(|_| Failure::Stopped)?;
        if let Some(worker) = slot.as_ref() {
            return Ok(Arc::clone(worker));
        }
        let worker = Arc::new(transport::Worker::spawn().map_err(|_| Failure::Stopped)?);
        *slot = Some(Arc::clone(&worker));
        Ok(worker)
    }

    /// Send one request through the boundary. Nothing is resolved or
    /// connected unless the policy admits the operation with both grants.
    pub async fn send(
        &self,
        request: Outbound,
        process: GrantId,
        share: GrantId,
    ) -> Result<Streaming, Box<Failed>> {
        let mut record = request.record();
        let artifact = match (&request.artifact_target, &request.binding().artifact) {
            (None, None) => true,
            (Some(target), Some(binding)) => target.matches(binding),
            _ => false,
        };
        let page = match (&request.page_target, &request.binding().page) {
            (None, None) => true,
            (Some(target), Some(binding)) => target.matches(binding),
            _ => false,
        };
        if !artifact || !page {
            return Err(Box::new(Failed {
                failure: Failure::Denied(Denial::Scope),
                record,
            }));
        }
        if let Err(denial) = self.policy.admit(request.operation(), process, share) {
            return Err(Box::new(Failed {
                failure: Failure::from_denial(denial),
                record,
            }));
        }
        record.credential = request.destination().credential().is_some();
        let worker = match self.worker() {
            Ok(worker) => worker,
            Err(failure) => return Err(Box::new(Failed { failure, record })),
        };
        let shared = Arc::new(Mutex::new(record));
        let job = transport::Job {
            policy: Arc::clone(&self.policy),
            credentials: Arc::clone(&self.credentials),
            config: Arc::clone(&self.config),
            request,
            process,
            share,
            record: Arc::clone(&shared),
            #[cfg(test)]
            after_commit: Mutex::new(None),
        };
        worker.exchange(job, shared).await
    }

    #[cfg(test)]
    pub(crate) async fn send_with_hook(
        &self,
        request: Outbound,
        process: GrantId,
        share: GrantId,
        after_commit: Box<dyn FnOnce() + Send>,
    ) -> Result<Streaming, Box<Failed>> {
        let mut record = request.record();
        if let Err(denial) = self.policy.admit(request.operation(), process, share) {
            return Err(Box::new(Failed {
                failure: Failure::from_denial(denial),
                record,
            }));
        }
        record.credential = request.destination().credential().is_some();
        let worker = self.worker().expect("worker");
        let shared = Arc::new(Mutex::new(record));
        let job = transport::Job {
            policy: Arc::clone(&self.policy),
            credentials: Arc::clone(&self.credentials),
            config: Arc::clone(&self.config),
            request,
            process,
            share,
            record: Arc::clone(&shared),
            after_commit: Mutex::new(Some(after_commit)),
        };
        worker.exchange(job, shared).await
    }
}

/// A response whose head passed the policy. Body pieces are delivered only
/// while the operation and its grants stay live. Dropping it aborts the
/// exchange.
pub struct Streaming {
    authorization: ResponseAuthorization,
    ended: bool,
    pub status: u16,
    pub content_type: Option<String>,
    artifact_head: Option<http::Head>,
    body: futures_channel::mpsc::Receiver<Result<Vec<u8>, Failure>>,
    done: Option<futures_channel::oneshot::Receiver<()>>,
    record: Arc<Mutex<EgressRecord>>,
}

struct ResponseAuthorization {
    policy: Arc<Policy>,
    operation: Arc<Operation>,
    process: GrantId,
    share: GrantId,
}

impl ResponseAuthorization {
    fn check(&self) -> Result<(), Failure> {
        self.policy
            .accept_response(&self.operation, self.process, self.share)
            .map_err(Failure::from_denial)
    }
}

impl Streaming {
    /// The next body piece, `None` at the end of the body.
    pub async fn next(&mut self) -> Option<Result<Vec<u8>, Failure>> {
        use futures_util::StreamExt;
        if self.ended {
            return None;
        }
        let piece = self.body.next().await;
        // A worker can finish and queue the entire response before the adapter
        // polls. Check on the receiving side, including end-of-stream.
        if let Err(failure) = self.check_live() {
            self.body.close();
            self.ended = true;
            return Some(Err(failure));
        }
        self.ended = !matches!(piece, Some(Ok(_)));
        piece
    }

    pub(crate) fn check_live(&self) -> Result<(), Failure> {
        self.authorization.check()
    }

    pub(crate) async fn finish_checked(mut self) -> (EgressRecord, Result<(), Failure>) {
        self.body.close();
        if let Some(done) = self.done.take() {
            let _ = done.await;
        }
        (snapshot(&self.record), self.check_live())
    }

    /// Abort any remaining transfer and return the final transport record.
    pub async fn finish(self) -> EgressRecord {
        self.finish_checked().await.0
    }
}

fn snapshot(record: &Mutex<EgressRecord>) -> EgressRecord {
    match record.lock() {
        Ok(record) => record.clone(),
        Err(poisoned) => {
            let mut record = poisoned.into_inner().clone();
            record.transmission = Transmission::Unknown;
            record
        }
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod policy_tests;

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
