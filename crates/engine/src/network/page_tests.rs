//! Public page retrieval against loopback TLS fixtures and fixed resolver
//! answers. Synthetic pages and addresses only; nothing leaves the machine.

use std::{
    collections::HashMap,
    io::Write,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent_core::{
    capability::{CapabilityRegistry, GENERATE, WEB_PAGE, WEB_SEARCH},
    classification::{DataClasses, HISTORY, Labels, PUBLIC_WEB, USER_INPUT},
    inference::Cancellation,
    intelligence::{EgressRecord, Transmission},
};
use futures_executor::block_on;

use super::*;
use crate::{
    credentials::NoSecureStorage,
    fixture::{Authority, Fixture, Handler},
    network::{BoundaryConfig, Limits, NetworkScope, ResolveFuture, Resolver, TrustAnchors},
    policy::Policy,
};

struct Fixed(HashMap<String, Vec<IpAddr>>);

impl Resolver for Fixed {
    fn resolve(&self, host: &str, _port: u16) -> ResolveFuture {
        let answer = self.0.get(host).cloned();
        Box::pin(async move { answer.ok_or_else(|| std::io::Error::other("no fixture answer")) })
    }
}

const HOSTS: &[&str] = &[
    "news.fixture-site.org",
    "mirror.fixture-site.net",
    "www.fixture-site.org",
];

struct Pages {
    policy: Arc<Policy>,
    boundary: NetworkBoundary,
    server: Option<Fixture>,
}

fn pages(handler: Option<Handler>, answers: &[(&str, &[&str])]) -> Pages {
    let authority = Authority::new();
    let mut table: HashMap<String, Vec<IpAddr>> = HOSTS
        .iter()
        .map(|host| ((*host).to_owned(), vec!["127.0.0.1".parse().unwrap()]))
        .collect();
    for (host, addresses) in answers {
        table.insert(
            (*host).to_owned(),
            addresses.iter().map(|a| a.parse().unwrap()).collect(),
        );
    }
    let policy = Arc::new(Policy::new(
        CapabilityRegistry::new([GENERATE, WEB_PAGE, WEB_SEARCH]),
        DataClasses::default(),
        [],
    ));
    let boundary = NetworkBoundary::new(
        Arc::clone(&policy),
        Arc::new(NoSecureStorage),
        BoundaryConfig {
            resolver: Arc::new(Fixed(table)),
            anchors: TrustAnchors::from_der([authority.root_der()]),
            limits: Limits {
                head: Duration::from_secs(3),
                idle: Duration::from_secs(3),
                total: Duration::from_secs(10),
                ..Limits::default()
            },
        },
    );
    let server =
        handler.map(|handler| Fixture::start(Some(authority.server(HOSTS, false)), handler));
    boundary.route_pages_to_fixture(server.as_ref().map(|server| server.port));
    Pages {
        policy,
        boundary,
        server,
    }
}

fn html(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nSet-Cookie: tracker=1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn redirect(location: &str) -> String {
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
}

/// Route by request line, so one fixture serves several paths.
fn routes(table: &[(&'static str, String)]) -> Handler {
    let table: Vec<(&'static str, String)> = table.to_vec();
    Arc::new(move |request: &[u8], out: &mut dyn Write| {
        let line = String::from_utf8_lossy(request);
        let line = line.lines().next().unwrap_or_default().to_owned();
        let response = table
            .iter()
            .find(|(prefix, _)| line.starts_with(prefix))
            .map(|(_, response)| response.clone())
            .unwrap_or_else(|| "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into());
        let _ = out.write_all(response.as_bytes());
    })
}

impl Pages {
    fn consent(&self, requests: usize) -> ResearchConsent {
        ResearchConsent::new(Cancellation::default(), requests, Duration::from_secs(30))
    }

    fn fetch(
        &self,
        consent: &ResearchConsent,
        url: &str,
    ) -> (Result<FetchedPage, PageError>, Vec<EgressRecord>) {
        let records = Mutex::new(Vec::new());
        let result = block_on(self.boundary.fetch_page(
            consent,
            PublicUrl::parse(url).unwrap(),
            &Labels::from_source(PUBLIC_WEB),
            |record| records.lock().unwrap().push(record),
        ));
        (result, records.into_inner().unwrap())
    }

    fn requests(&self) -> Vec<String> {
        self.server
            .as_ref()
            .map(|server| {
                server
                    .requests()
                    .iter()
                    .map(|request| String::from_utf8_lossy(request).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[test]
fn canonicalizes_public_urls_and_rejects_ambiguous_or_local_ones() {
    let url = PublicUrl::parse("HTTPS://News.Fixture-Site.ORG./a/../b%20c?q=1#frag").unwrap();
    assert_eq!(url.as_str(), "https://news.fixture-site.org/b%20c?q=1");
    assert_eq!(url.host(), "news.fixture-site.org");
    assert_eq!(url.without_query(), "https://news.fixture-site.org/b%20c");
    assert!(url.has_query());
    assert_eq!(
        PublicUrl::parse("https://bücher.example.org/")
            .unwrap()
            .host(),
        "xn--bcher-kva.example.org"
    );
    for (input, error) in [
        (
            "http://news.fixture-site.org/",
            PublicUrlError::InsecureScheme,
        ),
        ("ftp://news.fixture-site.org/", PublicUrlError::Scheme),
        ("file:///etc/passwd", PublicUrlError::Scheme),
        ("javascript:alert(1)", PublicUrlError::Scheme),
        (
            "https://user:pw@news.fixture-site.org/",
            PublicUrlError::Credentials,
        ),
        ("https://news.fixture-site.org:8443/", PublicUrlError::Port),
        ("https://127.0.0.1/", PublicUrlError::Host),
        ("https://2130706433/", PublicUrlError::Host),
        ("https://0x7f.1/", PublicUrlError::Host),
        ("https://[::1]/", PublicUrlError::Host),
        (
            "https://169.254.169.254/latest/meta-data",
            PublicUrlError::Host,
        ),
        ("https://localhost/", PublicUrlError::Host),
        ("https://intranet/", PublicUrlError::Host),
        ("https://router.local/", PublicUrlError::Host),
        ("https://metadata.google.internal/", PublicUrlError::Host),
        ("https://box.home.arpa/", PublicUrlError::Host),
        ("https://site.onion/", PublicUrlError::Host),
        ("https://under_score.example.org/", PublicUrlError::Host),
        ("https://news.fixture-site.org/a b", PublicUrlError::Invalid),
        (
            "https://news.fixture-site.org\\@evil.example.org/",
            PublicUrlError::Invalid,
        ),
        ("not a url", PublicUrlError::Invalid),
    ] {
        assert_eq!(PublicUrl::parse(input), Err(error), "{input}");
    }
    let long = format!("https://news.fixture-site.org/{}", "a".repeat(2_100));
    assert!(PublicUrl::parse(&long).is_err());
    // Redirect targets are validated as new URLs; downgrades are refused.
    assert_eq!(
        url.redirect("/next").unwrap().as_str(),
        "https://news.fixture-site.org/next"
    );
    assert_eq!(
        url.redirect("http://news.fixture-site.org/"),
        Err(PublicUrlError::InsecureScheme)
    );
    assert_eq!(url.redirect("https://10.0.0.1/"), Err(PublicUrlError::Host));
}

#[test]
fn reads_a_page_without_identifying_headers_and_records_the_path_only() {
    let p = pages(
        Some(routes(&[(
            "GET /story?id=7",
            html("<title>Story</title><p>Synthetic body.</p>"),
        )])),
        &[],
    );
    let consent = p.consent(5);
    let (result, records) = p.fetch(&consent, "https://news.fixture-site.org/story?id=7");
    let page = result.unwrap();
    assert_eq!(page.content_type, "text/html; charset=utf-8");
    assert!(
        String::from_utf8(page.body)
            .unwrap()
            .contains("Synthetic body.")
    );
    assert!(!page.truncated);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.destination, DESTINATION);
    assert_eq!(record.route, "/story");
    assert_eq!(record.method, "GET");
    assert_eq!(record.body_bytes, 0);
    assert!(!record.credential);
    assert_eq!(record.transmission, Transmission::Sent);
    assert_eq!(record.authorization.len(), 2);
    let requests = p.requests();
    assert_eq!(requests.len(), 1);
    let request = requests[0].to_ascii_lowercase();
    // The fixture listens on a loopback port, which the Host header names.
    assert!(request.starts_with("get /story?id=7 http/1.1\r\nhost: news.fixture-site.org:"));
    for header in [
        "cookie",
        "referer",
        "authorization",
        "x-subscription-token",
        "accept-language",
    ] {
        assert!(!request.contains(header), "{header}");
    }
    // A second page under the same consent never echoes the cookie it was offered.
    let (again, _) = p.fetch(&consent, "https://news.fixture-site.org/story?id=7");
    assert!(again.is_ok());
    assert!(!p.requests()[1].to_ascii_lowercase().contains("cookie"));
}

#[test]
fn every_redirect_is_validated_and_authorized_on_its_own() {
    let p = pages(
        Some(routes(&[
            (
                "GET /moved",
                redirect("https://mirror.fixture-site.net/final"),
            ),
            ("GET /final", html("<p>Final page.</p>")),
            (
                "GET /downgrade",
                redirect("http://mirror.fixture-site.net/final"),
            ),
            ("GET /private", redirect("https://10.1.2.3/admin")),
            ("GET /metadata", redirect("https://169.254.169.254/latest")),
            ("GET /local", redirect("https://printer.local/")),
            ("GET /loop", redirect("/loop")),
            (
                "GET /nolocation",
                "HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n".into(),
            ),
        ])),
        &[],
    );
    let consent = p.consent(64);
    let (result, records) = p.fetch(&consent, "https://news.fixture-site.org/moved");
    let page = result.unwrap();
    assert_eq!(page.url.as_str(), "https://mirror.fixture-site.net/final");
    assert_eq!(records.len(), 2);
    assert!(
        records[0]
            .origin
            .starts_with("https://news.fixture-site.org:")
    );
    assert!(
        records[1]
            .origin
            .starts_with("https://mirror.fixture-site.net:")
    );
    assert_ne!(records[0].authorization, records[1].authorization);

    for (path, expected) in [
        ("/downgrade", PageError::Url(PublicUrlError::InsecureScheme)),
        ("/private", PageError::Url(PublicUrlError::Host)),
        ("/metadata", PageError::Url(PublicUrlError::Host)),
        ("/local", PageError::Url(PublicUrlError::Host)),
        ("/nolocation", PageError::Url(PublicUrlError::Invalid)),
        ("/loop", PageError::TooManyRedirects),
    ] {
        let before = p.requests().len();
        let (result, records) = p.fetch(&consent, &format!("https://news.fixture-site.org{path}"));
        assert_eq!(result.err(), Some(expected), "{path}");
        let hops = if path == "/loop" {
            MAX_REDIRECTS + 1
        } else {
            1
        };
        assert_eq!(records.len(), hops, "{path}");
        assert_eq!(p.requests().len() - before, hops, "{path}");
    }
}

#[test]
fn private_and_mixed_dns_answers_are_refused_before_connecting() {
    // Real public scope: no fixture routing.
    let p = pages(
        None,
        &[
            ("rebind.fixture-site.org", &["93.184.216.34", "127.0.0.1"]),
            ("private.fixture-site.org", &["10.0.0.8"]),
            ("metadata.fixture-site.org", &["169.254.169.254"]),
            ("mapped.fixture-site.org", &["::ffff:192.168.1.1"]),
            ("nat64.fixture-site.org", &["64:ff9b::a00:1"]),
            ("loopback6.fixture-site.org", &["::1"]),
        ],
    );
    let consent = p.consent(16);
    for host in [
        "rebind.fixture-site.org",
        "private.fixture-site.org",
        "metadata.fixture-site.org",
        "mapped.fixture-site.org",
        "nat64.fixture-site.org",
        "loopback6.fixture-site.org",
    ] {
        let (result, records) = p.fetch(&consent, &format!("https://{host}/"));
        assert_eq!(
            result.err(),
            Some(PageError::Network(Failure::Address)),
            "{host}"
        );
        assert_eq!(records[0].transmission, Transmission::Unsent);
    }
    assert_eq!(p.boundary.connections(), 0);
}

#[test]
fn unsupported_restricted_and_oversized_responses_are_reported() {
    let big = "a".repeat(MAX_PAGE_BYTES + 4096);
    let p = pages(
        Some(routes(&[
            ("GET /pdf", "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: 4\r\n\r\n%PDF".into()),
            ("GET /untyped", "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".into()),
            ("GET /paywall", "HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 0\r\n\r\n".into()),
            ("GET /huge-declared", "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 99999999\r\n\r\n".into()),
            ("GET /big", html(&big)),
            ("GET /gzip", "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\nxx".into()),
        ])),
        &[],
    );
    let consent = p.consent(16);
    let url = |path: &str| format!("https://news.fixture-site.org{path}");
    assert_eq!(
        p.fetch(&consent, &url("/pdf")).0.err(),
        Some(PageError::Unsupported("application/pdf".into()))
    );
    assert!(matches!(
        p.fetch(&consent, &url("/untyped")).0.err(),
        Some(PageError::Unsupported(_))
    ));
    assert_eq!(
        p.fetch(&consent, &url("/paywall")).0.err(),
        Some(PageError::Status(403))
    );
    assert_eq!(
        p.fetch(&consent, &url("/huge-declared")).0.err(),
        Some(PageError::Network(Failure::TooLarge))
    );
    assert_eq!(
        p.fetch(&consent, &url("/gzip")).0.err(),
        Some(PageError::Network(Failure::Protocol))
    );
    let page = p.fetch(&consent, &url("/big")).0.unwrap();
    assert!(page.truncated);
    assert_eq!(page.body.len(), MAX_PAGE_BYTES);
}

#[test]
fn budget_cancellation_and_session_closure_stop_research_requests() {
    let p = pages(
        Some(routes(&[
            ("GET /moved", redirect("/final")),
            ("GET /final", html("<p>ok</p>")),
        ])),
        &[],
    );
    // The disclosed request budget covers redirect hops too.
    let small = p.consent(1);
    assert_eq!(
        p.fetch(&small, "https://news.fixture-site.org/moved")
            .0
            .err(),
        Some(PageError::Network(Failure::Denied(Denial::Scope)))
    );
    let accepted = p.requests().len();

    let cancel = Cancellation::default();
    let consent = ResearchConsent::new(cancel.clone(), 8, Duration::from_secs(30));
    cancel.cancel();
    assert_eq!(
        p.fetch(&consent, "https://news.fixture-site.org/final")
            .0
            .err(),
        Some(PageError::Network(Failure::Cancelled))
    );

    let consent = p.consent(8);
    consent.session().close();
    assert_eq!(
        p.fetch(&consent, "https://news.fixture-site.org/final")
            .0
            .err(),
        Some(PageError::Network(Failure::Cancelled))
    );
    assert_eq!(p.requests().len(), accepted);
}

#[test]
fn the_policy_refuses_any_widened_page_operation() {
    let p = pages(None, &[]);
    let consent = p.consent(4);
    let url = PublicUrl::parse("https://news.fixture-site.org/a").unwrap();
    let binding = |path: &str| RequestBinding {
        method: Method::Get,
        route: Route::new("/"),
        body_sha256: sha256(&[]),
        body_len: 0,
        artifact: None,
        page: Some(Binding {
            target_sha256: sha256(path.as_bytes()),
        }),
    };
    let operation = |capability, purpose, labels: Labels, destination: Destination, request| {
        Operation::new(
            &consent.session,
            capability,
            purpose,
            labels,
            destination.locality(),
            Connectivity::ThroughBoundary,
            Some(destination),
            Some(request),
            Retention::ProviderDeclared,
        )
    };
    let until = Instant::now() + Duration::from_secs(10);
    let public = destination(&url).unwrap();
    let web = Labels::from_source(PUBLIC_WEB);
    // The legitimate shape is admitted.
    assert!(
        p.policy
            .grant(
                Permission::Process,
                &operation(
                    WEB_PAGE,
                    "web-page",
                    web.clone(),
                    public.clone(),
                    binding("/a")
                ),
                until
            )
            .is_ok()
    );
    let mut personal = web.clone();
    personal.insert(HISTORY);
    let with_key = public.clone().with_credential("anything");
    let mut loopback = public.clone();
    loopback.scope = NetworkScope::Pinned(vec!["10.0.0.1".parse().unwrap()]);
    let mut unbound = binding("/a");
    unbound.page = None;
    let mut post = binding("/a");
    post.method = Method::Post;
    for (case, op) in [
        (
            "personal labels",
            operation(
                WEB_PAGE,
                "web-page",
                personal,
                public.clone(),
                binding("/a"),
            ),
        ),
        (
            "user input alone",
            operation(
                WEB_PAGE,
                "web-page",
                Labels::from_source(USER_INPUT),
                public.clone(),
                binding("/a"),
            ),
        ),
        (
            "credential",
            operation(WEB_PAGE, "web-page", web.clone(), with_key, binding("/a")),
        ),
        (
            "private scope",
            operation(WEB_PAGE, "web-page", web.clone(), loopback, binding("/a")),
        ),
        (
            "missing page binding",
            operation(WEB_PAGE, "web-page", web.clone(), public.clone(), unbound),
        ),
        (
            "method",
            operation(WEB_PAGE, "web-page", web.clone(), public.clone(), post),
        ),
        (
            "purpose",
            operation(WEB_PAGE, "chat", web.clone(), public.clone(), binding("/a")),
        ),
        (
            "other capability",
            operation(
                GENERATE,
                "web-page",
                web.clone(),
                public.clone(),
                binding("/a"),
            ),
        ),
    ] {
        assert_eq!(
            p.policy.grant(Permission::Process, &op, until).err(),
            Some(Denial::Scope),
            "{case}"
        );
    }
}
