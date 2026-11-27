use super::*;
use crate::{knowledge::DocumentImport, semantic::RetrievalMode};
use agent_core::{
    agent::AgentErrorCode,
    chat_context::ContextUsage,
    knowledge::{DocumentFormat, PdfCoverage, SourceReference, append_chunks},
    semantic::BGE_KEY,
};

fn document(
    db: &mut SqliteStore,
    fingerprint: u8,
    text: &str,
    replace: Option<i64>,
    pdf: bool,
) -> i64 {
    use crate::knowledge::ImportOutcome;
    let mut chunks = Vec::new();
    append_chunks(&mut chunks, text, pdf.then_some(2), || Ok(())).unwrap();
    match db
        .save_knowledge(
            &DocumentImport {
                name: if pdf { "guide.pdf" } else { "guide.txt" },
                format: if pdf {
                    DocumentFormat::Pdf
                } else {
                    DocumentFormat::Txt
                },
                fingerprint: &[fingerprint; 32],
                file_bytes: text.len(),
                pdf_coverage: pdf.then_some(PdfCoverage {
                    page_count: 3,
                    pages_examined: 2,
                    pages_without_text: 1,
                }),
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
        ImportOutcome::Duplicate { .. } => panic!("expected distinct bytes"),
    }
}

#[test]
fn chat_selection_tracks_partial_coverage_and_reuses_rankings() {
    use agent_core::semantic::{DIMENSIONS, EmbeddingWindow, UnitVector};
    let mut db = in_memory();
    let guide = document(&mut db, 1, "The orchard gate closes at seven.", None, true);
    let excluded = document(
        &mut db,
        2,
        "orchard gate gate gate closes at nine",
        None,
        false,
    );
    let unindexed = document(&mut db, 3, "orchard gate admission costs six", None, false);
    let ids = [guide, unindexed, 55];
    let mut values = [0.0; DIMENSIONS];
    values[17] = 1.0;
    let vector = UnitVector::normalize(&values).unwrap();
    let query = Some("What time does the orchard gate close?");
    let result = db
        .retrieve_for_chat(query, &[guide, 55], RetrievalMode::Keyword, BGE_KEY, None)
        .unwrap();
    assert_eq!(result.coverage.partial_document_ids, [guide]);
    assert_eq!(result.coverage.unavailable_document_ids, [55]);
    assert_eq!(result.coverage.retrieved, 1);
    assert_eq!(result.sources[0].document_id, guide);
    assert_eq!(result.sources[0].source, SourceReference::Page { page: 2 });
    assert_eq!(
        db.retrieve_for_chat(
            Some("astrophysics"),
            &ids,
            RetrievalMode::Keyword,
            BGE_KEY,
            None
        )
        .unwrap()
        .coverage
        .retrieved,
        0
    );
    for mode in [RetrievalMode::Semantic, RetrievalMode::Hybrid] {
        assert_eq!(
            db.retrieve_for_chat(query, &ids, mode, BGE_KEY, Some(&vector))
                .err()
                .unwrap()
                .code,
            AgentErrorCode::EmbeddingIndexUnavailable
        );
    }
    let snapshot = db.embedding_snapshot(guide).unwrap();
    db.install_embeddings(
        &snapshot,
        BGE_KEY,
        &[vec![EmbeddingWindow {
            start: 0,
            end: 1,
            vector: UnitVector::normalize(&values).unwrap(),
        }]],
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let semantic = db
        .retrieve_for_chat(query, &ids, RetrievalMode::Semantic, BGE_KEY, Some(&vector))
        .unwrap();
    let coverage = semantic.coverage.embeddings.unwrap();
    assert_eq!(coverage.indexed_documents, 1);
    assert_eq!(coverage.missing_documents, 1);
    assert_eq!(
        semantic
            .sources
            .iter()
            .map(|hit| hit.document_id)
            .collect::<Vec<_>>(),
        [guide]
    );
    let hybrid = db
        .retrieve_for_chat(query, &ids, RetrievalMode::Hybrid, BGE_KEY, Some(&vector))
        .unwrap();
    assert_eq!(hybrid.coverage.retrieved, 2);
    assert!(hybrid.sources.iter().all(|hit| hit.document_id != excluded));
}

#[test]
fn only_fitted_citations_resolve_and_revisions_never_retarget() {
    let profile = TestDatabase::new();
    let mut db = profile.open();
    let conv = db.create().unwrap().id;
    let first = document(&mut db, 1, "Quartz quartz quartz", None, false);
    let second = document(
        &mut db,
        2,
        "A considerable body of ordinary text about quartz",
        None,
        true,
    );
    let candidates = db
        .retrieve_for_chat(
            Some("quartz"),
            &[first, second],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap()
        .sources;
    assert_eq!(candidates[0].document_id, first);
    let turn = db
        .begin_contextual_chat_attempt(conv, TurnAction::Send("Quartz?".into()), None)
        .unwrap();
    let id = turn.assistant.id;
    let answer = "<think>[S2]</think> Quartz [S1], fabricated [S9] and unfitted [S2].";
    let mut stats = synthetic_replay_stats();
    stats.context_usage = Some(ContextUsage {
        sources: 1,
        memories: 0,
    });
    db.finish_chat_with_sources(
        id,
        answer,
        GenerationState::Truncated,
        Some(&stats),
        &candidates,
    )
    .unwrap();
    assert_eq!(db.chat_source_references(id).unwrap().len(), 1);
    assert!(db.resolve_chat_source(id, "S2").unwrap().is_none());
    assert!(db.resolve_chat_source(id, "S9").unwrap().is_none());
    assert_eq!(
        db.resolve_chat_source(id, "S1").unwrap().as_deref(),
        Some("Quartz quartz quartz")
    );
    assert!(
        db.continuation_remaining(conv, id, 1)
            .unwrap_err()
            .message
            .contains("retrieve current")
    );
    drop(db);
    let mut db = profile.open();
    assert_eq!(db.chat_source_references(id).unwrap()[0].revision, 1);
    document(
        &mut db,
        3,
        "Different quartz text at the same ordinal",
        Some(first),
        false,
    );
    let refs = db.chat_source_references(id).unwrap();
    assert_eq!(refs[0].chunk_ordinal, 0);
    assert!(!refs[0].available);
    assert!(db.resolve_chat_source(id, "S1").unwrap().is_none());
    let new = db
        .retrieve_for_chat(
            Some("quartz"),
            &[first],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap()
        .sources;
    assert_eq!(new[0].revision, 2);
    let attempt = db
        .begin_contextual_chat_attempt(conv, TurnAction::Regenerate, None)
        .unwrap();
    let new_id = attempt.assistant.id;
    db.finish_chat_with_sources(
        new_id,
        "Revised quotation [S1]",
        GenerationState::Complete,
        Some(&stats),
        &new,
    )
    .unwrap();
    assert!(
        db.resolve_chat_source(new_id, "S1")
            .unwrap()
            .unwrap()
            .contains("Different")
    );
    db.delete_knowledge_document(first).unwrap();
    assert!(!db.chat_source_references(new_id).unwrap()[0].available);
    assert!(db.resolve_chat_source(new_id, "S1").unwrap().is_none());
    assert_eq!(db.get(conv).unwrap().messages[1].content, answer);
    assert!(db.agent_context(conv).unwrap().0[0].1.assistant.is_empty());
    let next = db
        .begin_chat(conv, TurnAction::Send("Another question".into()), None)
        .unwrap();
    assert!(next.history.turns.is_empty());
    assert_eq!(
        db.get(conv).unwrap().messages[2].content,
        "Revised quotation [S1]"
    );
}

#[test]
fn memories_persist_and_retrieve_only_when_enabled_and_relevant() {
    let profile = TestDatabase::new();
    let mut db = profile.open();
    assert!(!db.memory_enabled().unwrap());
    let memory = db.create_memory("Preferred tea is hibiscus.").unwrap();
    let irrelevant = db.create_memory("My bicycle is green.").unwrap();
    assert_eq!(
        db.list_memories("HIBISCUS", None, 100).unwrap()[0].id,
        memory.id
    );
    assert_eq!(
        db.list_memories("", Some(irrelevant.id), 100).unwrap()[0].id,
        memory.id
    );
    assert!(
        db.relevant_memories("Which tea do I prefer?")
            .unwrap()
            .is_empty()
    );
    db.set_memory_enabled(true).unwrap();
    let revision = db
        .edit_memory(memory.id, "Preferred tea is oolong.")
        .unwrap();
    assert_eq!(revision.created_at, memory.created_at);
    drop(db);
    let mut db = profile.open();
    assert!(db.memory_enabled().unwrap());
    assert_eq!(
        db.relevant_memories("Which tea do I prefer?").unwrap(),
        ["Preferred tea is oolong."]
    );
    assert!(
        db.relevant_memories("Show me my camera.")
            .unwrap()
            .is_empty()
    );
    assert!(db.create_memory(&"x".repeat(1025)).is_err());
    db.set_memory_enabled(false).unwrap();
    assert!(db.relevant_memories("Which tea?").unwrap().is_empty());
    assert_eq!(db.list_memories("", None, 100).unwrap().len(), 2);
    db.delete_memory(memory.id).unwrap();
    assert!(db.edit_memory(memory.id, "unused").is_err());
    db.set_memory_enabled(true).unwrap();
    assert!(db.relevant_memories("Which tea?").unwrap().is_empty());
    assert_eq!(db.clear_memories().unwrap(), 1);
    assert_eq!(db.clear_memories().unwrap(), 0);
    assert!(db.list_memories("", None, 100).unwrap().is_empty());
}

#[test]
fn encrypts_populated_schema_seven_without_changing_saved_state() {
    let profile = TestDatabase::new();
    let mut db = profile.open();
    let conversation = db.create().unwrap().id;
    let turn = db
        .begin_chat(
            conversation,
            TurnAction::Send("legacy question".into()),
            None,
        )
        .unwrap();
    finish_complete(&mut db, turn.assistant.id, "legacy answer");
    let doc = document(&mut db, 44, "legacy file", None, false);
    let snapshot = db.embedding_snapshot(doc).unwrap();
    let mut values = [0.0; agent_core::semantic::DIMENSIONS];
    values[0] = 1.0;
    let vector = agent_core::semantic::UnitVector::normalize(&values).unwrap();
    db.install_embeddings(
        &snapshot,
        BGE_KEY,
        &[vec![agent_core::semantic::EmbeddingWindow {
            start: 0,
            end: 1,
            vector,
        }]],
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    db.connection
        .execute_batch(
            "DROP TABLE web_research_settings; DROP TABLE model_downloads;
         DROP TABLE provider_usage; DROP TABLE intelligence_settings; DROP TABLE providers;
         ALTER TABLE models DROP COLUMN artifact_key; DROP TABLE intelligence_receipts; ALTER TABLE messages DROP COLUMN classification;
        DROP TABLE chat_source_references; DROP TABLE chat_context_attempts;
        DROP TABLE memory_settings; DROP TABLE personal_memories;
        DROP TABLE chat_route_coverage; ALTER TABLE models DROP COLUMN content_sha256;
        DROP TABLE model_runtime; ALTER TABLE models DROP COLUMN settings_mode;
        ALTER TABLE models DROP COLUMN content_key; PRAGMA user_version=7;",
        )
        .unwrap();
    drop(db);
    let db = profile.open_encrypted();
    assert_eq!(
        db.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(
        db.get(conversation).unwrap().messages[1].content,
        "legacy answer"
    );
    assert_eq!(
        db.embedding_coverage(BGE_KEY, Some(&[doc]))
            .unwrap()
            .indexed_documents,
        1
    );
    let keyword = db
        .retrieve_for_chat(
            Some("legacy"),
            &[doc],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap();
    assert_eq!(keyword.sources[0].document_id, doc);
    assert!(!db.memory_enabled().unwrap());
    assert!(
        db.chat_source_references(turn.assistant.id)
            .unwrap()
            .is_empty()
    );
    let violations: i64 = db
        .connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn fitted_evidence_and_coverage_survive_interruption_and_replacement() {
    use agent_core::chat_context::AttemptContext;
    let profile = TestDatabase::new();
    let mut db = profile.open();
    let doc = document(&mut db, 1, "The orchard passphrase is fern.", None, true);
    let retrieved = db
        .retrieve_for_chat(
            Some("orchard"),
            &[doc, 999],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap();
    let conversation = db.create().unwrap().id;
    let context = AttemptContext {
        coverage: Some(retrieved.coverage),
        memory_enabled: Some(false),
        fitted: None,
        research: None,
    };
    let first = db
        .begin_chat_with_context(
            conversation,
            TurnAction::Send("Orchard?".into()),
            None,
            context.clone(),
        )
        .unwrap()
        .assistant
        .id;
    assert!(
        db.get(conversation)
            .unwrap()
            .messages
            .last()
            .unwrap()
            .context
            .is_some()
    );
    assert!(db.chat_supplied_evidence(first).unwrap().is_empty());
    db.checkpoint_chat_evidence(
        first,
        &context,
        ContextUsage {
            sources: 1,
            memories: 0,
        },
        &retrieved.sources,
    )
    .unwrap();
    db.checkpoint(first, "The answer is [fern]. Invalid group [S1, S7].")
        .unwrap();
    drop(db);

    let mut db = profile.open();
    let saved = db.get(conversation).unwrap();
    let assistant = saved.messages.last().unwrap();
    assert_eq!(assistant.state, GenerationState::Interrupted);
    let saved_context = assistant.context.as_ref().unwrap();
    assert_eq!(saved_context.fitted.unwrap().sources, 1);
    assert_eq!(
        saved_context
            .coverage
            .as_ref()
            .unwrap()
            .unsearched_document_ids,
        [999]
    );
    assert_eq!(
        saved_context
            .coverage
            .as_ref()
            .unwrap()
            .partial_document_ids,
        [doc]
    );
    assert!(db.chat_source_references(first).unwrap().is_empty());
    assert!(db.resolve_chat_source(first, "S1").unwrap().is_none());
    assert!(
        db.resolve_chat_evidence(first, "S1")
            .unwrap()
            .unwrap()
            .contains("fern")
    );

    document(
        &mut db,
        2,
        "A replacement gives a different answer.",
        Some(doc),
        false,
    );
    let old = db.chat_supplied_evidence(first).unwrap();
    assert_eq!(old.len(), 1);
    assert!(!old[0].available);
    assert!(!old[0].cited);
    assert!(db.resolve_chat_evidence(first, "S1").unwrap().is_none());
    assert_eq!(
        db.chat_attempt_context(first)
            .unwrap()
            .unwrap()
            .coverage
            .unwrap()
            .partial_document_ids,
        [doc]
    );

    let failed = db
        .begin_chat_with_context(conversation, TurnAction::Regenerate, None, context)
        .unwrap()
        .assistant
        .id;
    db.finish(failed, "", GenerationState::Failed, None)
        .unwrap();
    let saved = db.get(conversation).unwrap();
    assert_eq!(
        saved.messages.last().unwrap().state,
        GenerationState::Failed
    );
    assert!(
        saved
            .messages
            .last()
            .unwrap()
            .context
            .as_ref()
            .unwrap()
            .fitted
            .is_none()
    );
    assert_eq!(saved.messages[1].state, GenerationState::Interrupted);
}

#[test]
fn schema_eight_citation_remains_available_without_invented_historical_coverage() {
    let profile = TestDatabase::new();
    let mut db = profile.open();
    let doc = document(&mut db, 1, "cobalt", None, false);
    let conv = db.create().unwrap().id;
    let id = db
        .begin_contextual_chat_attempt(conv, TurnAction::Send("Cobalt?".into()), None)
        .unwrap()
        .assistant
        .id;
    let sources = db
        .retrieve_for_chat(
            Some("cobalt"),
            &[doc],
            RetrievalMode::Keyword,
            BGE_KEY,
            None,
        )
        .unwrap()
        .sources;
    let mut stats = synthetic_replay_stats();
    stats.context_usage = Some(ContextUsage {
        sources: 1,
        memories: 0,
    });
    db.finish_chat_with_sources(
        id,
        "Cobalt [S1].",
        GenerationState::Complete,
        Some(&stats),
        &sources,
    )
    .unwrap();
    db.connection
        .execute_batch(
            "DROP TABLE web_research_settings; DROP TABLE model_downloads;
         ALTER TABLE personal_memories DROP COLUMN provenance; ALTER TABLE personal_memories DROP COLUMN classification;
         DROP TABLE provider_usage; DROP TABLE intelligence_settings; DROP TABLE providers;
         ALTER TABLE models DROP COLUMN artifact_key; DROP TABLE intelligence_receipts; ALTER TABLE messages DROP COLUMN classification;
         ALTER TABLE chat_source_references DROP COLUMN cited;
         ALTER TABLE chat_context_attempts DROP COLUMN metadata;
         DROP TABLE chat_route_coverage; ALTER TABLE models DROP COLUMN content_sha256;
         DROP TABLE model_runtime; ALTER TABLE models DROP COLUMN settings_mode;
         ALTER TABLE models DROP COLUMN content_key;
         PRAGMA user_version=8;",
        )
        .unwrap();
    drop(db);
    let db = profile.open();
    let saved = db.get(conv).unwrap();
    let context = saved.messages[1].context.as_ref().unwrap();
    assert!(context.coverage.is_none() && context.memory_enabled.is_none());
    assert_eq!(
        saved.messages[1]
            .stats
            .as_ref()
            .unwrap()
            .context_usage
            .unwrap()
            .sources,
        1
    );
    assert!(db.chat_supplied_evidence(id).unwrap()[0].cited);
    assert_eq!(
        db.resolve_chat_source(id, "S1").unwrap().as_deref(),
        Some("cobalt")
    );
}

#[test]
fn memory_count_paginates_at_capacity_and_uses_utf8_byte_limits() {
    let mut db = in_memory();
    assert!(db.create_memory(&"é".repeat(513)).is_err());
    assert!(db.create_memory("x\0y").is_err());
    let first = db.create_memory(&"é".repeat(512)).unwrap().id;
    for i in 1..512 {
        db.create_memory(&format!("Memory {i} – ÉCLAIR")).unwrap();
    }
    assert_eq!(
        db.create_memory("overflow").unwrap_err().code,
        ErrorCode::MemoryLimit
    );
    let page = db.list_memory_page("éclair", None, 50).unwrap();
    assert_eq!(page.total, 512);
    assert!(page.has_more);
    assert_eq!(page.items.len(), 50);
    let second = db
        .list_memory_page("ÉCLAIR", page.items.last().map(|row| row.id), 50)
        .unwrap();
    assert_eq!(second.items[0].id, page.items[49].id - 1);
    assert_eq!(db.list_memory_page("absent", None, 50).unwrap().total, 512);
    assert!(db.list_memory_page("", Some(first + 1), 50).unwrap().items[0].id == first);
    assert_eq!(db.clear_memories().unwrap(), 512);
    assert_eq!(db.list_memory_page("", None, 50).unwrap().total, 0);
}
