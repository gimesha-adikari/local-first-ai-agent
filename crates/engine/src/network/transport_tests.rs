//! Socket-level tests of the trusted transport against loopback HTTP and TLS
//! fixtures. Synthetic payloads and credentials only.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, TcpListener},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use agent_core::{
    capability::{CapabilityRegistry, GENERATE, Locality, Retention},
    classification::{Classified, DataClasses, USER_INPUT},
    inference::Cancellation,
    intelligence::Transmission,
};
use futures_executor::block_on;
use zeroize::Zeroizing;

use super::*;
use crate::{
    credentials::{CredentialFuture, CredentialStatus, Secret},
    fixture::{Authority, Fixture, json, respond},
    policy::Permission,
};

#[derive(Default)]
struct Vault {
    values: Mutex<HashMap<CredentialKey, String>>,
    reads: Mutex<Vec<CredentialKey>>,
}

impl Vault {
    fn with(key: CredentialKey, value: &str) -> Arc<Self> {
        let vault = Self::default();
        vault.values.lock().unwrap().insert(key, value.to_owned());
        Arc::new(vault)
    }
}

impl CredentialSource for Vault {
    fn read(&self, key: &CredentialKey) -> CredentialFuture<Secret> {
        self.reads.lock().unwrap().push(key.clone());
        let value = self.values.lock().unwrap().get(key).cloned();
        Box::pin(async move {
            value
                .and_then(|value| Secret::new(Zeroizing::new(value)))
                .ok_or(CredentialError::Missing)
        })
    }

    fn status(&self, _key: &CredentialKey) -> CredentialFuture<CredentialStatus> {
        Box::pin(async { Ok(CredentialStatus::Missing) })
    }

    fn delete(&self, _key: &CredentialKey) -> CredentialFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

struct Fixed(HashMap<String, Vec<IpAddr>>);

impl Resolver for Fixed {
    fn resolve(&self, host: &str, _port: u16) -> ResolveFuture {
        let answer = self.0.get(host).cloned();
        Box::pin(async move { answer.ok_or_else(|| io::Error::other("no fixture answer")) })
    }
}

fn resolver(entries: &[(&str, &[&str])]) -> Arc<dyn Resolver> {
    Arc::new(Fixed(
        entries
            .iter()
            .map(|(host, addresses)| {
                (
                    (*host).to_owned(),
                    addresses.iter().map(|a| a.parse().unwrap()).collect(),
                )
            })
            .collect(),
    ))
}

fn fast_limits() -> Limits {
    Limits {
        head: Duration::from_secs(3),
        idle: Duration::from_secs(3),
        total: Duration::from_secs(10),
        ..Limits::default()
    }
}

struct Harness {
    policy: Arc<Policy>,
    boundary: NetworkBoundary,
    session: Session,
}

fn harness(
    destinations: Vec<Destination>,
    vault: Arc<Vault>,
    resolver: Arc<dyn Resolver>,
    anchors: TrustAnchors,
    limits: Limits,
) -> Harness {
    let policy = Arc::new(Policy::new(
        CapabilityRegistry::new([GENERATE]),
        DataClasses::default(),
        destinations,
        false,
    ));
    let boundary = NetworkBoundary::new(
        Arc::clone(&policy),
        vault,
        BoundaryConfig {
            resolver,
            anchors,
            limits,
        },
    );
    Harness {
        policy,
        boundary,
        session: Session::new(Cancellation::default()),
    }
}

fn self_hosted(id: &str, url: &str) -> Destination {
    Destination::new(id, Endpoint::parse(url).unwrap(), Locality::SelfHosted).unwrap()
}

fn loopback(port: u16) -> Destination {
    self_hosted("fixture.local/1", &format!("http://127.0.0.1:{port}/v1"))
}

impl Harness {
    fn request(&self, destination: Destination, body: &[u8]) -> Outbound {
        Outbound::new(
            &self.session,
            GENERATE,
            "answer",
            destination.locality(),
            destination,
            Method::Post,
            Route::new("/chat/completions"),
            Classified::from_source(body.to_vec(), USER_INPUT),
            Retention::None,
        )
    }

    fn grants(&self, request: &Outbound, until: Instant) -> (GrantId, GrantId) {
        (
            self.policy
                .grant(Permission::Process, request.operation(), until)
                .unwrap(),
            self.policy
                .grant(Permission::Share, request.operation(), until)
                .unwrap(),
        )
    }

    fn send(&self, request: Outbound) -> Result<Streaming, Box<Failed>> {
        let (process, share) = self.grants(&request, future());
        block_on(self.boundary.send(request, process, share))
    }
}

fn future() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn body(streaming: Streaming) -> (Vec<u8>, Option<Failure>, EgressRecord) {
    block_on(async move {
        let mut streaming = streaming;
        let mut bytes = Vec::new();
        let mut failure = None;
        while let Some(piece) = streaming.next().await {
            match piece {
                Ok(piece) => bytes.extend(piece),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        (bytes, failure, streaming.finish().await)
    })
}

fn none() -> Arc<Vault> {
    Arc::new(Vault::default())
}

#[test]
fn delivers_the_bound_request_with_its_credential_and_records_it() {
    let server = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"),
    );
    let destination = loopback(server.port).with_credential("fixture");
    let key = destination.credential().unwrap().key().clone();
    assert_eq!(key.origin, format!("http://127.0.0.1:{}", server.port));
    let vault = Vault::with(key.clone(), "sk-synthetic-123");
    let h = harness(
        vec![destination.clone()],
        Arc::clone(&vault),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(destination, b"{\"synthetic\":true}");
    let digest = hex(&sha256(b"{\"synthetic\":true}"));
    let streaming = h.send(request).unwrap();
    assert_eq!(streaming.status, 200);
    let (bytes, failure, record) = body(streaming);
    assert_eq!(bytes, b"hello");
    assert_eq!(failure, None);
    assert_eq!(record.transmission, Transmission::Sent);
    assert!(record.credential);
    assert_eq!(record.body_sha256, digest);
    assert_eq!(record.route, "/chat/completions");
    assert_eq!(record.method, "POST");
    assert_eq!(record.authorization.len(), 2);
    let requests = server.wait_requests(1);
    let text = String::from_utf8_lossy(&requests[0]).into_owned();
    assert!(text.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(text.contains(&format!("Host: 127.0.0.1:{}\r\n", server.port)));
    assert!(text.contains("Authorization: Bearer sk-synthetic-123\r\n"));
    assert!(text.ends_with("{\"synthetic\":true}"));
    assert_eq!(*vault.reads.lock().unwrap(), vec![key]);
    // Neither the record nor its debug form carries the credential.
    let record_text = format!("{record:?}");
    assert!(!record_text.contains("sk-synthetic"));
    assert_eq!(server.accepted(), 1);
}

#[test]
fn local_only_and_unregistered_destinations_open_no_connection() {
    let server = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(loopback(server.port), b"{}");
    let grants = h.grants(&request, future());
    h.policy.set_local_only(true);
    let failed = block_on(h.boundary.send(request, grants.0, grants.1))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::Locality));
    assert_eq!(failed.record.transmission, Transmission::Unsent);
    h.policy.set_local_only(false);
    let elsewhere = self_hosted("fixture.local/1", "http://127.0.0.1:9/v1");
    let failed = block_on(h.boundary.send(h.request(elsewhere, b"{}"), 0, 0))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::UnknownDestination));
    assert!(!h.boundary.started(), "no transport worker is created");
    assert_eq!(h.boundary.connections(), 0);
    assert_eq!(server.accepted(), 0);
}

#[test]
fn resolved_addresses_must_fit_the_destination_scope() {
    let server = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let port = server.port;
    let cloud = Destination::new(
        "fixture.cloud/1",
        Endpoint::parse("https://api.fixture.example/v1").unwrap(),
        Locality::ThirdParty,
    )
    .unwrap();
    let named_loopback = self_hosted("fixture.named/1", &format!("http://localhost:{port}/v1"));
    let pinned = self_hosted("fixture.lan/1", "http://192.168.50.7:8000/v1");
    for (answers, destination) in [
        // Rebinding a public name to loopback or a private address.
        (&["127.0.0.1"][..], cloud.clone()),
        (&["10.0.0.8"][..], cloud.clone()),
        (&["169.254.169.254"][..], cloud.clone()),
        // A mixed answer is refused as a whole.
        (&["93.184.216.34", "127.0.0.1"][..], cloud.clone()),
        (&["::ffff:127.0.0.1"][..], cloud.clone()),
        // A loopback name that resolves elsewhere.
        (&["127.0.0.1", "192.168.1.4"][..], named_loopback.clone()),
    ] {
        let h = harness(
            vec![destination.clone()],
            none(),
            resolver(&[("api.fixture.example", answers), ("localhost", answers)]),
            TrustAnchors::from_der([]),
            fast_limits(),
        );
        let failed = h.send(h.request(destination.clone(), b"{}")).err().unwrap();
        assert_eq!(failed.failure, Failure::Address, "{answers:?}");
        assert_eq!(failed.record.transmission, Transmission::Unsent);
        assert_eq!(h.boundary.connections(), 0);
    }
    // A pinned local-network destination is an IP literal; no resolution
    // happens and only the pinned address is admitted by its scope.
    assert!(pinned.scope().permits("192.168.50.7".parse().unwrap()));
    assert!(!pinned.scope().permits("192.168.50.8".parse().unwrap()));
    // A loopback name resolving to loopback connects.
    let h = harness(
        vec![named_loopback.clone()],
        none(),
        resolver(&[("localhost", &["127.0.0.1"])]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let (_, failure, record) = body(h.send(h.request(named_loopback, b"{}")).unwrap());
    assert_eq!(failure, None);
    assert_eq!(record.transmission, Transmission::Sent);
    assert_eq!(server.accepted(), 1);
}

#[test]
fn redirects_are_returned_as_data_and_never_followed() {
    let target = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let location = format!("http://127.0.0.1:{}/v1/chat/completions", target.port);
    let redirect: &'static str = Box::leak(
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
        )
        .into_boxed_str(),
    );
    let server = Fixture::start(None, respond(redirect));
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let failed = h
        .send(h.request(loopback(server.port), b"{}"))
        .err()
        .unwrap();
    assert_eq!(
        failed.failure,
        Failure::Redirect {
            status: 307,
            location: Some(location)
        }
    );
    assert_eq!(failed.record.transmission, Transmission::Sent);
    assert_eq!(target.accepted(), 0);
    assert_eq!(server.accepted(), 1);
}

#[test]
fn failures_are_not_retried_and_proxy_settings_are_ignored() {
    let proxy = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let proxy_url = format!("http://127.0.0.1:{}", proxy.port);
    // SAFETY: test-only; other tests never read these variables.
    unsafe {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
        ] {
            std::env::set_var(name, &proxy_url);
        }
    }
    let server = Fixture::start(None, Arc::new(|_, _| {}));
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let failed = h
        .send(h.request(loopback(server.port), b"{}"))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Protocol);
    assert_eq!(failed.record.transmission, Transmission::Sent);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(server.accepted(), 1, "exactly one attempt");
    assert_eq!(proxy.accepted(), 0, "environment proxies are never used");
}

#[test]
fn tls_requires_a_trusted_certificate_for_the_exact_host() {
    let authority = Authority::new();
    let other = Authority::new();
    let resolve = || resolver(&[("localhost", &["127.0.0.1"])]);
    let destination =
        |port: u16| self_hosted("fixture.tls/1", &format!("https://localhost:{port}/v1"));
    let good = Fixture::start(
        Some(authority.server(&["localhost"], false)),
        respond("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    );
    let h = harness(
        vec![destination(good.port)],
        none(),
        resolve(),
        TrustAnchors::from_der([authority.root_der()]),
        fast_limits(),
    );
    let (bytes, failure, record) = body(h.send(h.request(destination(good.port), b"{}")).unwrap());
    assert_eq!((bytes.as_slice(), failure), (&b"ok"[..], None));
    assert_eq!(record.transmission, Transmission::Sent);
    assert_eq!(good.wait_requests(1).len(), 1);

    for (server, anchors) in [
        // Untrusted issuer.
        (
            Fixture::start(Some(other.server(&["localhost"], false)), respond("")),
            authority.root_der(),
        ),
        // Wrong host name.
        (
            Fixture::start(
                Some(authority.server(&["api.other.example"], false)),
                respond(""),
            ),
            authority.root_der(),
        ),
        // Expired certificate.
        (
            Fixture::start(Some(authority.server(&["localhost"], true)), respond("")),
            authority.root_der(),
        ),
    ] {
        let h = harness(
            vec![destination(server.port)],
            none(),
            resolve(),
            TrustAnchors::from_der([anchors]),
            fast_limits(),
        );
        let failed = h
            .send(h.request(destination(server.port), b"{}"))
            .err()
            .unwrap();
        assert_eq!(failed.failure, Failure::Tls);
        assert_eq!(failed.record.transmission, Transmission::Unsent);
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            server.requests().is_empty(),
            "no request reached the server"
        );
    }
}

#[test]
fn expiry_cancellation_and_missing_credentials_stop_before_connecting() {
    let server = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(loopback(server.port), b"{}");
    let (process, share) = h.grants(&request, Instant::now() + Duration::from_millis(30));
    std::thread::sleep(Duration::from_millis(40));
    let failed = block_on(h.boundary.send(request, process, share))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::Expired));

    // Replacing the registration with a credential-bound variant.
    h.policy
        .register_destination(loopback(server.port).with_credential("fixture"))
        .unwrap();
    let failed = h
        .send(h.request(loopback(server.port).with_credential("fixture"), b"{}"))
        .err()
        .unwrap();
    assert_eq!(
        failed.failure,
        Failure::Credential(CredentialError::Missing)
    );
    assert_eq!(failed.record.transmission, Transmission::Unsent);

    h.policy
        .register_destination(loopback(server.port))
        .unwrap();
    let request = h.request(loopback(server.port), b"{}");
    let grants = h.grants(&request, future());
    h.session.cancellation().cancel();
    let failed = block_on(h.boundary.send(request, grants.0, grants.1))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Cancelled);
    assert_eq!(server.accepted(), 0);
}

#[test]
fn revocation_while_queued_drops_the_request_without_a_connection() {
    let server = Fixture::start(
        None,
        Arc::new(|_, out: &mut dyn Write| {
            std::thread::sleep(Duration::from_millis(400));
            let _ = out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }),
    );
    let h = Arc::new(harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        Limits {
            concurrency: 1,
            ..fast_limits()
        },
    ));
    let first = h.request(loopback(server.port), b"{\"n\":1}");
    let first_grants = h.grants(&first, future());
    let runner = Arc::clone(&h);
    let slow = std::thread::spawn(move || {
        let streaming =
            block_on(runner.boundary.send(first, first_grants.0, first_grants.1)).unwrap();
        body(streaming).2
    });
    std::thread::sleep(Duration::from_millis(100));
    let queued = h.request(loopback(server.port), b"{\"n\":2}");
    let (process, share) = h.grants(&queued, future());
    let revoker = Arc::clone(&h.policy);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        revoker.revoke(share).unwrap();
    });
    let failed = block_on(h.boundary.send(queued, process, share))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::Revoked));
    assert_eq!(failed.record.transmission, Transmission::Unsent);
    assert_eq!(slow.join().unwrap().transmission, Transmission::Sent);
    assert_eq!(server.accepted(), 1);
}

#[test]
fn revocation_ordered_after_commit_but_before_the_first_write_sends_nothing() {
    let server = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(loopback(server.port), b"{}");
    let (process, share) = h.grants(&request, future());
    let policy = Arc::clone(&h.policy);
    let failed = block_on(h.boundary.send_with_hook(
        request,
        process,
        share,
        Box::new(move || {
            policy.revoke(process).unwrap();
        }),
    ))
    .err()
    .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::Revoked));
    assert_eq!(failed.record.transmission, Transmission::Unsent);
    assert_eq!(failed.record.authorization, vec![process, share]);
    let requests = server.wait_requests(1);
    assert!(
        requests.iter().all(Vec::is_empty),
        "the connection carried no request bytes"
    );
}

#[test]
fn cancellation_expiry_and_revocation_interrupt_a_stalled_request_body() {
    for expected in [
        Failure::Cancelled,
        Failure::Denied(Denial::Expired),
        Failure::Denied(Denial::Revoked),
        Failure::Denied(Denial::Locality),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let destination = loopback(listener.local_addr().unwrap().port());
        let h = harness(
            vec![destination.clone()],
            none(),
            resolver(&[]),
            TrustAnchors::from_der([]),
            fast_limits(),
        );
        let payload = vec![b'x'; MAX_REQUEST_BODY];
        let request = h.request(destination, &payload);
        let share_until = if expected == Failure::Denied(Denial::Expired) {
            Instant::now() + Duration::from_secs(1)
        } else {
            future()
        };
        let process = h
            .policy
            .grant(Permission::Process, request.operation(), future())
            .unwrap();
        let share = h
            .policy
            .grant(Permission::Share, request.operation(), share_until)
            .unwrap();
        let cancel = h.session.cancellation().clone();
        let policy = Arc::clone(&h.policy);
        let (finished, wait) = mpsc::channel();
        let peer = std::thread::spawn({
            let expected = expected.clone();
            move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut first = [0];
                reader.read_exact(&mut first).unwrap();
                assert_eq!(first, [b'x']);
                assert!(Instant::now() < share_until);
                // Leave the rest of the upload unread until the sender stops.
                // Let the socket fill before changing liveness.
                std::thread::sleep(Duration::from_millis(50));
                match expected {
                    Failure::Cancelled => cancel.cancel(),
                    Failure::Denied(Denial::Revoked) => assert!(policy.revoke(share).unwrap()),
                    Failure::Denied(Denial::Locality) => policy.set_local_only(true),
                    _ => {}
                }
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                let mut remaining = Vec::new();
                reader.read_to_end(&mut remaining).unwrap();
                remaining.len() + first.len()
            }
        });

        let failed = block_on(h.boundary.send(request, process, share))
            .err()
            .unwrap();
        finished.send(()).unwrap();
        let received = peer.join().unwrap();
        assert_eq!(failed.failure, expected);
        assert_eq!(failed.record.transmission, Transmission::Unknown);
        assert!(received < payload.len(), "the peer received the full body");
    }
}

#[test]
fn revocation_or_expiry_after_sending_discards_the_late_response() {
    let slow = || {
        Fixture::start(
            None,
            Arc::new(|_, out: &mut dyn Write| {
                std::thread::sleep(Duration::from_millis(300));
                let _ = out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nlate");
            }),
        )
    };
    let server = slow();
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(loopback(server.port), b"{}");
    let (process, share) = h.grants(&request, future());
    let revoker = Arc::clone(&h.policy);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        revoker.revoke(share).unwrap();
    });
    let failed = block_on(h.boundary.send(request, process, share))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::Revoked));
    assert_eq!(failed.record.transmission, Transmission::Sent);

    let server = slow();
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(loopback(server.port), b"{}");
    let (process, share) = h.grants(&request, Instant::now() + Duration::from_millis(150));
    let failed = block_on(h.boundary.send(request, process, share))
        .err()
        .unwrap();
    assert_eq!(failed.failure, Failure::Denied(Denial::Expired));
    assert_eq!(failed.record.transmission, Transmission::Sent);
}

#[test]
fn cancellation_during_streaming_stops_delivery() {
    let server = Fixture::start(
        None,
        Arc::new(|_, out: &mut dyn Write| {
            let _ = out.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
            for _ in 0..50 {
                if out.write_all(b"6\r\nchunk.\r\n").is_err() || out.flush().is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            let _ = out.write_all(b"0\r\n\r\n");
        }),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let mut streaming = h.send(h.request(loopback(server.port), b"{}")).unwrap();
    let first = block_on(streaming.next()).unwrap().unwrap();
    assert_eq!(first, b"chunk.");
    h.session.cancellation().cancel();
    let (_, failure, record) = body(streaming);
    assert_eq!(failure, Some(Failure::Cancelled));
    assert_eq!(record.transmission, Transmission::Sent);
}

#[test]
fn response_and_header_sizes_and_idle_time_are_bounded() {
    let endless = Fixture::start(
        None,
        Arc::new(|_, out: &mut dyn Write| {
            let _ = out.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
            let piece = format!("400\r\n{}\r\n", "x".repeat(1024));
            for _ in 0..200 {
                if out.write_all(piece.as_bytes()).is_err() {
                    return;
                }
            }
        }),
    );
    let oversized_length = Fixture::start(
        None,
        respond("HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n"),
    );
    let huge_head: &'static str = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nX-Fill: {}\r\n\r\n",
            "a".repeat(80 * 1024)
        )
        .into_boxed_str(),
    );
    let large_head = Fixture::start(None, respond(huge_head));
    let silent = Fixture::start(
        None,
        Arc::new(|_, _: &mut dyn Write| std::thread::sleep(Duration::from_millis(1500))),
    );
    let limits = Limits {
        max_body: 64 * 1024,
        idle: Duration::from_millis(300),
        head: Duration::from_millis(300),
        ..fast_limits()
    };
    for (server, expected) in [
        (&endless, Failure::TooLarge),
        (&oversized_length, Failure::TooLarge),
        (&large_head, Failure::TooLarge),
        (&silent, Failure::TimedOut),
    ] {
        let h = harness(
            vec![loopback(server.port)],
            none(),
            resolver(&[]),
            TrustAnchors::from_der([]),
            limits,
        );
        let failure = match h.send(h.request(loopback(server.port), b"{}")) {
            Err(failed) => failed.failure,
            Ok(streaming) => body(streaming).1.unwrap(),
        };
        assert_eq!(failure, expected);
    }
}

#[test]
fn json_fixture_helper_frames_bodies() {
    let server = Fixture::start(
        None,
        Arc::new(|_, out: &mut dyn Write| {
            let _ = out.write_all(json(201, "{\"ok\":true}").as_bytes());
        }),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let streaming = h.send(h.request(loopback(server.port), b"{}")).unwrap();
    assert_eq!(streaming.status, 201);
    assert_eq!(streaming.content_type.as_deref(), Some("application/json"));
    assert_eq!(body(streaming).0, b"{\"ok\":true}");
}

#[test]
fn restriction_revokes_in_flight_and_queued_requests_across_reenable() {
    let (entered, arrived) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let resume = Mutex::new(resume);
    let server = Fixture::start(
        None,
        Arc::new(move |_, out| {
            entered.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            let _ = out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nlate");
        }),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        Limits {
            concurrency: 1,
            ..fast_limits()
        },
    );
    let first = h.request(loopback(server.port), b"first");
    let a = h.grants(&first, future());
    let second = h.request(loopback(server.port), b"second");
    let b = h.grants(&second, future());
    block_on(async {
        let mut first = Box::pin(h.boundary.send(first, a.0, a.1));
        assert!(
            std::future::Future::poll(
                first.as_mut(),
                &mut std::task::Context::from_waker(std::task::Waker::noop())
            )
            .is_pending()
        );
        arrived.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut second = Box::pin(h.boundary.send(second, b.0, b.1));
        assert!(
            std::future::Future::poll(
                second.as_mut(),
                &mut std::task::Context::from_waker(std::task::Waker::noop())
            )
            .is_pending()
        );
        h.policy.set_local_only(true);
        h.policy.set_local_only(false);
        release.send(()).unwrap();
        let first = first.await.err().unwrap();
        let second = second.await.err().unwrap();
        assert!(matches!(
            first.failure,
            Failure::Denied(Denial::Revoked | Denial::Locality)
        ));
        assert!(matches!(
            second.failure,
            Failure::Denied(Denial::Revoked | Denial::Locality)
        ));
        assert_eq!(first.record.transmission, Transmission::Sent);
        assert_eq!(second.record.transmission, Transmission::Unsent);
        assert_eq!(server.accepted(), 1);
    });
}

#[test]
fn restriction_discards_a_buffered_response_after_reenable() {
    let server = Fixture::start(
        None,
        Arc::new(|_, out| {
            let _ = out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nresponse");
        }),
    );
    let h = harness(
        vec![loopback(server.port)],
        none(),
        resolver(&[]),
        TrustAnchors::from_der([]),
        fast_limits(),
    );
    let request = h.request(loopback(server.port), b"prompt");
    let mut response = h.send(request).unwrap();
    h.policy.set_local_only(true);
    h.policy.set_local_only(false);
    assert_eq!(
        block_on(response.next()).unwrap().unwrap_err(),
        Failure::Denied(Denial::Revoked)
    );
    assert_eq!(block_on(response.finish()).transmission, Transmission::Sent);
}
