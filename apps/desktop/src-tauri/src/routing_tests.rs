//! Task 06-A routing through the real storage layer, without models.

use std::path::Path;

use agent_core::{
    chat_context::{ChatIntent, EvidenceTask, Grounding},
    chat_route::route,
    knowledge::{DocumentFormat, append_chunks},
    semantic::{DIMENSIONS, UnitVector},
};
use storage::{
    SqliteStore,
    knowledge::{DocumentImport, ImportOutcome},
    semantic::RetrievalMode,
};

use super::{dense_unusable, prepare_knowledge};
use crate::chat_context::{KnowledgeSelection, retrieval_query};

fn library() -> (SqliteStore, i64) {
    let mut db = SqliteStore::open(Path::new(":memory:")).unwrap();
    let mut chunks = Vec::new();
    append_chunks(
        &mut chunks,
        "The backend listens on port 8080. Start it with bash run_dev.sh.\n",
        None,
        || Ok(()),
    )
    .unwrap();
    let ImportOutcome::Imported { document_id } = db
        .save_knowledge(
            &DocumentImport {
                name: "backend.md",
                format: DocumentFormat::Markdown,
                fingerprint: &[7; 32],
                file_bytes: 64,
                pdf_coverage: None,
                chunks: &chunks,
            },
            None,
            |_| Ok(()),
        )
        .unwrap()
    else {
        panic!("expected a new document");
    };
    (db, document_id)
}

fn prepare(
    db: &SqliteStore,
    id: i64,
    mode: RetrievalMode,
    grounding: Grounding,
    question: &str,
) -> super::PreparedKnowledge {
    let selection = KnowledgeSelection {
        mode,
        document_ids: vec![id],
        grounding,
    };
    let plan = route(question, None, grounding);
    let query = (plan.intent == ChatIntent::Question)
        .then(|| retrieval_query(&plan.query))
        .flatten();
    prepare_knowledge(db, &selection, &plan, query.as_deref(), None, None).unwrap()
}

#[test]
fn selected_documents_do_not_block_unrelated_requests() {
    let (db, id) = library();
    let poem = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Auto,
        "Write a four-line poem about rain",
    );
    assert_eq!(poem.coverage.intent, Some(ChatIntent::General));
    assert!(poem.sources.is_empty());
    // Explicit document grounding keeps the earlier contract.
    let strict = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Documents,
        "Write a four-line poem about rain",
    );
    assert_eq!(strict.coverage.intent, Some(ChatIntent::Question));
    assert_eq!(strict.coverage.grounding, Some(Grounding::Documents));
    assert!(strict.sources.is_empty());
}

#[test]
fn search_overview_and_questions_use_the_selected_document() {
    let (db, id) = library();
    let search = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Auto,
        "fins run in this selected file",
    );
    assert_eq!(search.coverage.intent, Some(ChatIntent::Search));
    assert_eq!(search.sources.len(), 1);
    assert_eq!(
        search.task,
        EvidenceTask::Search {
            target: "run".into()
        }
    );
    for question in ["what it contain", "what it do"] {
        let overview = prepare(&db, id, RetrievalMode::Semantic, Grounding::Auto, question);
        assert_eq!(overview.coverage.intent, Some(ChatIntent::Overview));
        assert_eq!(overview.sources.len(), 1);
        assert_eq!(overview.coverage.overview[0].total_chunks, 1);
        assert_eq!(overview.task, EvidenceTask::Overview { total_chunks: 1 });
    }
    let question = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Auto,
        "Which port does the backend listen on?",
    );
    assert_eq!(question.coverage.intent, Some(ChatIntent::Question));
    assert_eq!(question.sources.len(), 1);
}

#[test]
fn automatic_grounding_reports_keyword_fallback_without_a_dense_index() {
    let (db, id) = library();
    let selection = KnowledgeSelection {
        mode: RetrievalMode::Semantic,
        document_ids: vec![id],
        grounding: Grounding::Auto,
    };
    let plan = route(
        "Which port does the backend listen on?",
        None,
        Grounding::Auto,
    );
    let mut values = [0.0; DIMENSIONS];
    values[3] = 1.0;
    let vector = UnitVector::normalize(&values).unwrap();
    let query = retrieval_query(&plan.query);
    let prepared = prepare_knowledge(
        &db,
        &selection,
        &plan,
        query.as_deref(),
        Some(&vector),
        None,
    )
    .unwrap();
    assert_eq!(prepared.coverage.mode, RetrievalMode::Keyword);
    assert_eq!(
        prepared.coverage.requested_mode,
        Some(RetrievalMode::Semantic)
    );
    assert_eq!(prepared.sources.len(), 1);
    // Explicit grounding still rejects instead of falling back.
    let strict = KnowledgeSelection {
        grounding: Grounding::Documents,
        ..selection
    };
    let error = prepare_knowledge(&db, &strict, &plan, query.as_deref(), Some(&vector), None)
        .err()
        .unwrap();
    assert!(dense_unusable(&error));
}

/// Automatic settings for real GGUF files: bounded by the trained context and
/// with Thinking carried over from the saved row.
#[test]
#[ignore = "inspects AGENT_TEST_MODEL and AGENT_TEST_MODEL_ALT"]
fn automatic_settings_follow_each_real_model() {
    use agent_core::chat::ModelSettings;
    for variable in ["AGENT_TEST_MODEL", "AGENT_TEST_MODEL_ALT"] {
        let path = std::path::PathBuf::from(std::env::var_os(variable).unwrap());
        let (trained, _) = inference::trained_context(&path).unwrap();
        let mut saved = ModelSettings::recommended(2048);
        saved.enable_thinking = true;
        let settings =
            tauri::async_runtime::block_on(super::recommended_for_file(path.clone(), saved))
                .unwrap();
        println!(
            "{variable}: trained={trained} context={} max_tokens={} threads={}",
            settings.model.context_length, settings.max_tokens, settings.model.threads
        );
        assert!(settings.model.context_length <= trained.min(8192));
        assert!(settings.max_tokens < settings.model.context_length);
        assert!(settings.enable_thinking);
        settings.validate().unwrap();
    }
}

#[test]
fn explicit_document_questions_without_evidence_stay_grounded() {
    let (db, id) = library();
    // An unrelated request becomes ordinary Chat under automatic grounding.
    let general = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Auto,
        "What is the capital of France?",
    );
    assert_eq!(general.coverage.intent, Some(ChatIntent::General));
    // The same lack of evidence for a question about the documents stays
    // grounded and receives the fixed no-evidence reply.
    let grounded = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Auto,
        "What does the document say about the capital of France?",
    );
    assert_eq!(grounded.coverage.intent, Some(ChatIntent::Question));
    assert!(grounded.sources.is_empty());
    assert!(
        agent_core::chat_context::no_evidence_answer(&grounded.task)
            .starts_with("The selected documents")
    );
    let missing = prepare(
        &db,
        id,
        RetrievalMode::Keyword,
        Grounding::Documents,
        "search for walrus",
    );
    assert_eq!(
        agent_core::chat_context::no_evidence_answer(&missing.task),
        "The text \u{201c}walrus\u{201d} wasn't found in the selected documents."
    );
}
