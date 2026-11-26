#![cfg(all(feature = "llama", target_os = "linux"))]

use std::path::PathBuf;

use agent_core::{
    agent::{AgentError, AgentErrorCode},
    semantic::{DIMENSIONS, UnitVector, VECTOR_BYTES},
};
use inference::embedding::{BgeProvider, EmbeddingWorker};

#[test]
fn model_free_embedding_worker_reports_unavailable_and_rejects_wrong_weights() {
    let worker = EmbeddingWorker::spawn().unwrap();
    let name = std::env::temp_dir().join(format!("agent-bge-invalid-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&name)
        .unwrap();
    file.set_len(67_308_128).unwrap(); // Correct size alone cannot pass the hash gate.
    drop(file);
    let path = name.clone();
    let result = futures_executor::block_on(
        worker
            .submit(move |provider| {
                assert_eq!(
                    provider.session().err().unwrap().code,
                    AgentErrorCode::EmbeddingUnavailable
                );
                provider.load(&path)
            })
            .unwrap(),
    )
    .unwrap();
    std::fs::remove_file(name).unwrap();
    assert_eq!(
        result.unwrap_err().code,
        AgentErrorCode::EmbeddingIncompatible
    );
}

#[test]
#[ignore = "uses the explicitly supplied AGENT_BGE_TEST_MODEL file"]
fn verified_bge_produces_unit_vectors_and_evaluates_every_window() {
    let path = PathBuf::from(std::env::var_os("AGENT_BGE_TEST_MODEL").unwrap());
    let mut provider = BgeProvider::default();
    provider.load(&path).unwrap();
    let mut session = provider.session().unwrap();
    let train = session
        .query("How do I catch the regional train?", || Ok(()))
        .unwrap();
    let related = session
        .passage(
            "The railway station runs commuter trains every morning.",
            || Ok(()),
        )
        .unwrap();
    let unrelated = session
        .passage("Pastry chefs chill the lemon tart before serving.", || {
            Ok(())
        })
        .unwrap();
    assert!(train.cosine(&related[0].vector) > train.cosine(&unrelated[0].vector));
    let bytes = train.to_bytes();
    assert_eq!(bytes.len(), VECTOR_BYTES);
    assert!((train.cosine(&UnitVector::from_bytes(&bytes).unwrap()) - 1.0).abs() < 1e-5);
    assert_eq!(DIMENSIONS, 384);

    let long = "! ".repeat(700);
    let windows = session.passage(&long, || Ok(())).unwrap();
    assert_eq!((windows[0].start, windows[0].end), (0, 510));
    assert_eq!((windows[1].start, windows[1].end), (478, 700));
    let check_count = std::cell::Cell::new(0);
    let cancelled = session.passage(&long, || {
        let count = check_count.get() + 1;
        check_count.set(count);
        if count == 4 {
            return Err(AgentError::new(
                AgentErrorCode::Cancelled,
                "synthetic cancellation",
            ));
        }
        Ok(())
    });
    assert_eq!(cancelled.unwrap_err().code, AgentErrorCode::Cancelled);
}

/// Prints and checks query–passage similarity for the relevance floor used by
/// semantic and hybrid retrieval. Positive, paraphrased and unrelated queries
/// cover the Task 06-A manual review cases.
#[test]
#[ignore = "uses the explicitly supplied AGENT_BGE_TEST_MODEL file"]
fn verified_bge_relevance_floor_separates_related_and_unrelated_queries() {
    use agent_core::semantic::RELEVANCE_FLOOR;
    let path = PathBuf::from(std::env::var_os("AGENT_BGE_TEST_MODEL").unwrap());
    let mut provider = BgeProvider::default();
    provider.load(&path).unwrap();
    let mut session = provider.session().unwrap();
    let passages = [
        "PLATEN_TEST_PAGE_A_PORTRAIT\nPlaten PDF Phase 5 Synthetic Fixture A - Single Portrait Page",
        "PLATEN_TEST_PAGE_B1_LANDSCAPE\nPlaten PDF Phase 5 Synthetic Fixture B - Landscape Page 1 of 2",
        "To start the local API, open the backend folder and run bash run_dev.sh. The server listens on port 8080.",
        "The orchard gate closes at seven in the evening. Visitors must leave before dusk.",
        "Preheat the oven to 180 degrees and chill the lemon tart for two hours before serving.",
        "Regional trains depart from platform four every thirty minutes on weekdays.",
        "Invoices are due within thirty days. Late payments incur a two percent monthly fee.",
        "The quarterly report shows revenue grew twelve percent while operating costs fell.",
        "Section 4. Maintenance. The facilities team inspects the boiler every spring and replaces \
         air filters each autumn. Tenants report leaks through the online portal, and urgent \
         repairs are scheduled within twenty-four hours. Parking permits renew every January. \
         Bicycles must be stored in the basement rack, not in hallways. Quiet hours run from ten \
         at night until seven in the morning, and moving furniture during those hours is not \
         permitted. Guests may stay up to fourteen nights per year without registration.",
    ];
    let embedded: Vec<_> = passages
        .iter()
        .map(|text| session.passage(text, || Ok(())).unwrap().remove(0).vector)
        .collect();
    // (query, index of the relevant passage or None when nothing qualifies)
    let cases: &[(&str, Option<usize>)] = &[
        ("When does the orchard gate close?", Some(3)),
        ("What time do visitors have to leave the orchard?", Some(3)),
        ("How do I launch the development server?", Some(2)),
        ("Which port does the backend use?", Some(2)),
        ("How often do commuter trains leave?", Some(5)),
        ("What happens if I pay an invoice late?", Some(6)),
        ("How did sales change this quarter?", Some(7)),
        ("How long should the dessert be refrigerated?", Some(4)),
        ("landscape page", Some(1)),
        ("How quickly are urgent repairs handled?", Some(8)),
        ("Where can I keep my bike?", Some(8)),
        ("run", None),
        ("Write a four-line poem about rain", None),
        ("What is the capital of France?", None),
        ("Explain quantum entanglement simply", None),
        ("hello, how are you today?", None),
        ("Tell me a joke about cats", None),
        // The best passage literally contains "run"; lexical ranking also finds it.
        ("fins run in this selected file", Some(2)),
    ];
    let mut failures = Vec::new();
    for (query, relevant) in cases {
        let vector = session.query(query, || Ok(())).unwrap();
        let scores: Vec<f64> = embedded.iter().map(|p| vector.cosine(p)).collect();
        let (best, best_score) = scores
            .iter()
            .copied()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let formatted: Vec<String> = scores.iter().map(|s| format!("{s:.3}")).collect();
        println!(
            "{query:?} best={best} {best_score:.3} [{}]",
            formatted.join(" ")
        );
        match relevant {
            Some(index) if best != *index || best_score < RELEVANCE_FLOOR => failures.push(*query),
            None if best_score >= RELEVANCE_FLOOR => failures.push(*query),
            _ => {}
        }
    }
    assert!(failures.is_empty(), "misclassified: {failures:?}");
}
