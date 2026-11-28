use super::*;
use crate::{
    BackgroundTask, BlockingTask, Platform,
    credentials::NoSecureStorage,
    fixture::{Authority, Fixture, Handler},
    network::{BoundaryConfig, Limits, ResolveFuture, Resolver, TrustAnchors},
};
use futures_executor::block_on;
use inference::{InferenceWorker, LlamaProvider};
use std::{
    fs::{self, OpenOptions},
    thread,
    time::{Duration, Instant},
};
use storage::profile::{DatabaseKey, Profile};

const ETAG: &str = "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"";
const CDN: &str = "us.aws.cdn.hf.co";
const REDIRECT_PATH: &str = "/xet-bridge-us/0123456789abcdef01234567/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef?Signature=synthetic";
static NEXT: AtomicU64 = AtomicU64::new(1);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "agent-model-downloads-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct LocalFiles(PathBuf, u64);

impl ModelFiles for LocalFiles {
    fn inspection_path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn open(&self, name: &str, writable: bool, create: bool) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(writable)
            .create_new(create)
            .open(self.0.join(name))
    }
    fn free_bytes(&self) -> io::Result<u64> {
        Ok(self.1)
    }
    fn install(&self, partial: &str, final_name: &str) -> io::Result<()> {
        fs::hard_link(self.0.join(partial), self.0.join(final_name))?;
        fs::remove_file(self.0.join(partial))
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.0.join(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

struct LocalPlatform(u64);

impl Platform for LocalPlatform {
    fn spawn_blocking(&self, task: BlockingTask) {
        thread::spawn(task);
    }
    fn spawn(&self, task: BackgroundTask) {
        thread::spawn(move || block_on(task));
    }
    fn available_memory(&self) -> Option<u64> {
        Some(8 << 30)
    }
    fn model_files(&self, path: &Path) -> io::Result<Box<dyn ModelFiles>> {
        fs::create_dir_all(path)?;
        Ok(Box::new(LocalFiles(path.into(), self.0)))
    }
}

struct TestResolver(&'static [&'static str]);

impl Resolver for TestResolver {
    fn resolve(&self, _host: &str, _port: u16) -> ResolveFuture {
        let addresses = self.0.iter().map(|ip| ip.parse().unwrap()).collect();
        Box::pin(async move { Ok(addresses) })
    }
}

// Sufficient GGUF metadata for native inspection, with no model weights.
fn gguf() -> Vec<u8> {
    fn string(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&(value.len() as u64).to_le_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    let mut data = b"GGUF".to_vec();
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.extend_from_slice(&2u64.to_le_bytes());
    string(&mut data, "general.architecture");
    data.extend_from_slice(&8u32.to_le_bytes());
    string(&mut data, "llama");
    string(&mut data, "llama.context_length");
    data.extend_from_slice(&4u32.to_le_bytes());
    data.extend_from_slice(&4096u32.to_le_bytes());
    data.resize(data.len().next_multiple_of(32), 0);
    data
}

fn artifact(data: &[u8]) -> &'static Artifact {
    Box::leak(Box::new(Artifact {
        key: "fixture",
        file_name: "fixture.gguf",
        bytes: data.len() as u64,
        sha256: Box::leak(format!("{:x}", Sha256::digest(data)).into_boxed_str()),
        ..manifest::LFM25
    }))
}

struct Harness {
    engine: Arc<Engine>,
    fixture: Fixture,
    path: PathBuf,
    key: DatabaseKey,
    _scratch: Scratch,
}

impl Harness {
    fn new(
        space: u64,
        handler: Handler,
        fixture_route: bool,
        addresses: &'static [&'static str],
    ) -> Self {
        let scratch = Scratch::new();
        let path = scratch.0.join("profile.sqlite3");
        let key = DatabaseKey::generate().unwrap();
        let db = Profile::claim(&path).unwrap().open(&key).unwrap();
        let authority = Authority::new();
        let fixture = Fixture::start(
            Some(authority.server(&["huggingface.co", CDN], false)),
            handler,
        );
        let engine = Arc::new(
            Engine::with_network(
                Arc::new(LocalPlatform(space)),
                Arc::new(InferenceWorker::spawn(LlamaProvider::default).unwrap()),
                db,
                path.clone(),
                Arc::new(NoSecureStorage),
                BoundaryConfig {
                    resolver: Arc::new(TestResolver(addresses)),
                    anchors: TrustAnchors::from_der([authority.root_der()]),
                    limits: Limits::default(),
                },
            )
            .unwrap(),
        );
        if fixture_route {
            crate::network::artifact::route_fixture(&engine.policy, fixture.port);
        }
        Self {
            engine,
            fixture,
            path,
            key,
            _scratch: scratch,
        }
    }

    fn start(&self, artifact: &'static Artifact, restart: bool) -> Result<()> {
        self.engine
            .start_acquisition(artifact, Acquire::Download { restart })
    }

    fn wait(&self, artifact: &Artifact) -> DownloadRecord {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.engine.downloads.0.lock().unwrap().is_none() {
                return database(&self.engine.database)
                    .unwrap()
                    .model_download(artifact.key, artifact.sha256)
                    .unwrap()
                    .unwrap();
            }
            assert!(Instant::now() < deadline, "download fixture did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn partial(&self, artifact: &Artifact) -> PathBuf {
        self.engine.model_directory().join(partial_name(artifact))
    }
}

fn send(out: &mut dyn Write, head: &str, body: &[u8]) {
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(body);
    let _ = out.flush();
}

fn hub_redirect(artifact: &Artifact, target: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {target}\r\nX-Linked-Etag: \"{}\"\r\nX-Linked-Size: {}\r\nContent-Length: 0\r\n\r\n",
        artifact.sha256, artifact.bytes
    )
}

fn full(size: u64) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nETag: {ETAG}\r\n\r\n")
}

fn via_cdn(artifact: &'static Artifact, handler: Handler) -> Handler {
    Arc::new(move |request, output| {
        let request_text = std::str::from_utf8(request).unwrap();
        assert!(!request_text.contains("Authorization:") && !request_text.contains("Cookie:"));
        if request_text.contains("Host: huggingface.co") {
            assert!(request_text.starts_with(&format!(
                "GET /LiquidAI/LFM2.5-1.2B-Instruct-GGUF/resolve/{}/{}?download=true",
                artifact.revision, artifact.file_name
            )));
            assert!(!request_text.contains("If-Range:"));
            send(
                output,
                &hub_redirect(artifact, &format!("https://{CDN}{REDIRECT_PATH}")),
                &[],
            );
        } else {
            assert!(request_text.starts_with(&format!("GET {REDIRECT_PATH} HTTP/1.1")));
            handler(request, output);
        }
    })
}

#[test]
fn interruption_resumes_through_the_verified_cdn_and_registers_once() {
    let data = gguf();
    let artifact = artifact(&data);
    let data_on_wire = data.clone();
    let offset = 32;
    let handler = via_cdn(
        artifact,
        Arc::new(move |request, output| {
            let request = std::str::from_utf8(request).unwrap();
            if request.contains("Range: bytes=32-") {
                assert!(request.contains(&format!("If-Range: {ETAG}")));
                send(
                    output,
                    &format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 32-{}/{}\r\nETag: {ETAG}\r\n\r\n",
                        artifact.bytes - 32,
                        artifact.bytes - 1,
                        artifact.bytes,
                    ),
                    &data_on_wire[offset..],
                );
            } else {
                send(output, &full(artifact.bytes), &data_on_wire[..offset]);
            }
        }),
    );
    let h = Harness::new(u64::MAX, handler, true, &["127.0.0.1"]);
    h.start(artifact, false).unwrap();
    assert_eq!(h.wait(artifact).phase, "interrupted");
    assert_eq!(
        fs::metadata(h.partial(artifact)).unwrap().len(),
        offset as u64
    );
    assert!(h.engine.list_models().unwrap().is_empty());
    h.start(artifact, false).unwrap();
    assert_eq!(h.wait(artifact).phase, "installed");
    assert_eq!(h.fixture.requests().len(), 4);
    assert_eq!(fs::read(h.engine.final_path(artifact)).unwrap(), data);
    assert!(!h.partial(artifact).exists());
    let model = h.engine.list_models().unwrap().pop().unwrap();
    assert_eq!(model.artifact.as_deref(), Some(artifact.key));
    assert_eq!(h.engine.active_model(), None);
    assert_eq!(
        h.start(artifact, false).unwrap_err().code,
        ErrorCode::InvalidConfig
    );
    let record: String = {
        let eng = &h.engine;
        database(&eng.database)
            .unwrap()
            .model_download(artifact.key, artifact.sha256)
            .unwrap()
            .unwrap()
            .phase
    };
    assert_eq!(record, "installed");

    // Removing managed bytes retains the model id, settings and historic row.
    let model_path = h.engine.final_path(artifact);
    h.engine.active_model.store(model.id, Ordering::Release);
    assert_eq!(
        block_on(h.engine.remove_artifact(artifact, true))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    h.engine.active_model.store(0, Ordering::Release);
    block_on(h.engine.remove_artifact(artifact, true)).unwrap();
    assert!(!model_path.exists());
    assert_eq!(h.engine.list_models().unwrap()[0].id, model.id);
    let Harness {
        engine,
        path,
        key,
        _scratch,
        ..
    } = h;
    drop(engine);
    assert!(!fs::read(&path).unwrap().starts_with(b"SQLite format"));
    let db = Profile::claim(&path).unwrap().open(&key).unwrap();
    assert_eq!(
        db.model_download(artifact.key, artifact.sha256)
            .unwrap()
            .unwrap()
            .phase,
        "removed"
    );
}

#[test]
fn rejects_bad_integrity_size_framing_and_unapproved_targets() {
    let data = gguf();
    let artifact = artifact(&data);
    let tests: Vec<(Handler, &str, usize)> = vec![
        (
            Arc::new(move |_, out| {
                send(
                    out,
                    &full(artifact.bytes),
                    &vec![0; artifact.bytes as usize],
                )
            }),
            "incompatible",
            1,
        ),
        (
            Arc::new(move |_, out| send(out, &full(artifact.bytes + 1), &[])),
            "failed",
            1,
        ),
        (
            {
                let data = data.clone();
                Arc::new(move |_, out| {
                    send(out, &full(artifact.bytes), &data);
                    let _ = out.write_all(b"x");
                })
            },
            "failed",
            1,
        ),
        (
            Arc::new(move |_, out| {
                send(
                    out,
                    &hub_redirect(artifact, "http://us.aws.cdn.hf.co/oops"),
                    &[],
                )
            }),
            "interrupted",
            1,
        ),
        (
            Arc::new(move |_, out| {
                send(
                    out,
                    &hub_redirect(artifact, "https://us.aws.cdn.hf.co.attacker.invalid/data"),
                    &[],
                )
            }),
            "interrupted",
            1,
        ),
        (
            Arc::new(move |_, out| {
                send(out, &hub_redirect(artifact, "https://127.0.0.1/oops"), &[])
            }),
            "interrupted",
            1,
        ),
    ];
    for (handler, phase, connections) in tests {
        let h = Harness::new(u64::MAX, handler, true, &["127.0.0.1"]);
        h.start(artifact, false).unwrap();
        assert_eq!(h.wait(artifact).phase, phase);
        assert_eq!(h.fixture.accepted(), connections);
        assert!(h.engine.list_models().unwrap().is_empty());
        assert!(!h.engine.final_path(artifact).exists());
    }

    for addresses in [&["10.1.2.3"][..], &["1.1.1.1", "::ffff:127.0.0.1"][..]] {
        let h = Harness::new(
            u64::MAX,
            Arc::new(|_, _| panic!("forbidden connection")),
            false,
            addresses,
        );
        h.start(artifact, false).unwrap();
        assert_eq!(h.wait(artifact).phase, "interrupted");
        assert_eq!(h.engine.network().connections(), 0);
    }
}

#[test]
fn unsafe_continuation_is_rejected_and_a_full_response_can_restart() {
    for ignore_range in [false, true] {
        let data = gguf();
        let artifact = artifact(&data);
        let count = AtomicU64::new(0);
        let handler = Arc::new(move |request: &[u8], out: &mut dyn Write| {
            match count.fetch_add(1, Ordering::Relaxed) {
                0 => send(out, &full(artifact.bytes), &data[..32]),
                1 if !ignore_range => {
                    assert!(String::from_utf8_lossy(request).contains("Range: bytes=32-"));
                    send(
                        out,
                        &format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 31-{}/{}\r\nETag: {ETAG}\r\n\r\n",
                            artifact.bytes - 32,
                            artifact.bytes - 1,
                            artifact.bytes,
                        ),
                        &data[32..],
                    );
                }
                _ => send(out, &full(artifact.bytes), &data),
            }
        });
        let h = Harness::new(u64::MAX, handler, true, &["127.0.0.1"]);
        h.start(artifact, false).unwrap();
        assert_eq!(h.wait(artifact).phase, "interrupted");
        h.start(artifact, false).unwrap();
        let second = h.wait(artifact);
        if ignore_range {
            assert_eq!(second.phase, "installed");
        } else {
            assert_eq!(second.phase, "failed");
            assert!(second.representation.is_none());
            assert_eq!(fs::metadata(h.partial(artifact)).unwrap().len(), 32);
            h.start(artifact, false).unwrap();
            assert_eq!(h.wait(artifact).phase, "installed");
            assert!(!String::from_utf8_lossy(&h.fixture.requests()[2]).contains("Range: bytes"));
        }
    }
}

#[test]
fn a_curated_download_starts_directly_and_cancellation_preserves_the_partial() {
    let mut data = gguf();
    let saved = 1024 * 1024;
    data.resize(saved, 0);
    let artifact = &manifest::LFM25;
    let (sent, received) = std::sync::mpsc::channel();
    let handler = Arc::new(move |_: &[u8], out: &mut dyn Write| {
        send(out, &full(artifact.bytes), &data[..saved]);
        let _ = sent.send(());
        thread::sleep(Duration::from_secs(3));
    });
    let h = Harness::new(u64::MAX, handler, true, &["127.0.0.1"]);
    h.engine.start_model_download(artifact.key, false).unwrap();
    received.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        h.engine
            .start_model_download(artifact.key, false)
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while fs::metadata(h.partial(artifact)).unwrap().len() != saved as u64 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(h.engine.cancel_model_download(artifact.key).unwrap());
    let phase = h.wait(artifact);
    assert_eq!(phase.phase, "interrupted");
    assert_eq!(
        fs::metadata(h.partial(artifact)).unwrap().len(),
        saved as u64
    );
    assert!(phase.representation.is_some());
    assert_eq!(h.fixture.accepted(), 1);
    block_on(h.engine.remove_model_download(artifact.key, false)).unwrap();
    assert!(!h.partial(artifact).exists());
}

#[test]
fn low_disk_preflight_and_local_registration_recovery() {
    let data = gguf();
    let artifact = artifact(&data);
    let h = Harness::new(
        artifact.bytes,
        Arc::new(|_, _| panic!("low disk must stop before network")),
        true,
        &["127.0.0.1"],
    );
    h.start(artifact, false).unwrap();
    assert!(h.wait(artifact).error.unwrap().contains("free disk space"));
    assert_eq!(h.fixture.accepted(), 0);
    fs::write(h.engine.final_path(artifact), &data).unwrap();
    database(&h.engine.database)
        .unwrap()
        .set_model_download_phase(artifact.key, "ready", None)
        .unwrap();
    h.engine
        .start_acquisition(artifact, Acquire::Finish)
        .unwrap();
    assert_eq!(h.wait(artifact).phase, "installed");
    assert_eq!(h.fixture.accepted(), 0);
    assert_eq!(h.engine.list_models().unwrap().len(), 1);
}

#[test]
fn manual_import_still_works_and_catalog_removal_cannot_delete_external_files() {
    let data = gguf();
    let artifact = artifact(&data);
    let h = Harness::new(
        u64::MAX,
        Arc::new(|_, _| panic!("existing model")),
        true,
        &["127.0.0.1"],
    );
    let external = h.path.with_extension("gguf");
    fs::write(&external, &data).unwrap();
    let lease = h.engine.reserve().unwrap();
    let model = block_on(h.engine.register_model(&lease, external.clone())).unwrap();
    drop(lease);
    database(&h.engine.database)
        .unwrap()
        .set_model_sha256(model.id, artifact.sha256)
        .unwrap();
    assert_eq!(
        h.start(artifact, false).unwrap_err().code,
        ErrorCode::InvalidConfig
    );
    assert_eq!(
        block_on(h.engine.remove_artifact(artifact, true))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(fs::read(external).unwrap(), data);
    assert_eq!(h.fixture.accepted(), 0);
}

#[test]
#[ignore = "retrieves the single 64 MiB BGE artifact from its pinned public distribution into a disposable profile"]
fn live_bge_distribution() {
    let scratch = Scratch::new();
    let path = scratch.0.join("profile.sqlite3");
    let key = DatabaseKey::generate().unwrap();
    let engine = Arc::new(
        Engine::new(
            Arc::new(LocalPlatform(u64::MAX)),
            Arc::new(InferenceWorker::spawn(LlamaProvider::default).unwrap()),
            Profile::claim(&path).unwrap().open(&key).unwrap(),
            path,
        )
        .unwrap(),
    );
    let artifact = &manifest::BGE_SMALL_EN;
    engine.start_model_download(artifact.key, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(360);
    while engine.downloads.0.lock().unwrap().is_some() {
        if Instant::now() >= deadline {
            engine.cancel_model_download(artifact.key).unwrap();
            panic!("BGE transfer exceeded the live fixture deadline");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let entry = engine
        .model_catalog()
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.artifact == artifact)
        .unwrap();
    assert_eq!(entry.status, Status::Installed, "{:?}", entry.error);
    assert_eq!(entry.received, artifact.bytes);
    assert!(!engine.embedding_status().available);
    assert!(
        block_on(engine.use_downloaded_embedding(artifact.key))
            .unwrap()
            .available
    );
    assert_eq!(
        block_on(engine.remove_model_download(artifact.key, true))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    block_on(engine.unload_embedding_model()).unwrap();
    block_on(engine.remove_model_download(artifact.key, true)).unwrap();
    assert!(!engine.final_path(artifact).exists());
}
