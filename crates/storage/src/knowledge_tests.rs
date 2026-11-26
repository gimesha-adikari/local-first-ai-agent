use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentStore, TaskId},
    knowledge::{DocumentFormat, PdfCoverage, SourceReference, append_chunks},
};

use super::*;
use crate::knowledge::{
    DocumentImport, ImportOutcome, IndexingStatus, KnowledgeSearch, WriteStage,
};

fn save(
    store: &mut SqliteStore,
    name: &str,
    format: DocumentFormat,
    (seed, text): (u8, &str),
    replace: Option<i64>,
    checkpoint: impl FnMut(WriteStage) -> agent_core::agent::AgentResult<()>,
) -> agent_core::agent::AgentResult<ImportOutcome> {
    let mut chunks = Vec::new();
    let is_pdf = format == DocumentFormat::Pdf;
    append_chunks(&mut chunks, text, is_pdf.then_some(2), || Ok(()))?;
    store.save_knowledge(
        &DocumentImport {
            name,
            format,
            fingerprint: &[seed; 32],
            file_bytes: text.len(),
            pdf_coverage: is_pdf.then_some(PdfCoverage {
                page_count: 3,
                pages_examined: 3,
                pages_without_text: 2,
            }),
            chunks: &chunks,
        },
        replace,
        checkpoint,
    )
}

fn query(query: &str, document_ids: Option<Vec<i64>>, limit: u32) -> KnowledgeSearch {
    KnowledgeSearch {
        query: query.into(),
        document_ids,
        limit,
    }
}

#[test]
fn v5_migration_preserves_saved_domains_and_enables_fts5() {
    let database = TestDatabase::new();
    let legacy = Connection::open(database.path()).unwrap();
    for migration in [
        include_str!("../migrations/001_chat.sql"),
        include_str!("../migrations/002_agent.sql"),
        include_str!("../migrations/003_conversation_agent_turns.sql"),
        include_str!("../migrations/004_agent_turn_status.sql"),
        include_str!("../migrations/004_chat_continuation.sql"),
        include_str!("../migrations/005_bundled_model.sql"),
    ] {
        legacy.execute_batch(migration).unwrap();
    }
    legacy.execute_batch(
        "PRAGMA user_version=5;
         INSERT INTO conversations(id,title,created_at,updated_at) VALUES(42,'Saved chat',1,2);
         INSERT INTO messages(id,conversation_id,position,role,state,content,created_at)
          VALUES(1,42,0,'user','complete','Saved question',1),
                (2,42,1,'assistant','complete','Saved answer',2);
         INSERT INTO agent_tasks(id,state,finalized,created_at,updated_at) VALUES(91,'completed',1,1,2);
         INSERT INTO conversation_agent_turns(task_id,conversation_id,user_message_id,assistant_message_id,task_timeout_ms)
          VALUES(91,42,1,2,1000);
         INSERT INTO agent_task_events(task_id,sequence,created_at,event) VALUES(91,1,2,'{}');
         INSERT INTO agent_executions(operation_id,task_id,tool,status,created_at,updated_at)
          VALUES('saved_execution',91,'math.calculate','succeeded',1,2);"
    ).unwrap();
    let settings = ModelSettings::recommended(4096);
    let preferences = Preferences {
        defaults: settings,
        ..Preferences::default()
    };
    legacy
        .execute(
            "INSERT INTO application_preferences (id, preferences) VALUES(1, ?1)",
            [serde_json::to_string(&preferences).unwrap()],
        )
        .unwrap();
    legacy
        .execute(
            "INSERT INTO models(path,file_name,settings,last_used_at,bundled_resource)
         VALUES('/fixture/model.gguf','model.gguf',?1,2,'bundle-fixture')",
            [serde_json::to_string(&settings).unwrap()],
        )
        .unwrap();
    drop(legacy);

    let store = database.open();
    let (version, fts5): (String, i32) = store
        .connection
        .query_row(
            "SELECT sqlite_version(), sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(fts5, 1);
    eprintln!("bundled SQLite {version}, ENABLE_FTS5={fts5}");
    assert_eq!(
        store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(store.get(42).unwrap().messages[1].content, "Saved answer");
    assert_eq!(store.unified_conversation(42).unwrap().1.len(), 1);
    assert_eq!(store.preferences().unwrap().defaults, settings);
    assert_eq!(store.recent_models().unwrap()[0].file_name, "model.gguf");
    let task = store.agent_task(TaskId::new(91).unwrap()).unwrap();
    assert_eq!(task.events.len(), 1);
    assert_eq!(task.executions[0].operation_id, "saved_execution");
    assert!(store.knowledge_documents(None, 10).unwrap().is_empty());
}

#[test]
fn ranked_search_preserves_sources_filters_and_restart_persistence() {
    let database = TestDatabase::new();
    let mut store = database.open();
    save(
        &mut store,
        "notes.txt",
        DocumentFormat::Txt,
        (1, "filler\norchid\n"),
        None,
        |_| Ok(()),
    )
    .unwrap();
    save(
        &mut store,
        "botany.md",
        DocumentFormat::Markdown,
        (2, "# Orchid orchid orchid\n"),
        None,
        |_| Ok(()),
    )
    .unwrap();
    save(
        &mut store,
        "pages.pdf",
        DocumentFormat::Pdf,
        (3, "café orchid\n"),
        None,
        |_| Ok(()),
    )
    .unwrap();
    drop(store);

    let store = database.open();
    let hits = store.search_knowledge(&query("Orchid", None, 2)).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].document_id, 2);
    assert!(hits[0].score < hits[1].score);
    let lines = store
        .search_knowledge(&query("orchid", Some(vec![1]), 20))
        .unwrap();
    assert_eq!(
        lines[0].source,
        SourceReference::Lines {
            start_line: 1,
            end_line: 2
        }
    );
    let pages = store
        .search_knowledge(&query("CAFE orchid", Some(vec![3]), 20))
        .unwrap();
    assert_eq!(pages[0].source, SourceReference::Page { page: 2 });
    assert!(
        store
            .search_knowledge(&query("orchid", Some(vec![]), 20))
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search_knowledge(&query("orchid", Some(vec![40]), 20))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.knowledge_documents(None, 1).unwrap()[0].name,
        "pages.pdf"
    );
    assert_eq!(
        store.knowledge_documents(Some(3), 1).unwrap()[0].name,
        "botany.md"
    );
    assert_eq!(
        store.knowledge_document(3).unwrap().indexing_status,
        IndexingStatus::Partial
    );
}

#[test]
fn duplicate_and_cancelled_replacement_keep_prior_index_and_delete_removes_rows() {
    let database = TestDatabase::new();
    let mut store = database.open();
    assert_eq!(
        save(
            &mut store,
            "original.txt",
            DocumentFormat::Txt,
            (1, "firstvalue"),
            None,
            |_| Ok(())
        )
        .unwrap(),
        ImportOutcome::Imported { document_id: 1 }
    );
    assert_eq!(
        save(
            &mut store,
            "renamed.txt",
            DocumentFormat::Txt,
            (1, "firstvalue"),
            None,
            |_| Ok(())
        )
        .unwrap(),
        ImportOutcome::Duplicate { document_id: 1 }
    );
    save(
        &mut store,
        "other.txt",
        DocumentFormat::Txt,
        (2, "sharedvalue"),
        None,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        save(
            &mut store,
            "other.txt",
            DocumentFormat::Txt,
            (2, "sharedvalue"),
            Some(1),
            |_| Ok(())
        )
        .unwrap(),
        ImportOutcome::Duplicate { document_id: 2 }
    );
    let result = save(
        &mut store,
        "replacement.txt",
        DocumentFormat::Txt,
        (3, "secondvalue"),
        Some(1),
        |stage| {
            if matches!(stage, WriteStage::Committing) {
                return Err(AgentError::new(
                    AgentErrorCode::Cancelled,
                    "fixture cancellation",
                ));
            }
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().code, AgentErrorCode::Cancelled);
    drop(store);

    let mut store = database.open();
    assert_eq!(store.knowledge_document(1).unwrap().name, "original.txt");
    assert_eq!(
        store
            .search_knowledge(&query("firstvalue", None, 20))
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .search_knowledge(&query("secondvalue", None, 20))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        save(
            &mut store,
            "replacement.txt",
            DocumentFormat::Txt,
            (3, "secondvalue"),
            Some(1),
            |_| Ok(())
        )
        .unwrap(),
        ImportOutcome::Replaced { document_id: 1 }
    );
    assert!(
        store
            .search_knowledge(&query("firstvalue", None, 20))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .search_knowledge(&query("secondvalue", None, 20))
            .unwrap()
            .len(),
        1
    );
    store.delete_knowledge_document(1).unwrap();
    drop(store);

    let store = database.open();
    assert_eq!(
        store.knowledge_document(1).unwrap_err().code,
        AgentErrorCode::KnowledgeNotFound
    );
    assert!(
        store
            .search_knowledge(&query("secondvalue", None, 20))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .search_knowledge(&query("sharedvalue", None, 20))
            .unwrap()
            .len(),
        1
    );
    let indexed: u32 = store
        .connection
        .query_row("SELECT count(*) FROM knowledge_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(indexed, 1);
}

#[test]
fn malformed_search_input_is_rejected_or_treated_as_literal() {
    let mut store = in_memory();
    save(
        &mut store,
        "notes.txt",
        DocumentFormat::Txt,
        (1, "orchid"),
        None,
        |_| Ok(()),
    )
    .unwrap();
    for input in [
        query("", None, 20),
        query("\" ***", None, 20),
        query("orchid\0", None, 20),
        query(&"a".repeat(257), None, 20),
        query(&"a ".repeat(17), None, 20),
        query("orchid", None, 0),
        query("orchid", None, 21),
        query("orchid", Some(vec![0]), 20),
        query("orchid", Some(vec![1; 129]), 20),
    ] {
        assert_eq!(
            store.search_knowledge(&input).unwrap_err().code,
            AgentErrorCode::InvalidArguments
        );
    }
    for literal in [
        "orchid NOT bloom",
        "orchid\" OR \"bloom",
        "orch*",
        "orchid NOT*",
    ] {
        assert!(
            store
                .search_knowledge(&query(literal, None, 20))
                .unwrap()
                .is_empty(),
            "{literal}"
        );
    }
    assert_eq!(
        store
            .search_knowledge(&query("\"orchid", None, 20))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn document_capacity_allows_replacement_and_frees_space_on_delete() {
    let mut store = in_memory();
    for seed in 1..=128 {
        save(
            &mut store,
            "notes.txt",
            DocumentFormat::Txt,
            (seed, "small"),
            None,
            |_| Ok(()),
        )
        .unwrap();
    }
    let import = |store: &mut SqliteStore, replace| {
        save(
            store,
            "large.txt",
            DocumentFormat::Txt,
            (129, "newwords"),
            replace,
            |_| Ok(()),
        )
    };
    assert_eq!(
        import(&mut store, None).unwrap_err().code,
        AgentErrorCode::KnowledgeLimit
    );
    assert_eq!(
        import(&mut store, Some(1)).unwrap(),
        ImportOutcome::Replaced { document_id: 1 }
    );
    store.delete_knowledge_document(2).unwrap();
    let imported = save(
        &mut store,
        "large.txt",
        DocumentFormat::Txt,
        (130, "fresh"),
        None,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(imported, ImportOutcome::Imported { document_id: 129 });
}
