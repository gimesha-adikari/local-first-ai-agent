//! Stage 04-C2 feasibility probe: a managed, one-shot SearXNG worker whose
//! upstream requests are brokered by the trusted boundary. Test-only and
//! experimental; see `docs/feasibility-04-c2/README.md`.
//!
//! The worker runs in Bubblewrap with no network namespace connectivity and a
//! seccomp filter that denies sockets and new processes. It can only describe
//! the request it wants. The broker accepts it when it equals the request this
//! module derives from the authorized query for an authorized engine, and then
//! sends its own copy, never the worker's bytes or headers, through
//! [`NetworkBoundary::send`] under the research consent.

use std::{
    collections::{BTreeSet, HashMap},
    io::{BufRead, BufReader, Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, atomic::AtomicU64, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

use agent_core::{
    capability::{CapabilityRegistry, Locality, Retention, WEB_SEARCH},
    classification::{Classified, DataClasses, Labels, QUERY, USER_INPUT},
    inference::Cancellation,
    intelligence::{EgressRecord, Transmission},
    research::{MAX_SEARCH_BYTES, SearchProviderKind, SearchQuery, SearchResult},
};
use futures_executor::block_on;
use serde_json::{Value, json};
use url::Url;

use super::{page::ResearchConsent, *};
use crate::{
    credentials::NoSecureStorage,
    fixture::{Authority, Fixture, Handler},
    policy::{Denial, Permission, Policy},
};

/// Largest protocol line accepted from the worker.
const MAX_LINE: u64 = 1024 * 1024;
/// Most protocol messages accepted from one worker.
const MAX_MESSAGES: usize = 64;

/// One upstream API: a fixed origin and path, and the exact parameters the
/// pinned SearXNG engine derives from a query.
struct Upstream {
    engine: &'static str,
    id: &'static str,
    host: &'static str,
    route: Route,
    parameters: fn(&str) -> Vec<(&'static str, String)>,
}

fn fixed(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
    pairs.iter().map(|(k, v)| (*k, (*v).to_owned())).collect()
}

const UPSTREAMS: [Upstream; 4] = [
    Upstream {
        engine: "arxiv",
        id: "search/managed/arxiv",
        host: "export.arxiv.org",
        route: Route::new("/api/query"),
        parameters: |q| {
            fixed(&[
                ("search_query", &format!("all:{q}")),
                ("start", "0"),
                ("max_results", "10"),
            ])
        },
    },
    Upstream {
        engine: "github",
        id: "search/managed/github",
        host: "api.github.com",
        route: Route::new("/search/repositories"),
        parameters: |q| fixed(&[("sort", "stars"), ("order", "desc"), ("q", q)]),
    },
    Upstream {
        engine: "stackoverflow",
        id: "search/managed/stackoverflow",
        host: "api.stackexchange.com",
        route: Route::new("/2.3/search/advanced"),
        parameters: |q| {
            fixed(&[
                ("q", q),
                ("page", "1"),
                ("pagesize", "10"),
                ("site", "stackoverflow"),
                ("sort", "activity"),
                ("order", "desc"),
            ])
        },
    },
    Upstream {
        engine: "wiby",
        id: "search/managed/wiby",
        host: "wiby.me",
        route: Route::new("/json/"),
        parameters: |q| fixed(&[("q", q), ("p", "1")]),
    },
];

const ALL: [&str; 4] = ["arxiv", "github", "stackoverflow", "wiby"];

/// Match a proposed URL against the engines authorized for this query.
/// Returns the upstream and the query string the host itself will send.
fn authorize(
    proposed: &str,
    query: &SearchQuery,
    engines: &[&str],
) -> Result<(&'static Upstream, String), &'static str> {
    let url = Url::parse(proposed).map_err(|_| "unparsable")?;
    if url.scheme() != "https" {
        return Err("scheme");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials");
    }
    if url.port().is_some() {
        return Err("port");
    }
    if url.fragment().is_some() {
        return Err("fragment");
    }
    let upstream = UPSTREAMS
        .iter()
        .find(|upstream| url.host_str() == Some(upstream.host))
        .ok_or("destination")?;
    if !engines.contains(&upstream.engine) {
        return Err("engine not authorized");
    }
    if url.path() != upstream.route.as_str() {
        return Err("path");
    }
    let expected = (upstream.parameters)(query.as_str());
    let proposed: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if proposed.len() != expected.len()
        || !proposed
            .iter()
            .zip(&expected)
            .all(|((k, v), (key, value))| k == key && v == value)
    {
        return Err("parameters");
    }
    let wire = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&expected)
        .finish();
    Ok((upstream, wire))
}

type Response = Result<(u16, Option<String>, Vec<u8>), Failure>;

/// One upstream exchange with its own one-handoff grants.
async fn exchange(
    boundary: &NetworkBoundary,
    consent: &ResearchConsent,
    upstream: &Upstream,
    wire: String,
) -> (Response, Option<EgressRecord>) {
    let policy = boundary.authority();
    let Some(destination) = policy.registered_destination(upstream.id) else {
        return (Err(Failure::Denied(Denial::UnknownDestination)), None);
    };
    let mut request = Outbound::new(
        consent.session(),
        WEB_SEARCH,
        "web-search",
        Locality::ThirdParty,
        destination,
        Method::Get,
        upstream.route,
        Classified::derived(Vec::new(), [&Labels::from_source(USER_INPUT)], QUERY),
        Retention::ProviderDeclared,
    );
    request.query = Some(wire);
    let until = consent.expires();
    let process = match policy.grant(Permission::Process, request.operation(), until) {
        Ok(grant) => grant,
        Err(denial) => return (Err(Failure::from_denial(denial)), Some(request.record())),
    };
    let share = match policy.grant(Permission::Share, request.operation(), until) {
        Ok(grant) => grant,
        Err(denial) => {
            let _ = policy.revoke(process);
            return (Err(Failure::from_denial(denial)), Some(request.record()));
        }
    };
    let revoke = || {
        let _ = policy.revoke(process);
        let _ = policy.revoke(share);
    };
    let mut stream = match boundary.send(request, process, share).await {
        Ok(stream) => stream,
        Err(failed) => {
            revoke();
            return (Err(failed.failure), Some(failed.record));
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
    let content_type = stream.content_type.clone();
    let (record, live) = stream.finish_checked().await;
    revoke();
    match failure.or(live.err()) {
        Some(failure) => (Err(failure), Some(record)),
        None => (Ok((status, content_type, body)), Some(record)),
    }
}

/// A seccomp program: no sockets (nor io_uring, which can open them), no new
/// processes (threads only), no ptrace.
fn seccomp() -> Vec<u8> {
    const ARCH_X86_64: u32 = 0xc000_003e;
    const ALLOW: u32 = 0x7fff_0000;
    const KILL: u32 = 0x8000_0000;
    const EPERM: u32 = 0x0005_0000 | 1;
    const ENOSYS: u32 = 0x0005_0000 | 38;
    const CLONE_THREAD: u32 = 0x0001_0000;
    // socket, fork, vfork, ptrace, io_uring_setup/enter/register
    const DENIED: [u32; 7] = [41, 57, 58, 101, 425, 426, 427];
    const CLONE: u32 = 56;
    const CLONE3: u32 = 435;
    let mut code = Vec::new();
    let mut op = |code_: u16, yes: u8, no: u8, value: u32| {
        code.extend(code_.to_ne_bytes());
        code.extend([yes, no]);
        code.extend(value.to_ne_bytes());
    };
    const LOAD: u16 = 0x20;
    const EQUAL: u16 = 0x15;
    const AT_LEAST: u16 = 0x35;
    const ANY_BIT: u16 = 0x45;
    const RETURN: u16 = 0x06;
    op(LOAD, 0, 0, 4);
    op(EQUAL, 1, 0, ARCH_X86_64);
    op(RETURN, 0, 0, KILL);
    op(LOAD, 0, 0, 0);
    // The x32 ABI and the reserved range.
    op(AT_LEAST, 0, 1, 512);
    op(RETURN, 0, 0, ENOSYS);
    // clone3 hides its flags behind a pointer; glibc falls back to clone.
    op(EQUAL, 0, 1, CLONE3);
    op(RETURN, 0, 0, ENOSYS);
    for syscall in DENIED {
        op(EQUAL, 0, 1, syscall);
        op(RETURN, 0, 0, EPERM);
    }
    op(EQUAL, 0, 3, CLONE);
    op(LOAD, 0, 0, 16); // low word of the flags argument
    op(ANY_BIT, 1, 0, CLONE_THREAD);
    op(RETURN, 0, 0, EPERM);
    op(RETURN, 0, 0, ALLOW);
    code
}

/// What the sandbox may read besides system libraries.
struct Pack {
    /// The worker script, bound at `/pack/worker.py`.
    script: PathBuf,
    /// An assembled SearXNG pack, when the real worker runs.
    contents: Option<PathBuf>,
    arguments: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Abort {
    /// Cancellation, Local-only mode or an expired authorization.
    Stopped(Failure),
    Deadline,
    Protocol(&'static str),
    Worker(String),
}

#[derive(Default, Debug)]
struct Report {
    results: Vec<SearchResult>,
    last: Value,
    refused: Vec<(String, &'static str)>,
    records: Vec<EgressRecord>,
    ready: Duration,
    total: Duration,
    stderr: String,
}

enum Event {
    Line(Vec<u8>),
    Oversize,
    Closed,
    Reply(u64, Response, Option<Box<EgressRecord>>),
}

fn command(pack: &Pack, filter: &Path) -> Command {
    let python = std::fs::canonicalize("/usr/bin/python3").expect("system python");
    let python = python.to_str().expect("python path").to_owned();
    let mut command = Command::new("/bin/sh");
    // Descriptor 3 carries the filter; the shell is replaced immediately.
    command
        .args(["-c", "exec \"$@\" 3<\"$0\""])
        .arg(filter)
        .args([
            "/usr/bin/prlimit",
            "--as=805306368:805306368",
            "--cpu=20:20",
            "--fsize=16777216:16777216",
            "--nofile=64:64",
            "--core=0:0",
            "--",
            "/usr/bin/bwrap",
            "--unshare-user",
            "--unshare-all",
            "--disable-userns",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ]);
    for (name, value) in [
        ("HOME", "/tmp"),
        ("PATH", "/nonexistent"),
        ("LANG", "C.UTF-8"),
        ("MALLOC_ARENA_MAX", "2"),
        ("SEARXNG_SETTINGS_PATH", "/pack/settings.yml"),
    ] {
        command.args(["--setenv", name, value]);
    }
    command.args([
        "--ro-bind",
        "/usr/lib",
        "/usr/lib",
        "--ro-bind-try",
        "/usr/lib64",
        "/usr/lib64",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--ro-bind",
        &python,
        &python,
        "--dev",
        "/dev",
        "--size",
        "16777216",
        "--tmpfs",
        "/tmp",
    ]);
    command
        .arg("--ro-bind")
        .arg(&pack.script)
        .arg("/pack/worker.py");
    if let Some(contents) = &pack.contents {
        for name in ["searxng/searx", "site-packages", "settings.yml"] {
            command
                .arg("--ro-bind")
                .arg(contents.join(name))
                .arg(format!("/pack/{name}"));
        }
    }
    command
        .args(["--remount-ro", "/", "--chdir", "/", "--seccomp", "3", "--"])
        .args([&python, "-I", "-S", "-B", "/pack/worker.py"])
        .args(&pack.arguments)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Run one worker for one authorized query. Every upstream attempt is
/// reported whether or not the search succeeds.
fn search(
    boundary: &NetworkBoundary,
    consent: &ResearchConsent,
    query: &SearchQuery,
    engines: &[&str],
    pack: &Pack,
    deadline: Duration,
) -> (Report, Result<(), Abort>) {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut report = Report::default();
    // Nothing starts without a live authorization.
    if let Err(failure) = consent.check(boundary.authority()) {
        return (report, Err(Abort::Stopped(failure)));
    }
    let filter = std::env::temp_dir().join(format!(
        "agent-managed-search-{}-{}.bpf",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::AcqRel)
    ));
    std::fs::write(&filter, seccomp()).expect("write filter");
    let started = Instant::now();
    let mut child = command(pack, &filter).spawn().expect("start sandbox");
    let mut input = child.stdin.take().expect("stdin");
    let output = child.stdout.take().expect("stdout");
    let errors = child.stderr.take().expect("stderr");
    // Writes happen off the control loop: a worker that stops reading must
    // not delay revocation.
    let (write, writes) = mpsc::channel::<Vec<u8>>();
    let writer = std::thread::spawn(move || {
        for line in writes {
            if input.write_all(&line).is_err() {
                break;
            }
        }
    });
    let send = |message: Value| {
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        let _ = write.send(line);
    };
    let diagnostics = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut errors = errors;
        let mut buffer = [0u8; 4096];
        while let Ok(read @ 1..) = errors.read(&mut buffer) {
            let room = 16 * 1024 - kept.len();
            kept.extend_from_slice(&buffer[..read.min(room)]);
        }
        String::from_utf8_lossy(&kept).into_owned()
    });
    let (events, received) = mpsc::channel::<Event>();
    let lines = events.clone();
    let reader = std::thread::spawn(move || {
        let mut output = BufReader::new(output);
        loop {
            let mut line = Vec::new();
            let read = (&mut output)
                .take(MAX_LINE + 1)
                .read_until(b'\n', &mut line);
            let event = match read {
                Ok(0) | Err(_) => Event::Closed,
                Ok(_) if line.len() as u64 > MAX_LINE => Event::Oversize,
                Ok(_) => Event::Line(line),
            };
            let last = !matches!(event, Event::Line(_));
            if lines.send(event).is_err() || last {
                break;
            }
        }
    });

    let outcome = std::thread::scope(|scope| {
        let mut searching = false;
        let mut used = BTreeSet::new();
        let mut messages = 0;
        let outcome = loop {
            if let Err(failure) = consent.check(boundary.authority()) {
                break Err(Abort::Stopped(failure));
            }
            if started.elapsed() >= deadline {
                break Err(Abort::Deadline);
            }
            let event = match received.recv_timeout(Duration::from_millis(10)) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(Abort::Protocol("channel"));
                }
            };
            let line = match event {
                Event::Line(line) => line,
                Event::Oversize => break Err(Abort::Protocol("oversized message")),
                Event::Closed => break Err(Abort::Worker("the worker exited".into())),
                Event::Reply(id, response, record) => {
                    report.records.extend(record.map(|record| *record));
                    send(match response {
                        Ok((status, content_type, body)) => json!({
                            "type": "response", "id": id, "status": status,
                            "contentType": content_type, "body": base64(&body),
                        }),
                        Err(failure) => json!({
                            "type": "refused", "id": id, "reason": format!("{failure:?}"),
                        }),
                    });
                    continue;
                }
            };
            messages += 1;
            if messages > MAX_MESSAGES {
                break Err(Abort::Protocol("too many messages"));
            }
            let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                break Err(Abort::Protocol("not JSON"));
            };
            match message["type"].as_str() {
                Some("ready") if !searching => {
                    searching = true;
                    report.ready = started.elapsed();
                    send(json!({
                        "type": "search", "query": query.as_str(), "engines": engines,
                    }));
                }
                Some("request") => {
                    let Some(id) = message["id"].as_u64() else {
                        break Err(Abort::Protocol("request without id"));
                    };
                    let url = message["url"].as_str().unwrap_or_default();
                    let admitted = if !searching {
                        Err("before the search")
                    } else if message["method"] != "GET" {
                        Err("method")
                    } else {
                        authorize(url, query, engines).and_then(|(upstream, wire)| {
                            if !used.insert(upstream.engine) {
                                return Err("repeated request");
                            }
                            consent.take().map_err(|_| "request budget")?;
                            Ok((upstream, wire))
                        })
                    };
                    match admitted {
                        Ok((upstream, wire)) => {
                            let events = events.clone();
                            scope.spawn(move || {
                                let (response, record) =
                                    block_on(exchange(boundary, consent, upstream, wire));
                                let _ =
                                    events.send(Event::Reply(id, response, record.map(Box::new)));
                            });
                        }
                        Err(reason) => {
                            report.refused.push((url.to_owned(), reason));
                            send(json!({"type": "refused", "id": id, "reason": reason}));
                        }
                    }
                }
                Some("results") => {
                    report.last = message;
                    break Ok(());
                }
                Some("fatal") => break Err(Abort::Worker(message.to_string())),
                _ => break Err(Abort::Protocol("unexpected message")),
            }
        };
        if outcome.is_err() {
            // Ends every exchange still in flight under this consent.
            consent.session().close();
        }
        let _ = child.kill();
        let _ = child.wait();
        outcome
    });
    // Exchanges that finished while the loop was ending.
    while let Ok(event) = received.try_recv() {
        if let Event::Reply(_, _, Some(record)) = event {
            report.records.push(*record);
        }
    }
    report.total = started.elapsed();
    drop(write);
    let _ = writer.join();
    let _ = reader.join();
    report.stderr = diagnostics.join().unwrap_or_default();
    let _ = std::fs::remove_file(&filter);
    // A revocation that raced with the last message still wins.
    let outcome =
        outcome.and_then(|()| consent.check(boundary.authority()).map_err(Abort::Stopped));
    if outcome.is_ok() {
        // The existing SearXNG normalization: public HTTPS URLs only,
        // de-duplicated, cleaned and bounded.
        let body = json!({"results": report.last["results"]}).to_string();
        match crate::research::providers::results(SearchProviderKind::Searxng, body.as_bytes()) {
            Ok(results) => report.results = results,
            Err(error) => return (report, Err(Abort::Worker(error.message))),
        }
    }
    (report, outcome)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (0..3).fold(0u32, |n, i| {
            (n << 8) | u32::from(chunk.get(i).copied().unwrap_or(0))
        });
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests. All are ignored by default: they need Linux user namespaces,
// Bubblewrap, prlimit and the system Python.
// ---------------------------------------------------------------------------

struct Fixed(HashMap<String, Vec<IpAddr>>);

impl Resolver for Fixed {
    fn resolve(&self, host: &str, _port: u16) -> ResolveFuture {
        let answer = self.0.get(host).cloned();
        Box::pin(async move { answer.ok_or_else(|| std::io::Error::other("no fixture answer")) })
    }
}

struct Host {
    policy: Arc<Policy>,
    boundary: NetworkBoundary,
    server: Option<Fixture>,
}

fn policy() -> Arc<Policy> {
    Arc::new(Policy::new(
        CapabilityRegistry::new([WEB_SEARCH]),
        DataClasses::default(),
        [],
        false,
    ))
}

fn destination(upstream: &Upstream) -> Destination {
    Destination::new(
        upstream.id,
        Endpoint::parse(&format!("https://{}", upstream.host)).unwrap(),
        Locality::ThirdParty,
    )
    .unwrap()
}

/// A boundary whose upstreams all lead to one loopback TLS fixture, or, with
/// `answer`, real public destinations resolving to that address.
fn host(handler: Option<Handler>, answer: Option<&str>) -> Host {
    let authority = Authority::new();
    let address: IpAddr = answer.unwrap_or("127.0.0.1").parse().unwrap();
    let hosts: Vec<&str> = UPSTREAMS.iter().map(|upstream| upstream.host).collect();
    let server =
        handler.map(|handler| Fixture::start(Some(authority.server(&hosts, false)), handler));
    let policy = policy();
    for upstream in &UPSTREAMS {
        let mut destination = destination(upstream);
        if let Some(server) = &server {
            destination.endpoint =
                Endpoint::parse(&format!("https://{}:{}", upstream.host, server.port)).unwrap();
            destination.scope = NetworkScope::Loopback;
        }
        policy.register_destination(destination).unwrap();
    }
    let boundary = NetworkBoundary::new(
        Arc::clone(&policy),
        Arc::new(NoSecureStorage),
        BoundaryConfig {
            resolver: Arc::new(Fixed(
                hosts
                    .iter()
                    .map(|host| ((*host).to_owned(), vec![address]))
                    .collect(),
            )),
            anchors: TrustAnchors::from_der([authority.root_der()]),
            limits: Limits::default(),
        },
    );
    Host {
        policy,
        boundary,
        server,
    }
}

fn consent(policy: &Policy, cancel: Cancellation, requests: usize) -> ResearchConsent {
    ResearchConsent::new(policy, cancel, requests, Duration::from_secs(60)).unwrap()
}

fn probe_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/feasibility-04-c2/probe")
        .canonicalize()
        .expect("probe directory")
}

fn hostile(mode: &str) -> Pack {
    Pack {
        script: probe_directory().join("hostile_worker.py"),
        contents: None,
        // The marker identifies this run's processes.
        arguments: vec![mode.into(), format!("marker-{mode}-{}", std::process::id())],
    }
}

fn searxng() -> Pack {
    let contents = PathBuf::from(
        std::env::var("AGENT_MANAGED_SEARCH_PACK")
            .expect("set AGENT_MANAGED_SEARCH_PACK to a directory built by build_pack.sh"),
    );
    Pack {
        script: probe_directory().join("worker.py"),
        contents: Some(contents),
        arguments: Vec::new(),
    }
}

fn processes_with(marker: &str) -> usize {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path().join("cmdline")).ok())
        .filter(|line| String::from_utf8_lossy(line).contains(marker))
        .count()
}

fn query(text: &str) -> SearchQuery {
    SearchQuery::new(text).unwrap()
}

const OK_JSON: &str = r#"{"items":[]}"#;

fn ok(body: &str, content_type: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nSet-Cookie: tracker=1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn always(response: String, delay: Duration) -> Handler {
    Arc::new(move |_: &[u8], out: &mut dyn Write| {
        std::thread::sleep(delay);
        let _ = out.write_all(response.as_bytes());
    })
}

#[test]
fn only_the_derived_request_is_authorized() {
    let q = query("bridge opening é ~* a&b=c");
    for upstream in &UPSTREAMS {
        let wire = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs((upstream.parameters)(q.as_str()))
            .finish();
        let good = format!(
            "https://{}{}?{wire}",
            upstream.host,
            upstream.route.as_str()
        );
        let (matched, sent) = authorize(&good, &q, &ALL).unwrap();
        assert_eq!(matched.engine, upstream.engine);
        assert_eq!(sent, wire);
        assert!(matches!(
            authorize(&good, &query("another query"), &ALL),
            Err("parameters")
        ));
        for (bad, reason) in [
            (format!("{good}&leak=secret"), "parameters"),
            (format!("{good}#x"), "fragment"),
            (good.replacen("https", "http", 1), "scheme"),
            (good.replacen("https://", "https://u:p@", 1), "credentials"),
            (
                good.replacen(upstream.host, &format!("{}:8443", upstream.host), 1),
                "port",
            ),
            (
                good.replacen(upstream.host, &format!("{}.evil.net", upstream.host), 1),
                "destination",
            ),
            (good.replacen(upstream.host, "127.0.0.1", 1), "destination"),
            (good.replacen('?', "/extra?", 1), "path"),
        ] {
            assert_eq!(authorize(&bad, &q, &ALL).err(), Some(reason), "{bad}");
        }
        assert_eq!(
            authorize(&good, &q, &[]).err(),
            Some("engine not authorized")
        );
    }
}

#[test]
#[ignore = "needs user namespaces, bubblewrap and the system Python"]
fn hostile_worker_is_contained_and_brokered() {
    let host = host(
        Some(always(ok(OK_JSON, "application/json"), Duration::ZERO)),
        None,
    );
    let consent = consent(&host.policy, Cancellation::default(), 2);
    let (report, outcome) = search(
        &host.boundary,
        &consent,
        &query("bridge opening"),
        &["github", "wiby"],
        &hostile("probe"),
        Duration::from_secs(20),
    );
    assert_eq!(outcome, Ok(()), "{}", report.stderr);
    let probes = &report.last["probes"];
    println!("sandbox probes: {probes:#}");
    for name in [
        "inet_socket",
        "unix_socket",
        "raw_socket",
        "libc_socket",
        "io_uring",
        "dns",
        "fork",
        "subprocess",
        "passwd",
        "home",
        "processes",
        "root_write",
        "pack_write",
    ] {
        assert!(probes[name].is_string(), "{name} was not probed");
        assert_ne!(probes[name], "allowed", "{name}");
    }
    assert_eq!(probes["thread"], "allowed");
    assert_eq!(
        probes["environment"],
        json!([
            "HOME",
            "LANG",
            "MALLOC_ARENA_MAX",
            "PATH",
            "PWD",
            "SEARXNG_SETTINGS_PATH"
        ])
    );
    let reasons: Vec<&str> = report.refused.iter().map(|(_, reason)| *reason).collect();
    println!("refusals: {reasons:?}");
    assert_eq!(
        reasons,
        [
            "before the search",
            "method",
            "parameters",
            "parameters",
            "scheme",
            "destination",
            "destination",
            "destination",
            "credentials",
            "port",
            "path",
            "engine not authorized",
            "repeated request",
        ]
    );
    // Exactly one request reached the network, built by the host.
    let server = host.server.as_ref().unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let sent = String::from_utf8_lossy(&requests[0]).to_ascii_lowercase();
    assert!(sent.starts_with(
        "get /search/repositories?sort=stars&order=desc&q=bridge+opening http/1.1\r\n"
    ));
    for forbidden in ["cookie", "authorization", "referer", "x-leak", "sec-fetch"] {
        assert!(!sent.contains(forbidden), "{sent}");
    }
    assert_eq!(report.records.len(), 1);
    let record = &report.records[0];
    assert_eq!(record.destination, "search/managed/github");
    assert_eq!(record.route, "/search/repositories");
    assert_eq!(record.method, "GET");
    assert_eq!(record.transmission, Transmission::Sent);
    assert!(!record.credential);
    assert!(!format!("{record:?}").contains("bridge"));
}

#[test]
#[ignore = "needs user namespaces, bubblewrap and the system Python"]
fn private_addresses_are_never_contacted() {
    for address in ["10.0.0.5", "127.0.0.1", "169.254.169.254", "::1"] {
        let host = host(None, Some(address));
        let consent = consent(&host.policy, Cancellation::default(), 1);
        let (report, outcome) = search(
            &host.boundary,
            &consent,
            &query("bridge opening"),
            &["github"],
            &hostile("probe"),
            Duration::from_secs(20),
        );
        assert_eq!(outcome, Ok(()), "{}", report.stderr);
        assert_eq!(host.boundary.connections(), 0, "{address}");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].transmission, Transmission::Unsent);
        assert!(
            report.last["replies"].to_string().contains("Address"),
            "{}",
            report.last["replies"]
        );
    }
}

#[test]
#[ignore = "needs user namespaces, bubblewrap and the system Python"]
fn local_only_and_cancellation_stop_the_worker_and_its_requests() {
    for local_only in [true, false] {
        let host = host(
            Some(always(
                ok(OK_JSON, "application/json"),
                Duration::from_secs(8),
            )),
            None,
        );
        let cancel = Cancellation::default();
        let consent = consent(&host.policy, cancel.clone(), 4);
        let pack = hostile("hang");
        let marker = pack.arguments[1].clone();
        let (report, outcome, stopped_after) = std::thread::scope(|scope| {
            let stopper = scope.spawn(|| {
                let seen = host.server.as_ref().unwrap().wait_requests(1);
                assert_eq!(seen.len(), 1, "the first request reached the fixture");
                assert!(processes_with(&marker) > 0);
                let at = Instant::now();
                if local_only {
                    host.policy.set_local_only(true);
                } else {
                    cancel.cancel();
                }
                at
            });
            let (report, outcome) = search(
                &host.boundary,
                &consent,
                &query("bridge opening"),
                &ALL,
                &pack,
                Duration::from_secs(20),
            );
            let at = stopper.join().unwrap();
            (report, outcome, at.elapsed())
        });
        println!("stopped {outcome:?} after {stopped_after:?}");
        let expected = if local_only {
            Failure::Denied(Denial::Locality)
        } else {
            Failure::Cancelled
        };
        assert_eq!(outcome, Err(Abort::Stopped(expected)));
        assert!(stopped_after < Duration::from_secs(1));
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].transmission, Transmission::Sent);
        assert!(report.results.is_empty());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(processes_with(&marker), 0, "the worker is gone");
        assert_eq!(host.server.as_ref().unwrap().requests().len(), 1);
        // The authorization cannot be used again.
        let (again, outcome) = search(
            &host.boundary,
            &consent,
            &query("bridge opening"),
            &ALL,
            &hostile("hang"),
            Duration::from_secs(5),
        );
        assert!(matches!(outcome, Err(Abort::Stopped(_))));
        assert!(again.records.is_empty());
    }
}

#[test]
#[ignore = "needs user namespaces, bubblewrap and the system Python"]
fn runaway_workers_are_bounded() {
    let host = host(None, Some("10.0.0.5"));
    for (mode, expected) in [
        ("memory", None),
        ("flood", Some(Abort::Protocol("oversized message"))),
        ("chatter", Some(Abort::Protocol("too many messages"))),
        ("spin", Some(Abort::Deadline)),
        ("exit", Some(Abort::Worker("the worker exited".into()))),
    ] {
        let consent = consent(&host.policy, Cancellation::default(), 1);
        let pack = hostile(mode);
        let marker = pack.arguments[1].clone();
        let (report, outcome) = search(
            &host.boundary,
            &consent,
            &query("bridge opening"),
            &["github"],
            &pack,
            Duration::from_secs(4),
        );
        println!("{mode}: {outcome:?} in {:?}", report.total);
        match expected {
            Some(expected) => assert_eq!(outcome, Err(expected), "{mode}"),
            None => {
                assert_eq!(outcome, Ok(()), "{mode}: {}", report.stderr);
                assert_eq!(report.last["probes"]["allocation"], "MemoryError");
            }
        }
        assert!(report.total < Duration::from_secs(6), "{mode}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(processes_with(&marker), 0, "{mode}");
    }
    assert_eq!(host.boundary.connections(), 0);
}

/// Canned API answers in the shapes the pinned engines parse.
fn upstream_answers() -> Handler {
    Arc::new(|request: &[u8], out: &mut dyn Write| {
        let line = String::from_utf8_lossy(request);
        let response = if line.starts_with("GET /search/repositories?") {
            ok(
                r#"{"items":[{"html_url":"https://github.com/fixture/bridge","full_name":"fixture/bridge","description":"A bridge repository","language":"Rust","updated_at":"2026-01-02T03:04:05Z","owner":{"login":"fixture"},"topics":[]}]}"#,
                "application/json",
            )
        } else if line.starts_with("GET /2.3/search/advanced?") {
            ok(
                r#"{"items":[{"question_id":1,"title":"Bridge &amp; borrow","tags":["rust"],"owner":{"display_name":"fixture"},"is_answered":true,"score":3}]}"#,
                "application/json",
            )
        } else if line.starts_with("GET /json/?") {
            ok(
                r#"[{"URL":"https://small.fixture-site.org/bridge","Title":"Small bridge page","Snippet":"A page about a bridge"},{"URL":"http://insecure.fixture-site.org/","Title":"dropped","Snippet":""}]"#,
                "application/json",
            )
        } else {
            ok(
                r#"<?xml version="1.0" encoding="UTF-8"?><feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom"><entry><id>https://arxiv.org/abs/0000.00001</id><title>Bridges</title><summary>On bridges.</summary><published>2026-01-02T03:04:05Z</published><author><name>A. Fixture</name></author></entry></feed>"#,
                "application/atom+xml",
            )
        };
        let _ = out.write_all(response.as_bytes());
    })
}

#[test]
#[ignore = "needs AGENT_MANAGED_SEARCH_PACK"]
fn real_searxng_is_brokered_against_fixtures() {
    let host = host(Some(upstream_answers()), None);
    let consent = consent(&host.policy, Cancellation::default(), ALL.len());
    let (report, outcome) = search(
        &host.boundary,
        &consent,
        &query("bridge opening é ~* a&b=c"),
        &ALL,
        &searxng(),
        Duration::from_secs(30),
    );
    assert_eq!(outcome, Ok(()), "{}", report.stderr);
    println!(
        "ready {:?}, total {:?}, max RSS {} KiB",
        report.ready, report.total, report.last["maxRssKib"]
    );
    println!("unresponsive: {}", report.last["unresponsive"]);
    // No startup traffic, and exactly one accepted request per engine.
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(report.records.len(), ALL.len());
    assert!(
        report
            .records
            .iter()
            .all(|record| record.transmission == Transmission::Sent)
    );
    assert_eq!(host.server.as_ref().unwrap().requests().len(), ALL.len());
    let mut urls: Vec<&str> = report.results.iter().map(|r| r.url.as_str()).collect();
    urls.sort_unstable();
    assert_eq!(
        urls,
        [
            "https://arxiv.org/abs/0000.00001",
            "https://github.com/fixture/bridge",
            "https://small.fixture-site.org/bridge",
            "https://stackoverflow.com/q/1",
        ]
    );
    // A second engine set: only what was authorized is asked.
    let narrow = host_requests(&host);
    let consent = self::consent(&host.policy, Cancellation::default(), 1);
    let (report, outcome) = search(
        &host.boundary,
        &consent,
        &query("bridge"),
        &["wiby"],
        &searxng(),
        Duration::from_secs(30),
    );
    assert_eq!(outcome, Ok(()), "{}", report.stderr);
    assert_eq!(report.records.len(), 1);
    assert_eq!(host_requests(&host), narrow + 1);
}

fn host_requests(host: &Host) -> usize {
    host.server.as_ref().unwrap().requests().len()
}

/// Opt-in live check with synthetic queries and the production resolver and
/// system trust store. One request per engine per query.
#[test]
#[ignore = "contacts public search APIs; needs AGENT_LIVE_WEB=1 and a pack"]
fn live_managed_search() {
    if std::env::var("AGENT_LIVE_WEB").as_deref() != Ok("1") {
        eprintln!("skipped: set AGENT_LIVE_WEB=1 to contact public APIs");
        return;
    }
    let policy = policy();
    for upstream in &UPSTREAMS {
        policy.register_destination(destination(upstream)).unwrap();
    }
    let boundary = NetworkBoundary::new(
        Arc::clone(&policy),
        Arc::new(NoSecureStorage),
        BoundaryConfig::default(),
    );
    let queries = [
        "rust borrow checker",
        "sqlite write ahead logging",
        "history of the bicycle",
    ];
    for (index, text) in queries.iter().enumerate() {
        if index > 0 {
            // arXiv asks for at most one request every three seconds.
            std::thread::sleep(Duration::from_secs(4));
        }
        let consent = consent(&policy, Cancellation::default(), ALL.len());
        let (report, outcome) = search(
            &boundary,
            &consent,
            &query(text),
            &ALL,
            &searxng(),
            Duration::from_secs(30),
        );
        println!(
            "\n== {text:?}: {outcome:?}; ready {:?}, total {:?}, max RSS {} KiB",
            report.ready, report.total, report.last["maxRssKib"]
        );
        println!("unresponsive: {}", report.last["unresponsive"]);
        println!("refused: {:?}", report.refused);
        for record in &report.records {
            println!(
                "  receipt {} {} {} {:?}",
                record.destination, record.method, record.route, record.transmission
            );
        }
        let mut engines: HashMap<String, usize> = HashMap::new();
        for item in report.last["results"].as_array().into_iter().flatten() {
            *engines
                .entry(item["engine"].as_str().unwrap_or("?").to_owned())
                .or_default() += 1;
        }
        println!("raw results per engine: {engines:?}");
        for result in &report.results {
            println!("  - {} | {}", result.title, result.url);
        }
    }
}
