//! The only constructor for public artifact requests. A download gesture
//! authorizes one pinned artifact and its verified distribution chain.

use std::time::{Duration, Instant};

use agent_core::{
    capability::{DOWNLOAD_MODEL, Locality, Retention},
    classification::{Labels, MODEL_ARTIFACT},
    inference::Cancellation,
};
use url::Url;

use super::{
    Destination, Endpoint, Failure, Method, NetworkBoundary, Outbound, RequestBinding, Route,
    Streaming, hex, http::Framing, sha256,
};
use crate::{
    manifest::Artifact,
    policy::{Connectivity, Operation, Permission, Session},
};

const HUB: &str = "huggingface.co";
const CDNS: &[&str] = &["us.aws.cdn.hf.co", "cas-bridge.xethub.hf.co"];
const MAX_REDIRECTS: usize = 3;
pub(super) const TRANSFER_LIMIT: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Binding {
    pub key: &'static str,
    target_sha256: [u8; 32],
}

pub(super) struct Target {
    pub path: String,
    pub offset: Option<u64>,
    pub if_range: Option<String>,
    pub max_body: usize,
}

impl Target {
    pub(super) fn matches(&self, binding: &Binding) -> bool {
        self.sha256() == binding.target_sha256
    }

    fn sha256(&self) -> [u8; 32] {
        sha256(
            format!(
                "{}\n{:?}\n{:?}\n{}",
                self.path, self.offset, self.if_range, self.max_body
            )
            .as_bytes(),
        )
    }
}

pub(crate) struct DownloadConsent {
    artifact: &'static Artifact,
    session: Session,
    expires: Instant,
}

impl DownloadConsent {
    pub fn new(artifact: &'static Artifact, cancel: Cancellation) -> Self {
        Self {
            artifact,
            session: Session::new(cancel),
            expires: Instant::now() + TRANSFER_LIMIT,
        }
    }

    pub fn check(&self) -> Result<(), Failure> {
        if self.session.is_closed() {
            return Err(Failure::Cancelled);
        }
        if Instant::now() >= self.expires {
            return Err(Failure::TimedOut);
        }
        Ok(())
    }
}

impl Drop for DownloadConsent {
    fn drop(&mut self) {
        self.session.close();
    }
}

pub(crate) struct Representation {
    pub fingerprint: String,
    pub etag: String,
}

pub(crate) struct ArtifactResponse {
    pub stream: Streaming,
    pub offset: u64,
    pub representation: Option<Representation>,
}

fn destination(host: &str) -> Destination {
    Destination::new(
        &format!("artifact/{host}"),
        Endpoint::parse(&format!("https://{host}")).expect("fixed HTTPS artifact origin"),
        Locality::ThirdParty,
    )
    .expect("fixed artifact destination")
}

pub(crate) fn destinations() -> impl Iterator<Item = Destination> {
    std::iter::once(HUB)
        .chain(CDNS.iter().copied())
        .map(destination)
}

fn hosts() -> impl Iterator<Item = &'static str> {
    std::iter::once(HUB).chain(CDNS.iter().copied())
}

fn origin_host(url: &Url) -> Result<&'static str, Failure> {
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.as_str().len() > 8192
    {
        return Err(Failure::Address);
    }
    hosts()
        .find(|host| Some(*host) == url.host_str())
        .ok_or(Failure::Address)
}

fn redirect(url: &Url, head: &super::http::Head, artifact: &Artifact) -> Result<Url, Failure> {
    if !matches!(head.status, 301 | 302 | 303 | 307 | 308) {
        return Err(Failure::Protocol);
    }
    let next = Url::parse(head.location.as_deref().ok_or(Failure::Protocol)?)
        .map_err(|_| Failure::Address)?;
    let host = origin_host(&next)?;
    let path = next.path();
    let mut segments = path.split('/');
    let storage_path = segments.next() == Some("")
        && segments.next() == Some("xet-bridge-us")
        && segments
            .next()
            .is_some_and(|id| id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        && segments
            .next()
            .is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        && segments.next().is_none();
    if !CDNS.contains(&host) || !storage_path || next.query().is_none() {
        return Err(Failure::Address);
    }
    if url.host_str() == Some(HUB) {
        if head.linked_etag.as_deref() != Some(&format!("\"{}\"", artifact.sha256))
            || head.linked_size.as_deref() != Some(&artifact.bytes.to_string())
        {
            return Err(Failure::Protocol);
        }
    } else if next.path() != url.path() {
        return Err(Failure::Address);
    }
    Ok(next)
}

fn fingerprint(url: &Url) -> String {
    hex(&sha256(
        format!("{}{}", url.origin().ascii_serialization(), url.path()).as_bytes(),
    ))
}

fn strong_etag(value: &str) -> bool {
    // The supported distribution path uses quoted hexadecimal identifiers.
    // Retain only that public identifier; never save arbitrary header values.
    value.len() == 66
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..65].iter().all(u8::is_ascii_hexdigit)
}

fn response_offset(
    head: &super::http::Head,
    bytes: u64,
    resume: Option<(u64, &Representation)>,
    representation: Option<&Representation>,
) -> Result<u64, Failure> {
    match (head.status, resume) {
        (200, _) if head.framing == Framing::Length(bytes) && head.content_range.is_none() => Ok(0),
        (206, Some((offset, expected))) if offset > 0 && offset < bytes => {
            let range = format!("bytes {offset}-{}/{bytes}", bytes - 1);
            if head.framing == Framing::Length(bytes - offset)
                && head.content_range.as_deref() == Some(&range)
                && representation.is_some_and(|actual| {
                    actual.fingerprint == expected.fingerprint && actual.etag == expected.etag
                })
            {
                Ok(offset)
            } else {
                Err(Failure::Protocol)
            }
        }
        (200 | 206 | 416, _) => Err(Failure::Protocol),
        _ => Err(Failure::Io),
    }
}

impl NetworkBoundary {
    pub(crate) async fn open_artifact(
        &self,
        consent: &DownloadConsent,
        resume: Option<(u64, &Representation)>,
        mut record_host: impl FnMut(&'static str) -> Result<(), Failure>,
    ) -> Result<ArtifactResponse, Failure> {
        let artifact = consent.artifact;
        let mut url = Url::parse(&format!(
            "https://{}/resolve/{}/{}?download=true",
            artifact.source, artifact.revision, artifact.file_name,
        ))
        .map_err(|_| Failure::Address)?;
        if origin_host(&url)? != HUB {
            return Err(Failure::Address);
        }
        for hop in 0..=MAX_REDIRECTS {
            consent.check()?;
            let host = origin_host(&url)?;
            #[cfg(not(test))]
            let destination = destination(host);
            #[cfg(test)]
            let destination = self
                .policy
                .registered_destination(&format!("artifact/{host}"))
                .ok_or(Failure::Address)?;
            let path = url[url::Position::BeforePath..].to_owned();
            let offset = resume.map(|(offset, _)| offset);
            let identity = fingerprint(&url);
            let if_range = resume
                .filter(|(_, representation)| {
                    representation.fingerprint == identity && strong_etag(&representation.etag)
                })
                .map(|(_, representation)| representation.etag.clone());
            let target = Target {
                path,
                offset,
                if_range,
                max_body: usize::try_from(artifact.bytes).map_err(|_| Failure::TooLarge)?,
            };
            let binding = RequestBinding {
                method: Method::Get,
                route: Route::new("/"),
                body_sha256: sha256(&[]),
                body_len: 0,
                artifact: Some(Binding {
                    key: artifact.key,
                    target_sha256: target.sha256(),
                }),
                page: None,
            };
            let outbound = Outbound {
                operation: Operation::new(
                    &consent.session,
                    DOWNLOAD_MODEL,
                    "model-download",
                    Labels::from_source(MODEL_ARTIFACT),
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
                artifact_target: Some(target),
                page_target: None,
                #[cfg(test)]
                query: None,
            };
            let process = self
                .policy
                .grant(Permission::Process, outbound.operation(), consent.expires)
                .map_err(Failure::from_denial)?;
            let share = self
                .policy
                .grant(Permission::Share, outbound.operation(), consent.expires)
                .map_err(Failure::from_denial)?;
            record_host(host)?;
            let mut stream = self
                .send(outbound, process, share)
                .await
                .map_err(|failed| failed.failure)?;
            let head = stream.artifact_head.take().ok_or(Failure::Protocol)?;
            if (300..400).contains(&head.status) {
                if hop == MAX_REDIRECTS {
                    return Err(Failure::Address);
                }
                url = redirect(&url, &head, artifact)?;
                stream.finish_checked().await.1?;
                continue;
            }
            let representation =
                head.etag
                    .as_deref()
                    .filter(|value| strong_etag(value))
                    .map(|etag| Representation {
                        fingerprint: identity,
                        etag: etag.to_owned(),
                    });
            let offset = response_offset(&head, artifact.bytes, resume, representation.as_ref())?;
            return Ok(ArtifactResponse {
                stream,
                offset,
                representation,
            });
        }
        Err(Failure::Address)
    }
}

#[cfg(test)]
pub(crate) fn route_fixture(policy: &crate::policy::Policy, port: u16) {
    for host in hosts() {
        let mut dest = destination(host);
        dest.endpoint = Endpoint::parse(&format!("https://{host}:{port}")).unwrap();
        dest.scope = super::NetworkScope::Loopback;
        policy.register_destination(dest).unwrap();
    }
}
