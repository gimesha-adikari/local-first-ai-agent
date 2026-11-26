use super::*;

fn job(document_id: i64) -> IndexJob {
    IndexJob {
        cancellation: Cancellation::default(),
        progress: Mutex::new(IndexProgress {
            job_id: 1,
            document_id,
            phase: IndexPhase::Indexing,
            processed_chunks: 0,
            total_chunks: 0,
            window_count: None,
            error: None,
        }),
    }
}

#[test]
fn cancellation_is_accepted_before_commit_only() {
    let first = job(1);
    assert!(first.cancel().unwrap());
    assert_eq!(
        first.begin_commit(&first.cancellation).unwrap_err().code,
        AgentErrorCode::Cancelled
    );
    first.finish(Err(unavailable()), None);
    assert!(first.progress().unwrap().phase == IndexPhase::Cancelled);

    let second = job(2);
    second.begin_commit(&second.cancellation).unwrap();
    assert!(!second.cancel().unwrap());
    second.finish(Ok(2), None);
    assert_eq!(second.progress().unwrap().window_count, Some(2));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "uses the explicitly supplied AGENT_BGE_TEST_MODEL file and a small synthetic corpus"]
fn bge_synthetic_retrieval_measurements() {
    use agent_core::{
        knowledge::{DocumentFormat, append_chunks},
        semantic::VECTOR_BYTES,
    };
    use std::{
        path::PathBuf,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };
    use storage::knowledge::{DocumentImport, ImportOutcome};

    let model = PathBuf::from(std::env::var_os("AGENT_BGE_TEST_MODEL").unwrap());
    let dir = std::env::temp_dir().join(format!(
        "agent-bge-eval-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("corpus.sqlite3");
    let mut db = SqliteStore::open(&path).unwrap();
    let long = format!(
        "{} The aircraft deicing equipment uses propylene glycol.",
        "! ".repeat(700)
    );
    let snippets = [
        (
            "rail",
            "The railway station dispatches regional passenger trains every morning. Commuters buy tickets at the depot.",
        ),
        (
            "hives",
            "The apiary keeper inspects beehives and protects the queen bee. She harvests honey after the flowers bloom.",
        ),
        (
            "equipment",
            "The compressor asset code is ZX-4179. The maintenance technician replaces its air filter every month.",
        ),
        (
            "proxy",
            "The quartzbridge service is a reverse proxy for incoming web traffic. Restart it after updating TLS certificates.",
        ),
        (
            "tart",
            "The lemon tart must be refrigerated before serving. Let the custard set before slicing the dessert.",
        ),
        (
            "rain",
            "The orchard collects rainwater to irrigate apple trees. Workers check the tank intake after heavy storms.",
        ),
        (
            "database",
            "The finance team restores encrypted database backups from archival media once per quarter.",
        ),
        ("aircraft", long.as_str()),
    ];
    let mut ids = Vec::new();
    let mut indexed_chunks = 0;
    for (i, (name, body)) in snippets.into_iter().enumerate() {
        let mut chunks = Vec::new();
        append_chunks(&mut chunks, body, None, || Ok(())).unwrap();
        indexed_chunks += chunks.len();
        let result = db
            .save_knowledge(
                &DocumentImport {
                    name,
                    format: DocumentFormat::Txt,
                    fingerprint: &[i as u8 + 1; 32],
                    file_bytes: body.len(),
                    pdf_coverage: None,
                    chunks: &chunks,
                },
                None,
                |_| Ok(()),
            )
            .unwrap();
        match result {
            ImportOutcome::Imported { document_id } => ids.push(document_id),
            _ => panic!("expected distinct fixture"),
        }
    }
    let storage_before = std::fs::metadata(&path).unwrap().len();
    let database = Arc::new(Mutex::new(db));
    let worker = EmbeddingWorker::spawn().unwrap();
    let load_start = Instant::now();
    futures_executor::block_on(worker.submit(move |p| p.load(&model)).unwrap())
        .unwrap()
        .unwrap();
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let mut windows = 0;
    for &id in &ids {
        let db = Arc::clone(&database);
        windows += futures_executor::block_on(
            worker
                .submit(move |provider| run_index(provider, &db, id, &job(id)))
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    }
    let seconds = start.elapsed().as_secs_f64();
    assert!(
        windows > indexed_chunks,
        "the long input must use multiple vectors"
    );
    let physical = std::fs::metadata(&path).unwrap().len() - storage_before;
    eprintln!(
        "RUNTIME load_ms={load_ms:.2} docs={} chunks={indexed_chunks} windows={windows} index_ms={:.2} windows_per_second={:.2} vector_bytes={} sqlite_growth_bytes={physical}",
        ids.len(),
        seconds * 1000.0,
        windows as f64 / seconds,
        windows * VECTOR_BYTES
    );

    let questions = [
        ("ZX-4179", 2, true),
        ("quartzbridge", 3, true),
        ("propylene glycol", 7, true),
        ("Where can I board a commuter rail service?", 0, false),
        (
            "Who looks after colonies of honey-producing insects?",
            1,
            false,
        ),
        ("How should I chill a citrus dessert?", 4, false),
    ];
    for mode in [
        RetrievalMode::Keyword,
        RetrievalMode::Semantic,
        RetrievalMode::Hybrid,
    ] {
        let mut latencies = Vec::new();
        let (mut exact, mut paraphrases) = (0, 0);
        for (query, expected, literal) in questions {
            let request = KnowledgeSearch {
                query: query.into(),
                document_ids: None,
                limit: 3,
            };
            let db = Arc::clone(&database);
            let start = Instant::now();
            let result = if mode == RetrievalMode::Keyword {
                db.lock()
                    .unwrap()
                    .retrieve_knowledge(&request, mode, BGE_KEY, None)
                    .unwrap()
            } else {
                futures_executor::block_on(
                    worker
                        .submit(move |p| run_retrieval(p, &db, &request, mode))
                        .unwrap(),
                )
                .unwrap()
                .unwrap()
            };
            latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            let hit = result.hits.first();
            let correct = hit.is_some_and(|hit| hit.document_id == ids[expected]);
            if literal {
                exact += u32::from(correct);
            } else {
                paraphrases += u32::from(correct);
            }
            eprintln!(
                "{mode:?} {query:?} => {}",
                hit.map_or("-", |hit| hit.document_name.as_str())
            );
        }
        latencies.sort_by(f64::total_cmp);
        let median = (latencies[2] + latencies[3]) / 2.0;
        eprintln!(
            "RETRIEVAL {mode:?} exact_top1={exact}/3 paraphrase_top1={paraphrases}/3 median_ms={median:.3} min_ms={:.3} max_ms={:.3}",
            latencies[0], latencies[5]
        );
        if mode == RetrievalMode::Keyword {
            assert_eq!(exact, 3);
            assert_eq!(paraphrases, 0);
        } else {
            assert!(
                paraphrases >= 2,
                "expected embedding retrieval to find paraphrases"
            );
        }
    }
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    eprintln!(
        "MEMORY {}",
        status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))
            .unwrap()
    );
}

#[test]
fn restore_handles_missing_unloaded_and_unremembered_choices_without_models() {
    let database = Arc::new(Mutex::new(
        SqliteStore::open(std::path::Path::new(":memory:")).unwrap(),
    ));
    let state = SemanticState::new().unwrap();
    let restore = |state: &SemanticState| model_status(state).restore;
    state.restore_remembered(Arc::clone(&database));
    assert_eq!(restore(&state), RestoreState::NotRemembered);
    let missing =
        std::env::temp_dir().join(format!("agent-missing-bge-{}.gguf", std::process::id()));
    database
        .lock()
        .unwrap()
        .remember_embedding_model(&missing)
        .unwrap();
    state.restore_remembered(Arc::clone(&database));
    assert_eq!(restore(&state), RestoreState::Missing);
    // A missing file keeps the choice for a later launch.
    assert!(
        database
            .lock()
            .unwrap()
            .embedding_model_choice()
            .unwrap()
            .is_some()
    );
    database
        .lock()
        .unwrap()
        .set_embedding_restore(false)
        .unwrap();
    state.restore_remembered(Arc::clone(&database));
    assert_eq!(restore(&state), RestoreState::Unloaded);
    assert!(!state.loaded.load(Ordering::Acquire));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "restores the explicitly supplied AGENT_BGE_TEST_MODEL file"]
fn restore_reverifies_the_remembered_bge_file() {
    let path = std::path::PathBuf::from(std::env::var_os("AGENT_BGE_TEST_MODEL").unwrap());
    let wait = |state: &SemanticState| {
        for _ in 0..200 {
            let current = model_status(state).restore;
            if current != RestoreState::Pending {
                return current;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("restore did not finish");
    };
    let database = Arc::new(Mutex::new(
        SqliteStore::open(std::path::Path::new(":memory:")).unwrap(),
    ));
    database
        .lock()
        .unwrap()
        .remember_embedding_model(&path)
        .unwrap();
    let state = SemanticState::new().unwrap();
    let started = std::time::Instant::now();
    state.restore_remembered(Arc::clone(&database));
    assert_eq!(wait(&state), RestoreState::Restored);
    println!("restored in {} ms", started.elapsed().as_millis());
    assert!(state.loaded.load(Ordering::Acquire));

    // Same size, different bytes: the checksum gate rejects it and the
    // choice is forgotten.
    let forged = std::env::temp_dir().join(format!("agent-forged-bge-{}.gguf", std::process::id()));
    let file = std::fs::File::create(&forged).unwrap();
    file.set_len(std::fs::metadata(&path).unwrap().len())
        .unwrap();
    drop(file);
    database
        .lock()
        .unwrap()
        .remember_embedding_model(&forged)
        .unwrap();
    let fresh = SemanticState::new().unwrap();
    fresh.restore_remembered(Arc::clone(&database));
    let outcome = wait(&fresh);
    std::fs::remove_file(&forged).unwrap();
    assert_eq!(outcome, RestoreState::Incompatible);
    assert!(!fresh.loaded.load(Ordering::Acquire));
    assert_eq!(
        database.lock().unwrap().embedding_model_choice().unwrap(),
        None
    );
}
