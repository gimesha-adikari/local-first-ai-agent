//! The trusted transport. It runs on its own thread with a private runtime,
//! opens a fresh connection for every request to an address it validated
//! itself, never consults proxy settings, never follows redirects, never
//! retries a request, and records whether request bytes left the process.

use std::{
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use agent_core::intelligence::{EgressRecord, Transmission};
use futures_channel::{mpsc, oneshot};
use futures_util::SinkExt;
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    sync::Semaphore,
};

use super::{
    BoundaryConfig, Failed, Failure, Host, Limits, Outbound, Resolver, ResponseAuthorization,
    Scheme, Streaming,
    http::{self, Decoder, Framing},
    snapshot,
};
use crate::{
    credentials::{CredentialError, CredentialSource},
    policy::{Denial, GrantId, Policy},
};

pub(super) struct Config {
    resolver: Arc<dyn Resolver>,
    tls: Arc<rustls::ClientConfig>,
    limits: Limits,
    slots: Arc<Semaphore>,
    artifact_slots: Arc<Semaphore>,
    connections: AtomicU64,
}

impl Config {
    pub fn new(config: BoundaryConfig) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_root_certificates(Arc::clone(&config.anchors.0))
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        // No session resumption or early data: every request starts from a
        // full, freshly verified handshake.
        tls.resumption = rustls::client::Resumption::disabled();
        tls.enable_early_data = false;
        Self {
            resolver: config.resolver,
            tls: Arc::new(tls),
            slots: Arc::new(Semaphore::new(config.limits.concurrency.max(1))),
            artifact_slots: Arc::new(Semaphore::new(1)),
            limits: config.limits,
            connections: AtomicU64::new(0),
        }
    }

    pub fn connections(&self) -> u64 {
        self.connections.load(Ordering::Acquire)
    }
}

pub(super) struct Job {
    pub policy: Arc<Policy>,
    pub credentials: Arc<dyn CredentialSource>,
    pub config: Arc<Config>,
    pub request: Outbound,
    pub process: GrantId,
    pub share: GrantId,
    pub record: Arc<Mutex<EgressRecord>>,
    #[cfg(test)]
    pub after_commit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

fn denied(denial: Denial) -> Failure {
    Failure::from_denial(denial)
}

impl Job {
    fn limits(&self) -> Limits {
        let mut limits = self.config.limits;
        if let Some(target) = &self.request.artifact_target {
            limits.max_body = target.max_body;
            limits.total = super::artifact::TRANSFER_LIMIT;
            limits.head = std::time::Duration::from_secs(30);
            limits.idle = limits.head;
            limits.chunk = 64 * 1024;
        }
        if self.request.page_target.is_some() {
            limits.max_body = limits.max_body.min(super::page::MAX_BODY);
            limits.total = limits.total.min(super::page::TOTAL);
            limits.head = limits.head.min(super::page::HEAD);
            limits.idle = limits.idle.min(super::page::HEAD);
        }
        limits
    }

    /// Before the commit: every admission condition, without consuming.
    fn admitted(&self) -> Result<(), Failure> {
        self.policy
            .admit(self.request.operation(), self.process, self.share)
            .map_err(denied)
    }

    /// After the commit: the operation and both grants are still live.
    fn live(&self) -> Result<(), Failure> {
        self.policy
            .accept_response(self.request.operation(), self.process, self.share)
            .map_err(denied)
    }

    fn set_transmission(&self, transmission: Transmission) {
        if let Ok(mut record) = self.record.lock() {
            record.transmission = transmission;
        }
    }
}

pub(super) struct Worker {
    handle: tokio::runtime::Handle,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Worker {
    pub fn spawn() -> io::Result<Self> {
        let (ready, started) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("egress-transport".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .max_blocking_threads(2)
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
                if ready.send(Ok((runtime.handle().clone(), stop))).is_err() {
                    return;
                }
                runtime.block_on(async {
                    let _ = stopped.await;
                });
            })?;
        let (handle, stop) = started
            .recv()
            .map_err(|_| io::Error::other("transport worker stopped"))??;
        Ok(Self {
            handle,
            stop: Some(stop),
        })
    }

    pub async fn exchange(
        &self,
        job: Job,
        record: Arc<Mutex<EgressRecord>>,
    ) -> Result<Streaming, Box<Failed>> {
        let (head_tx, head_rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(job.config.limits.queue.max(1));
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let authorization = ResponseAuthorization {
            policy: Arc::clone(&job.policy),
            operation: Arc::clone(&job.request.operation),
            process: job.process,
            share: job.share,
        };
        self.handle.spawn(async move {
            let limits = job.limits();
            let mut head_tx = Some(head_tx);
            let mut body_tx = body_tx;
            let outcome = tokio::time::timeout(limits.total, run(&job, &mut head_tx, &mut body_tx))
                .await
                .unwrap_or(Err(Failure::TimedOut));
            if let Err(failure) = outcome {
                match head_tx.take() {
                    Some(head) => {
                        let _ = head.send(Err(failure));
                    }
                    None => {
                        let _ = body_tx.send(Err(failure)).await;
                    }
                }
            }
            drop(body_tx);
            drop(job);
            drop(done_tx);
        });
        match head_rx.await {
            Ok(Ok((status, content_type, artifact_head))) => {
                if let Err(failure) = authorization.check() {
                    drop(body_rx);
                    let _ = done_rx.await;
                    return Err(Box::new(Failed {
                        failure,
                        record: snapshot(&record),
                    }));
                }
                Ok(Streaming {
                    authorization,
                    ended: false,
                    status,
                    content_type,
                    artifact_head,
                    body: body_rx,
                    done: Some(done_rx),
                    record,
                })
            }
            Ok(Err(failure)) => {
                let _ = done_rx.await;
                Err(Box::new(Failed {
                    failure,
                    record: snapshot(&record),
                }))
            }
            Err(_) => {
                let _ = done_rx.await;
                Err(Box::new(Failed {
                    failure: Failure::Stopped,
                    record: snapshot(&record),
                }))
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

type Head = (u16, Option<String>, Option<http::Head>);

/// Record socket progress immediately, including when the outer timeout drops a pending write.
struct Counted {
    inner: TcpStream,
    uploading: Arc<AtomicBool>,
    record: Arc<Mutex<EgressRecord>>,
}

impl AsyncRead for Counted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Counted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(count)) if count > 0)
            && self.uploading.load(Ordering::Acquire)
            && let Ok(mut record) = self.record.lock()
            && record.transmission == Transmission::Unsent
        {
            record.transmission = Transmission::Unknown;
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Wait for `operation`, rechecking `check` every tick.
async fn guarded<T>(
    limits: &Limits,
    deadline: Option<Instant>,
    check: impl Fn() -> Result<(), Failure>,
    operation: impl std::future::Future<Output = T>,
) -> Result<T, Failure> {
    let mut operation = std::pin::pin!(operation);
    let mut ticks = tokio::time::interval(limits.tick);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            value = &mut operation => return Ok(value),
            _ = ticks.tick() => {
                check()?;
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(Failure::TimedOut);
                }
            }
        }
    }
}

fn after(duration: std::time::Duration) -> Option<Instant> {
    Instant::now().checked_add(duration)
}

async fn resolve(job: &Job) -> Result<Vec<IpAddr>, Failure> {
    let limits = &job.limits();
    let endpoint = job.request.destination().endpoint();
    let scope = job.request.destination().scope();
    let addresses = match endpoint.host() {
        Host::Ip(ip) => vec![*ip],
        Host::Domain(name) => guarded(
            limits,
            after(limits.resolve),
            || job.admitted(),
            job.config.resolver.resolve(name, endpoint.port()),
        )
        .await?
        .map_err(|_| Failure::Resolve)?,
    };
    if addresses.is_empty() || addresses.len() > 32 {
        return Err(Failure::Resolve);
    }
    // One answer outside the scope refuses the request: a mixed answer is
    // how rebinding attacks smuggle a private address next to a public one.
    if !addresses.iter().all(|address| scope.permits(*address)) {
        return Err(Failure::Address);
    }
    let mut unique = Vec::with_capacity(addresses.len());
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    Ok(unique)
}

async fn connect(job: &Job, addresses: &[IpAddr]) -> Result<TcpStream, Failure> {
    let limits = &job.limits();
    let port = job.request.destination().endpoint().port();
    for address in addresses {
        job.admitted()?;
        job.config.connections.fetch_add(1, Ordering::AcqRel);
        let attempt = guarded(
            limits,
            after(limits.connect),
            || job.admitted(),
            TcpStream::connect(SocketAddr::new(*address, port)),
        )
        .await;
        match attempt {
            Ok(Ok(stream)) => {
                // The connected peer must be the validated address.
                let peer = stream.peer_addr().map_err(|_| Failure::Connect)?;
                if peer.ip() != *address || !job.request.destination().scope().permits(peer.ip()) {
                    return Err(Failure::Address);
                }
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(_)) | Err(Failure::TimedOut) => continue,
            Err(failure) => return Err(failure),
        }
    }
    Err(Failure::Connect)
}

async fn run(
    job: &Job,
    head_tx: &mut Option<oneshot::Sender<Result<Head, Failure>>>,
    body_tx: &mut mpsc::Sender<Result<Vec<u8>, Failure>>,
) -> Result<(), Failure> {
    let limits = job.limits();
    // Queue: wait for a slot while the operation stays admissible.
    let artifact = job.request.artifact_target.as_ref();
    let slots = Arc::clone(if artifact.is_some() {
        &job.config.artifact_slots
    } else {
        &job.config.slots
    });
    let _slot = guarded(&limits, None, || job.admitted(), slots.acquire_owned())
        .await?
        .map_err(|_| Failure::Stopped)?;
    job.admitted()?;

    let destination = job.request.destination();
    let endpoint = destination.endpoint();
    let credential = match destination.credential() {
        Some(binding) => {
            // The binding was made from this destination's own origin.
            if binding.key().origin != endpoint.origin() {
                return Err(Failure::Denied(Denial::Scope));
            }
            let read = guarded(
                &limits,
                after(limits.credential),
                || job.admitted(),
                job.credentials.read(binding.key()),
            )
            .await
            .map_err(|failure| {
                if failure == Failure::TimedOut {
                    Failure::Credential(CredentialError::TimedOut)
                } else {
                    failure
                }
            })?;
            Some(read.map_err(Failure::Credential)?)
        }
        None => None,
    };

    let addresses = resolve(job).await?;
    job.admitted()?;
    let tcp = connect(job, &addresses).await?;
    let uploading = Arc::new(AtomicBool::new(false));
    let counted = Counted {
        inner: tcp,
        uploading: Arc::clone(&uploading),
        record: Arc::clone(&job.record),
    };
    let mut stream: Box<dyn Io> = match endpoint.scheme() {
        Scheme::Http => Box::new(counted),
        Scheme::Https => {
            let name = match endpoint.host() {
                Host::Domain(name) => {
                    ServerName::try_from(name.clone()).map_err(|_| Failure::Tls)?
                }
                Host::Ip(ip) => ServerName::IpAddress((*ip).into()),
            };
            let connector = tokio_rustls::TlsConnector::from(Arc::clone(&job.config.tls));
            let tls = guarded(
                &limits,
                after(limits.handshake),
                || job.admitted(),
                connector.connect(name, counted),
            )
            .await?
            .map_err(|_| Failure::Tls)?;
            Box::new(tls)
        }
    };

    let binding = job.request.binding();
    let path = format!("{}{}", endpoint.base_path(), binding.route.as_str());
    let request = if let Some(target) = artifact {
        http::encode_artifact(&endpoint.authority(), target)
    } else if let Some(target) = &job.request.page_target {
        http::encode_page(&endpoint.authority(), &target.path)
    } else {
        http::encode_request(
            binding.method.as_str(),
            &endpoint.authority(),
            &path,
            job.request.content_type,
            &job.request.body,
            credential
                .as_ref()
                .zip(destination.credential().map(|binding| binding.header())),
        )
    };
    drop(credential);

    // Last check and consumption of the sharing grant, serialized with
    // revocation. No request byte is written before this.
    job.policy
        .commit(job.request.operation(), job.process, job.share)
        .map_err(denied)?;
    if let Ok(mut record) = job.record.lock() {
        record.authorization = vec![job.process, job.share];
    }
    #[cfg(test)]
    if let Some(hook) = job
        .after_commit
        .lock()
        .ok()
        .and_then(|mut hook| hook.take())
    {
        hook();
    }
    uploading.store(true, Ordering::Release);
    for slice in request.chunks(limits.chunk.max(1)) {
        guarded_write(job, stream.write_all(slice)).await?;
    }
    guarded_write(job, stream.flush()).await?;
    job.set_transmission(Transmission::Sent);
    drop(request);

    let mut buffer = Vec::with_capacity(8 * 1024);
    let head_deadline = after(limits.head);
    let head = loop {
        if let Some(head) = http::parse_head(&mut buffer, limits.max_head, artifact.is_some())? {
            break head;
        }
        if head_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(Failure::TimedOut);
        }
        if read_some(&mut stream, &mut buffer, &limits, job).await? == 0 {
            return Err(Failure::Protocol);
        }
    };
    job.live()?;
    if artifact.is_none() && (300..400).contains(&head.status) {
        return Err(Failure::Redirect {
            status: head.status,
            location: head.location,
        });
    }
    let framing = head.framing;
    let discard = artifact.is_some() && !matches!(head.status, 200 | 206);
    let mut decoder = Decoder::new(
        if discard { Framing::Empty } else { framing },
        limits.max_body,
    )?;
    let status = head.status;
    let content_type = head.content_type.clone();
    match head_tx.take() {
        Some(sender) => {
            if sender
                .send(Ok((status, content_type, artifact.map(|_| head))))
                .is_err()
            {
                return Ok(());
            }
        }
        None => return Err(Failure::Stopped),
    }
    if discard || framing == Framing::Empty {
        return Ok(());
    }
    let mut piece = Vec::with_capacity(limits.chunk);
    loop {
        decoder.decode(&mut buffer, &mut piece)?;
        while !piece.is_empty() {
            job.live()?;
            let rest = if piece.len() > limits.chunk {
                piece.split_off(limits.chunk)
            } else {
                Vec::new()
            };
            let sent = std::mem::replace(&mut piece, rest);
            if guarded(
                &limits,
                after(limits.idle),
                || job.live(),
                body_tx.send(Ok(sent)),
            )
            .await?
            .is_err()
            {
                // The adapter stopped reading: abort and discard.
                return Ok(());
            }
        }
        if decoder.is_done() {
            if artifact.is_some() && !buffer.is_empty() {
                return Err(Failure::Protocol);
            }
            if artifact.is_some() {
                // Connection: close disallows pipelining. Detect extra bytes
                // even when they arrive in a separate TLS record.
                let mut extra = [0u8; 1];
                match guarded(
                    &limits,
                    after(limits.idle),
                    || job.live(),
                    stream.read(&mut extra),
                )
                .await?
                {
                    Ok(0) => {}
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
                    Ok(_) => return Err(Failure::Protocol),
                    Err(_) => return Err(Failure::Io),
                }
            }
            return Ok(());
        }
        if read_some(&mut stream, &mut buffer, &limits, job).await? == 0 {
            if artifact.is_some() && matches!(framing, Framing::Length(_)) {
                return Err(Failure::Io);
            }
            decoder.finish_at_eof()?;
            return Ok(());
        }
    }
}

/// A ready socket may wake before the next liveness tick. Order each
/// non-blocking write/flush poll against the same lock used by restriction.
async fn guarded_write(
    job: &Job,
    write: impl std::future::Future<Output = io::Result<()>>,
) -> Result<(), Failure> {
    let limits = &job.limits();
    let mut write = std::pin::pin!(write);
    let poll = std::future::poll_fn(|cx| {
        match job
            .policy
            .with_live_grants(job.request.operation(), job.process, job.share, || {
                write.as_mut().poll(cx)
            }) {
            Ok(poll) => poll.map(|result| result.map_err(|_| Failure::Io)),
            Err(denial) => Poll::Ready(Err(denied(denial))),
        }
    });
    guarded(limits, after(limits.idle), || job.live(), poll).await?
}

async fn read_some(
    stream: &mut Box<dyn Io>,
    buffer: &mut Vec<u8>,
    limits: &Limits,
    job: &Job,
) -> Result<usize, Failure> {
    let mut scratch = vec![0; limits.chunk.max(1024)];
    let read = guarded(
        limits,
        after(limits.idle),
        || job.live(),
        stream.read(&mut scratch),
    )
    .await?
    .map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof && job.request.artifact_target.is_none() {
            Failure::Protocol
        } else {
            Failure::Io
        }
    })?;
    buffer.extend_from_slice(&scratch[..read]);
    Ok(read)
}
