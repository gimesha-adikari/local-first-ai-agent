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
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use agent_core::intelligence::{EgressRecord, Transmission};
use futures_channel::{mpsc, oneshot};
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    sync::Semaphore,
};

use super::{
    BoundaryConfig, Failed, Failure, Host, Limits, Outbound, Resolver, Scheme, Streaming,
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
        self.handle.spawn(async move {
            let limits = job.config.limits;
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
                        let _ = body_tx.try_send(Err(failure));
                    }
                }
            }
            drop(body_tx);
            drop(job);
            drop(done_tx);
        });
        match head_rx.await {
            Ok(Ok((status, content_type))) => Ok(Streaming {
                status,
                content_type,
                body: body_rx,
                done: Some(done_rx),
                record,
            }),
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

type Head = (u16, Option<String>);

/// Counts bytes accepted by the socket for writing.
struct Counted {
    inner: TcpStream,
    written: Arc<AtomicU64>,
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
        if let Poll::Ready(Ok(count)) = result {
            self.written.fetch_add(count as u64, Ordering::AcqRel);
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
    let limits = &job.config.limits;
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
    let limits = &job.config.limits;
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
    let limits = job.config.limits;
    // Queue: wait for a slot while the operation stays admissible.
    let slots = Arc::clone(&job.config.slots);
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
    let written = Arc::new(AtomicU64::new(0));
    let counted = Counted {
        inner: tcp,
        written: Arc::clone(&written),
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
    let request = http::encode_request(
        binding.method.as_str(),
        &endpoint.authority(),
        &path,
        job.request.content_type,
        &job.request.body,
        credential.as_ref(),
    );
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
    let baseline = written.load(Ordering::Acquire);
    let transmitted = |written: &AtomicU64| {
        if written.load(Ordering::Acquire) > baseline {
            Transmission::Unknown
        } else {
            Transmission::Unsent
        }
    };
    for slice in request.chunks(limits.chunk.max(1)) {
        if let Err(failure) = job.live() {
            job.set_transmission(transmitted(&written));
            return Err(failure);
        }
        let write = guarded(
            &limits,
            after(limits.idle),
            || job.live(),
            stream.write_all(slice),
        )
        .await;
        match write {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                job.set_transmission(transmitted(&written));
                return Err(Failure::Io);
            }
            Err(failure) => {
                job.set_transmission(transmitted(&written));
                return Err(failure);
            }
        }
    }
    match guarded(&limits, after(limits.idle), || job.live(), stream.flush()).await {
        Ok(Ok(())) => job.set_transmission(Transmission::Sent),
        Ok(Err(_)) => {
            job.set_transmission(transmitted(&written));
            return Err(Failure::Io);
        }
        Err(failure) => {
            job.set_transmission(transmitted(&written));
            return Err(failure);
        }
    }
    drop(request);

    let mut buffer = Vec::with_capacity(8 * 1024);
    let head_deadline = after(limits.head);
    let head = loop {
        if let Some(head) = http::parse_head(&mut buffer, limits.max_head)? {
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
    if (300..400).contains(&head.status) {
        return Err(Failure::Redirect {
            status: head.status,
            location: head.location,
        });
    }
    let mut decoder = Decoder::new(head.framing, limits.max_body)?;
    match head_tx.take() {
        Some(sender) => {
            if sender.send(Ok((head.status, head.content_type))).is_err() {
                return Ok(());
            }
        }
        None => return Err(Failure::Stopped),
    }
    if head.framing == Framing::Empty {
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
            use futures_util::SinkExt;
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
            return Ok(());
        }
        if read_some(&mut stream, &mut buffer, &limits, job).await? == 0 {
            decoder.finish_at_eof()?;
            return Ok(());
        }
    }
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
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Failure::Protocol
        } else {
            Failure::Io
        }
    })?;
    buffer.extend_from_slice(&scratch[..read]);
    Ok(read)
}
