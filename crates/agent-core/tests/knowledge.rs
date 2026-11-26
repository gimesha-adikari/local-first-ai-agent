use agent_core::{
    agent::{AgentError, AgentErrorCode},
    knowledge::{SourceReference, append_chunks},
};

#[test]
fn chunking_keeps_utf8_and_source_lines_across_long_lines() {
    let line = "é".repeat(1200);
    let text = format!("{line}\r\nsecond line\n");
    let mut chunks = Vec::new();
    append_chunks(&mut chunks, &text, None, || Ok(())).unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text.len(), 2048);
    assert_eq!(
        chunks[0].source,
        SourceReference::Lines {
            start_line: 1,
            end_line: 1,
        }
    );
    assert_eq!(
        chunks[1].source,
        SourceReference::Lines {
            start_line: 1,
            end_line: 2,
        }
    );
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>(),
        text
    );
}

#[test]
fn page_chunks_keep_the_real_page_and_stop_when_cancelled() {
    let mut chunks = Vec::new();
    let result = append_chunks(&mut chunks, "page words", Some(18), || {
        Err(AgentError::new(
            AgentErrorCode::Cancelled,
            "test cancellation",
        ))
    });
    assert_eq!(result.unwrap_err().code, AgentErrorCode::Cancelled);
    assert!(chunks.is_empty());
    append_chunks(&mut chunks, "page words", Some(18), || Ok(())).unwrap();
    assert_eq!(chunks[0].source, SourceReference::Page { page: 18 });
}
