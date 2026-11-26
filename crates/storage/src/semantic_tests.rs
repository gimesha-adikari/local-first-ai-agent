use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    knowledge::{DocumentFormat, SourceReference, append_chunks},
    semantic::{BGE_KEY, DIMENSIONS, EmbeddingKey, EmbeddingWindow, UnitVector},
};

use super::*;
use crate::{
    knowledge::{DocumentImport, ImportOutcome, KnowledgeSearch},
    semantic::{IndexCoverage, IndexSnapshot, RetrievalMode},
};

fn insert(store: &mut SqliteStore, seed: u8, body: &str, replace: Option<i64>) -> i64 {
    let mut chunks = Vec::new();
    append_chunks(&mut chunks, body, None, || Ok(())).unwrap();
    match store
        .save_knowledge(
            &DocumentImport {
                name: "snippet.txt",
                format: DocumentFormat::Txt,
                fingerprint: &[seed; 32],
                file_bytes: body.len(),
                pdf_coverage: None,
                chunks: &chunks,
            },
            replace,
            |_| Ok(()),
        )
        .unwrap()
    {
        ImportOutcome::Imported { document_id } | ImportOutcome::Replaced { document_id } => {
            document_id
        }
        ImportOutcome::Duplicate { .. } => panic!("unique fixture expected"),
    }
}

fn vector(x: f32, y: f32) -> UnitVector {
    let mut values = [0.0; DIMENSIONS];
    values[0] = x;
    values[1] = y;
    UnitVector::normalize(&values).unwrap()
}

fn windows(x: f32, y: f32) -> Vec<Vec<EmbeddingWindow>> {
    vec![vec![EmbeddingWindow {
        start: 0,
        end: 1,
        vector: vector(x, y),
    }]]
}

fn index(
    store: &mut SqliteStore,
    id: i64,
    key: EmbeddingKey,
    value: &[Vec<EmbeddingWindow>],
) -> IndexSnapshot {
    let snapshot = store.embedding_snapshot(id).unwrap();
    store
        .install_embeddings(&snapshot, key, value, || Ok(()), || Ok(()))
        .unwrap();
    snapshot
}

fn request(query: &str, ids: Option<Vec<i64>>, limit: u32) -> KnowledgeSearch {
    KnowledgeSearch {
        query: query.into(),
        document_ids: ids,
        limit,
    }
}

#[test]
fn populated_schema_six_migrates_without_rebuilding_keyword_index() {
    let fixture = TestDatabase::new();
    let db = Connection::open(fixture.path()).unwrap();
    for migration in [
        include_str!("../migrations/001_chat.sql"),
        include_str!("../migrations/002_agent.sql"),
        include_str!("../migrations/003_conversation_agent_turns.sql"),
        include_str!("../migrations/004_agent_turn_status.sql"),
        include_str!("../migrations/004_chat_continuation.sql"),
        include_str!("../migrations/005_bundled_model.sql"),
        include_str!("../migrations/006_knowledge.sql"),
    ] {
        db.execute_batch(migration).unwrap();
    }
    db.execute_batch(
        "PRAGMA user_version=6;
         INSERT INTO knowledge_documents (id,name,format,fingerprint,file_bytes,text_bytes,chunk_count,indexing_status,created_at,updated_at)
          VALUES(12,'old.txt','txt',zeroblob(32),17,17,1,'ready',20,20);
         INSERT INTO knowledge_chunks (id,document_id,ordinal,start_line,end_line)
          VALUES(81,12,0,2,3);
         INSERT INTO knowledge_fts(rowid,text) VALUES(81,'legacy blackberry');",
    ).unwrap();
    drop(db);
    let store = fixture.open();
    assert_eq!(
        store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let result = store
        .search_knowledge(&request("blackberry", None, 1))
        .unwrap();
    assert_eq!(
        result[0].source,
        SourceReference::Lines {
            start_line: 2,
            end_line: 3
        }
    );
    assert_eq!(result[0].document_id, 12);
    assert_eq!(
        store
            .embedding_coverage(BGE_KEY, None)
            .unwrap()
            .missing_documents,
        1
    );
}

#[test]
fn exact_and_rrf_rank_with_filters_and_persist_window_references() {
    let fixture = TestDatabase::new();
    let mut store = fixture.open();
    let semantic = insert(&mut store, 1, "bus and ferry timetable", None);
    let lexical = insert(
        &mut store,
        2,
        "rail transport identifier\nstation open",
        None,
    );
    let missing = insert(&mut store, 3, "local cheese shop", None);
    let multi = vec![vec![
        EmbeddingWindow {
            start: 0,
            end: 510,
            vector: vector(0.0, 1.0),
        },
        EmbeddingWindow {
            start: 478,
            end: 607,
            vector: vector(1.0, 0.0),
        },
    ]];
    index(&mut store, semantic, BGE_KEY, &multi);
    index(&mut store, lexical, BGE_KEY, &windows(0.6, 0.8));
    drop(store);

    let store = fixture.open();
    let req = request("rail transport", None, 1);
    let query = vector(1.0, 0.0);
    let run = |mode, vector| {
        store
            .retrieve_knowledge(&req, mode, BGE_KEY, vector)
            .unwrap()
    };
    assert_eq!(
        run(RetrievalMode::Keyword, None).hits[0].document_id,
        lexical
    );
    let dense = run(RetrievalMode::Semantic, Some(&query));
    assert_eq!(dense.hits[0].document_id, semantic);
    assert_eq!(
        dense.hits[0].source,
        SourceReference::Lines {
            start_line: 1,
            end_line: 1
        }
    );
    assert_eq!(
        dense.coverage.unwrap(),
        IndexCoverage {
            indexed_documents: 2,
            missing_documents: 1,
            incompatible_documents: 0,
        }
    );
    assert_eq!(
        run(RetrievalMode::Hybrid, Some(&query)).hits[0].document_id,
        lexical
    );
    let req = request("rail transport", Some(vec![lexical, missing, 99]), 20);
    let filtered = store
        .retrieve_knowledge(&req, RetrievalMode::Hybrid, BGE_KEY, Some(&query))
        .unwrap();
    assert_eq!(filtered.hits.len(), 1);
    assert_eq!(filtered.hits[0].document_id, lexical);
    assert_eq!(filtered.coverage.unwrap().missing_documents, 1);
    let none = store
        .retrieve_knowledge(
            &request("rail", Some(vec![]), 2),
            RetrievalMode::Semantic,
            BGE_KEY,
            Some(&query),
        )
        .unwrap();
    assert!(none.hits.is_empty());
}

#[test]
fn cancellation_preserves_old_vectors_and_revisions_invalidate_them() {
    let mut store = in_memory();
    let id = insert(&mut store, 1, "original material", None);
    let prior = index(&mut store, id, BGE_KEY, &windows(1.0, 0.0));
    let cancel = || {
        Err(AgentError::new(
            AgentErrorCode::Cancelled,
            "synthetic cancellation",
        ))
    };
    let err = store
        .install_embeddings(&prior, BGE_KEY, &windows(0.0, 1.0), || Ok(()), cancel)
        .unwrap_err();
    assert_eq!(err.code, AgentErrorCode::Cancelled);
    let req = request("material", None, 1);
    let query = vector(1.0, 0.0);
    let result = store
        .retrieve_knowledge(&req, RetrievalMode::Semantic, BGE_KEY, Some(&query))
        .unwrap();
    assert_eq!(result.hits[0].score, 1.0);

    assert_eq!(insert(&mut store, 2, "replaced material", Some(id)), id);
    assert_eq!(
        store
            .embedding_coverage(BGE_KEY, None)
            .unwrap()
            .missing_documents,
        1
    );
    assert_eq!(
        store
            .install_embeddings(&prior, BGE_KEY, &windows(1.0, 0.0), || Ok(()), || Ok(()))
            .unwrap_err()
            .code,
        AgentErrorCode::KnowledgeRevisionChanged,
    );
    let fresh = index(&mut store, id, BGE_KEY, &windows(0.0, 1.0));
    store.delete_knowledge_document(id).unwrap();
    let count: u32 = store
        .connection
        .query_row(
            "SELECT count(*) FROM knowledge_embedding_windows",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        store
            .install_embeddings(&fresh, BGE_KEY, &windows(1.0, 0.0), || Ok(()), || Ok(()))
            .unwrap_err()
            .code,
        AgentErrorCode::KnowledgeRevisionChanged,
    );
}

#[test]
fn incompatible_or_missing_indexes_do_not_turn_into_keyword_results() {
    let mut store = in_memory();
    let id = insert(&mut store, 1, "orchid greenhouse", None);
    let query = vector(1.0, 0.0);
    let req = request("orchid", None, 1);
    let err = store
        .retrieve_knowledge(&req, RetrievalMode::Semantic, BGE_KEY, None)
        .unwrap_err();
    assert_eq!(err.code, AgentErrorCode::EmbeddingUnavailable);
    let other_key = EmbeddingKey {
        fingerprint: [0x2; 32],
        preprocessing: BGE_KEY.preprocessing,
    };
    index(&mut store, id, other_key, &windows(1.0, 0.0));
    assert_eq!(
        store
            .embedding_coverage(BGE_KEY, None)
            .unwrap()
            .incompatible_documents,
        1
    );
    let err = store
        .retrieve_knowledge(&req, RetrievalMode::Hybrid, BGE_KEY, Some(&query))
        .unwrap_err();
    assert_eq!(err.code, AgentErrorCode::EmbeddingIndexUnavailable);
    assert_eq!(store.search_knowledge(&req).unwrap()[0].document_id, id);

    let snapshot = index(&mut store, id, BGE_KEY, &windows(0.0, 1.0));
    let gap = vec![vec![EmbeddingWindow {
        start: 1,
        end: 2,
        vector: vector(1.0, 0.0),
    }]];
    let err = store
        .install_embeddings(&snapshot, BGE_KEY, &gap, || Ok(()), || Ok(()))
        .unwrap_err();
    assert_eq!(err.code, AgentErrorCode::EmbeddingIncompatible);
    store
        .connection
        .execute(
            "UPDATE knowledge_embedding_windows SET vector=zeroblob(1536)",
            [],
        )
        .unwrap();
    let err = store
        .retrieve_knowledge(&req, RetrievalMode::Semantic, BGE_KEY, Some(&query))
        .unwrap_err();
    assert_eq!(err.code, AgentErrorCode::EmbeddingIncompatible);
}

#[test]
fn index_budget_rejects_overflow_and_reindex_excludes_prior_generation() -> AgentResult<()> {
    let mut store = in_memory();
    let candidate = insert(&mut store, 50, "candidate document", None);
    let first = insert(&mut store, 1, "first retained document", None);
    index(&mut store, first, BGE_KEY, &windows(1.0, 0.0));
    // Seed accounting rows at quota to test the guard without allocating 48 MiB of vectors.
    for seed in 2..=9 {
        let id = insert(&mut store, seed, &format!("retained document {seed}"), None);
        store
            .connection
            .execute(
                "INSERT INTO knowledge_embedding_indexes VALUES(?1,1,?2,1,?3)",
                params![
                    id,
                    BGE_KEY.fingerprint.as_slice(),
                    if seed == 9 { 4095 } else { 4096 }
                ],
            )
            .unwrap();
    }
    let snapshot = store.embedding_snapshot(candidate)?;
    let err = store
        .install_embeddings(&snapshot, BGE_KEY, &windows(1.0, 0.0), || Ok(()), || Ok(()))
        .unwrap_err();
    assert_eq!(err.code, AgentErrorCode::KnowledgeLimit);
    index(&mut store, first, BGE_KEY, &windows(0.0, 1.0));
    Ok(())
}
