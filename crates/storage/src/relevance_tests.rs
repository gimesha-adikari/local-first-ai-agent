//! Task 06-A: relevance filtering, exact search, overview sampling and
//! follow-up context for Chat with selected documents.

use agent_core::{
    chat::TurnAction,
    chat_context::{AttemptContext, ChatIntent, KnowledgeCoverage},
    knowledge::{DocumentFormat, PdfCoverage, append_chunks},
    semantic::{BGE_KEY, DIMENSIONS, EmbeddingWindow, UnitVector},
};

use super::*;
use crate::{
    knowledge::{DocumentImport, ImportOutcome, KnowledgeSearch},
    semantic::RetrievalMode,
};

fn insert(store: &mut SqliteStore, seed: u8, body: &str, pdf_pages: Option<&[&str]>) -> i64 {
    let mut chunks = Vec::new();
    match pdf_pages {
        Some(pages) => {
            for (page, text) in pages.iter().enumerate() {
                append_chunks(&mut chunks, text, Some(page as u32 + 1), || Ok(())).unwrap();
            }
        }
        None => append_chunks(&mut chunks, body, None, || Ok(())).unwrap(),
    }
    match store
        .save_knowledge(
            &DocumentImport {
                name: if pdf_pages.is_some() {
                    "fixture.pdf"
                } else {
                    "notes.txt"
                },
                format: if pdf_pages.is_some() {
                    DocumentFormat::Pdf
                } else {
                    DocumentFormat::Txt
                },
                fingerprint: &[seed; 32],
                file_bytes: body.len().max(1),
                pdf_coverage: pdf_pages.map(|pages| PdfCoverage {
                    page_count: pages.len() as u32,
                    pages_examined: pages.len() as u32,
                    pages_without_text: 0,
                }),
                chunks: &chunks,
            },
            None,
            |_| Ok(()),
        )
        .unwrap()
    {
        ImportOutcome::Imported { document_id } => document_id,
        outcome => panic!("unexpected {outcome:?}"),
    }
}

fn unit(x: f32, y: f32) -> UnitVector {
    let mut values = [0.0; DIMENSIONS];
    values[0] = x;
    values[1] = y;
    UnitVector::normalize(&values).unwrap()
}

fn index_one(store: &mut SqliteStore, id: i64, x: f32, y: f32) {
    let snapshot = store.embedding_snapshot(id).unwrap();
    let windows: Vec<_> = snapshot
        .chunks
        .iter()
        .map(|_| {
            vec![EmbeddingWindow {
                start: 0,
                end: 1,
                vector: unit(x, y),
            }]
        })
        .collect();
    store
        .install_embeddings(&snapshot, BGE_KEY, &windows, || Ok(()), || Ok(()))
        .unwrap();
}

const PLATEN: [&str; 3] = [
    "PLATEN_TEST_PAGE_A_PORTRAIT\nPlaten PDF Phase 5 Synthetic Fixture A - Single Portrait Page\n",
    "PLATEN_TEST_PAGE_B1_LANDSCAPE\nPlaten PDF Phase 5 Synthetic Fixture B - Landscape Page 1 of 2\n",
    "PLATEN_TEST_PAGE_B2_LANDSCAPE\nPlaten PDF Phase 5 Synthetic Fixture B - Landscape Page 2 of 2\n",
];

#[test]
fn dense_hits_below_the_relevance_floor_are_not_returned() {
    let mut db = in_memory();
    let pdf = insert(&mut db, 1, "", Some(&PLATEN));
    // cos((1,0),(0.5,0.866)) = 0.5: below the 0.56 floor.
    index_one(&mut db, pdf, 0.5, 0.866);
    let request = KnowledgeSearch {
        query: "run".into(),
        document_ids: Some(vec![pdf]),
        limit: 4,
    };
    let query = unit(1.0, 0.0);
    for mode in [RetrievalMode::Semantic, RetrievalMode::Hybrid] {
        let result = db
            .retrieve_knowledge(&request, mode, BGE_KEY, Some(&query))
            .unwrap();
        assert!(result.hits.is_empty(), "{mode:?} returned unrelated pages");
        let chat = db
            .retrieve_for_chat(Some("run"), &[pdf], mode, BGE_KEY, Some(&query))
            .unwrap();
        assert_eq!(chat.coverage.retrieved, 0);
        assert_eq!(chat.coverage.embeddings.unwrap().indexed_documents, 1);
    }
    // A relevant vector still retrieves, and the exact keyword still ranks.
    let related = unit(0.8, 0.6);
    let semantic = db
        .retrieve_knowledge(&request, RetrievalMode::Semantic, BGE_KEY, Some(&related))
        .unwrap();
    assert_eq!(semantic.hits.len(), 3);
    let keyword = KnowledgeSearch {
        query: "landscape".into(),
        document_ids: Some(vec![pdf]),
        limit: 4,
    };
    let hybrid = db
        .retrieve_knowledge(&keyword, RetrievalMode::Hybrid, BGE_KEY, Some(&query))
        .unwrap();
    assert_eq!(hybrid.hits.len(), 2, "exact keyword matches are preserved");
}

#[test]
fn chat_keyword_ranking_ignores_document_words_and_weak_overlap() {
    let mut db = in_memory();
    let pdf = insert(&mut db, 1, "", Some(&PLATEN));
    let notes = insert(
        &mut db,
        2,
        "To start the API, run bash run_dev.sh. This file explains the selected port.",
        None,
    );
    let chat = |question: &str| {
        db.retrieve_for_chat(
            Some(question),
            &[pdf],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap()
        .coverage
        .retrieved
    };
    assert_eq!(chat("fins run in this selected file"), 0);
    assert_eq!(chat("Write a four-line poem about rain"), 0);
    assert_eq!(chat("landscape orientation fixture"), 2);
    let both = db
        .retrieve_for_chat(
            Some("How do I run the API?"),
            &[pdf, notes],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap();
    assert_eq!(both.sources.len(), 1);
    assert_eq!(both.sources[0].document_id, notes);
}

#[test]
fn exact_search_and_overview_read_only_the_selection() {
    let mut db = in_memory();
    let pdf = insert(&mut db, 1, "", Some(&PLATEN));
    let long: String = (0..10)
        .map(|index| format!("Section {index}. {}\n", "filler words ".repeat(150)))
        .collect();
    let report = insert(&mut db, 2, &long, None);
    let other = insert(&mut db, 3, "run fast, run far", None);

    let found = db
        .search_for_chat("landscape page", &[pdf], RetrievalMode::Semantic)
        .unwrap();
    assert_eq!(found.coverage.retrieved, 2);
    assert!(found.sources.iter().all(|hit| hit.document_id == pdf));
    assert!(found.coverage.embeddings.is_none());
    let missing = db
        .search_for_chat("run", &[pdf], RetrievalMode::Keyword)
        .unwrap();
    assert!(
        missing.sources.is_empty(),
        "other documents are never searched"
    );
    assert_eq!(
        db.search_for_chat("run", &[other], RetrievalMode::Keyword)
            .unwrap()
            .coverage
            .retrieved,
        1
    );
    // Phrase order matters for exact search.
    assert!(
        db.search_for_chat("page landscape", &[pdf], RetrievalMode::Keyword)
            .unwrap()
            .sources
            .is_empty()
    );

    let chunks = db.knowledge_document(report).unwrap().chunk_count;
    assert!(chunks >= 6, "fixture must span several passages");
    let overview = db
        .overview_for_chat(&[report, pdf, 999], RetrievalMode::Keyword)
        .unwrap();
    assert_eq!(overview.sources.len(), 4);
    assert_eq!(overview.coverage.unavailable_document_ids, [999]);
    let counts: Vec<_> = overview
        .coverage
        .overview
        .iter()
        .map(|item| (item.document_id, item.supplied_chunks, item.total_chunks))
        .collect();
    assert_eq!(counts, [(report, 2, chunks), (pdf, 2, 3)]);
    let report_ordinals: Vec<_> = overview
        .sources
        .iter()
        .filter(|hit| hit.document_id == report)
        .map(|hit| hit.ordinal)
        .collect();
    assert_eq!(report_ordinals, [0, chunks / 2]);
    let small = db
        .overview_for_chat(&[pdf], RetrievalMode::Keyword)
        .unwrap();
    assert_eq!(small.coverage.overview[0].supplied_chunks, 3);
    assert_eq!(small.sources.len(), 3);
}

fn coverage(intent: ChatIntent) -> KnowledgeCoverage {
    KnowledgeCoverage {
        mode: RetrievalMode::Keyword,
        intent: Some(intent),
        grounding: None,
        overview: Vec::new(),
        requested_mode: None,
        embeddings: None,
        partial_document_ids: Vec::new(),
        unavailable_document_ids: Vec::new(),
        missing_index_document_ids: Vec::new(),
        incompatible_index_document_ids: Vec::new(),
        unsearched_document_ids: Vec::new(),
        retrieved: 1,
    }
}

#[test]
fn follow_ups_use_only_the_previous_document_question() {
    let mut db = in_memory();
    let conversation = db.create().unwrap().id;
    assert_eq!(
        db.previous_document_question(conversation, false).unwrap(),
        None
    );
    let ordinary = db
        .begin_chat(conversation, TurnAction::Send("hello".into()), None)
        .unwrap();
    finish_complete(&mut db, ordinary.assistant.id, "hi");
    assert_eq!(
        db.previous_document_question(conversation, false).unwrap(),
        None
    );

    let grounded = db
        .begin_chat_with_context(
            conversation,
            TurnAction::Send("Which port does the backend use?".into()),
            None,
            AttemptContext {
                coverage: Some(coverage(ChatIntent::Question)),
                memory_enabled: Some(false),
                fitted: None,
                research: None,
                attachments: None,
            },
        )
        .unwrap();
    finish_complete(&mut db, grounded.assistant.id, "8080 [S1]");
    assert_eq!(
        db.previous_document_question(conversation, false)
            .unwrap()
            .as_deref(),
        Some("Which port does the backend use?")
    );
    // When regenerating, the latest question is the current one.
    assert_eq!(
        db.previous_document_question(conversation, true).unwrap(),
        None
    );

    let general = db
        .begin_chat_with_context(
            conversation,
            TurnAction::Send("Write a poem".into()),
            None,
            AttemptContext {
                coverage: Some(coverage(ChatIntent::General)),
                memory_enabled: Some(true),
                fitted: None,
                research: None,
                attachments: None,
            },
        )
        .unwrap();
    finish_complete(&mut db, general.assistant.id, "Rain falls");
    assert_eq!(
        db.previous_document_question(conversation, false).unwrap(),
        None
    );
}

#[test]
fn ordinary_routing_coverage_persists_and_refinements_reach_their_question() {
    let mut db = in_memory();
    let conversation = db.create().unwrap().id;
    let mut general = coverage(ChatIntent::General);
    general.retrieved = 0;
    let model = agent_core::chat::ChatStore::remember_model(
        &mut db,
        std::path::Path::new("/m/model.gguf"),
        "model.gguf",
        agent_core::chat::ModelSettings::recommended(4096),
    )
    .unwrap();
    let routed = db
        .begin_routed_chat_attempt(
            conversation,
            TurnAction::Send("Write a poem".into()),
            None,
            model.id,
            agent_core::chat::ModelSettings::recommended(4096),
            &general,
        )
        .unwrap();
    finish_complete(&mut db, routed.assistant.id, "Rain falls");
    let saved = db.get(conversation).unwrap();
    let answer = saved.messages.last().unwrap();
    assert_eq!(
        answer
            .route_coverage
            .as_ref()
            .and_then(|coverage| coverage.intent),
        Some(ChatIntent::General)
    );
    // Ordinary answers stay ordinary: no contextual marker.
    assert!(answer.context.is_none());
    assert_eq!(
        db.previous_document_question(conversation, false).unwrap(),
        None
    );

    for (question, answer) in [
        ("Which port does the backend use?", "8080 [S1]"),
        ("make it shorter", "8080."),
    ] {
        let turn = db
            .begin_chat_with_context(
                conversation,
                TurnAction::Send(question.into()),
                None,
                AttemptContext {
                    coverage: Some(coverage(ChatIntent::Question)),
                    memory_enabled: Some(false),
                    fitted: None,
                    research: None,
                    attachments: None,
                },
            )
            .unwrap();
        finish_complete(&mut db, turn.assistant.id, answer);
    }
    // A second refinement revises the original question, not the first refinement.
    assert_eq!(
        db.previous_document_question(conversation, false)
            .unwrap()
            .as_deref(),
        Some("Which port does the backend use?")
    );
}
