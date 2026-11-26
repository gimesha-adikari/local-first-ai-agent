//! Controlled loopback HTTP and TLS servers for transport tests. Synthetic
//! data only; nothing leaves the machine.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

/// What the fixture writes for one request.
pub(crate) type Handler = Arc<dyn Fn(&[u8], &mut dyn Write) + Send + Sync>;

pub(crate) struct Fixture {
    pub port: u16,
    accepted: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) fn respond(raw: &'static str) -> Handler {
    Arc::new(move |_, out| {
        let _ = out.write_all(raw.as_bytes());
    })
}

pub(crate) fn json(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn read_request(stream: &mut dyn Read) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut expected: Option<usize> = None;
    loop {
        if let Some(total) = expected
            && request.len() >= total
        {
            break;
        }
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        request.extend_from_slice(&buffer[..read]);
        if request.len() > 16 << 20 {
            break;
        }
        if expected.is_none()
            && let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            expected = Some(end + 4 + length);
        }
    }
    request
}

impl Fixture {
    pub fn start(tls: Option<Arc<rustls::ServerConfig>>, handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let accepted = Arc::clone(&accepted);
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            accepted.fetch_add(1, Ordering::AcqRel);
                            let handler = Arc::clone(&handler);
                            let requests = Arc::clone(&requests);
                            let tls = tls.clone();
                            std::thread::spawn(move || serve(stream, tls, handler, requests));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
        };
        Self {
            port,
            accepted,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::Acquire)
    }

    pub fn requests(&self) -> Vec<Vec<u8>> {
        self.requests.lock().unwrap().clone()
    }

    /// Wait until `count` requests were recorded or a second passes.
    pub fn wait_requests(&self, count: usize) -> Vec<Vec<u8>> {
        for _ in 0..200 {
            let requests = self.requests();
            if requests.len() >= count {
                return requests;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.requests()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    tls: Option<Arc<rustls::ServerConfig>>,
    handler: Handler,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    match tls {
        None => {
            let mut stream = stream;
            let request = read_request(&mut stream);
            requests.lock().unwrap().push(request.clone());
            handler(&request, &mut stream);
            let _ = stream.flush();
        }
        Some(config) => {
            let Ok(connection) = rustls::ServerConnection::new(config) else {
                return;
            };
            let mut tls = rustls::StreamOwned::new(connection, stream);
            let request = read_request(&mut tls);
            if !tls.conn.is_handshaking() {
                requests.lock().unwrap().push(request.clone());
                handler(&request, &mut tls);
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        }
    }
}

/// A fixture certificate authority and server certificates.
pub(crate) struct Authority {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl Authority {
    pub fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        Self {
            issuer: CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap(),
        }
    }

    pub fn root_der(&self) -> Vec<u8> {
        self.issuer.der().to_vec()
    }

    /// A server configuration presenting a certificate for `names`.
    pub fn server(&self, names: &[&str], expired: bool) -> Arc<rustls::ServerConfig> {
        let mut params = CertificateParams::new(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        if expired {
            params.not_before = rcgen::date_time_ymd(2020, 1, 1);
            params.not_after = rcgen::date_time_ymd(2021, 1, 1);
        }
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.issuer).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.der().clone()],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
                )
                .unwrap(),
        )
    }
}
