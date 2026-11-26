# v0.2.0 Task 03 report

Task 03 adds host-driven knowledge retrieval to ordinary Chat and explicit personal memory. Knowledge is opt-in per attempt and memory retrieval is off by default. Both paths supply data to the existing Chat inference worker. They do not invoke Agent tools or grant permissions.

## Storage and behavior

Schema 8 adds `chat_context_attempts`, `chat_source_references`, `memory_settings` and `personal_memories` in one migration transaction. Existing messages, Agent records, keyword indexes and embedding generations are preserved. A pre-migration database backup is the downgrade path.

Retrieval is restricted to the selected documents and uses the requested BM25, semantic or hybrid rank. The host reports missing and incompatible dense indexes, partial PDF or embedding coverage, unavailable selections and zero hits. Up to four whole chunks and three relevant memories enter tokenizer-based fitting. Optional memory and lower-ranked chunks are removed first if they would cut into the output reservation. The host puts grounding rules in the system message and quotes evidence and memory as data in the user message. Template control delimiters in those data fields are escaped.

Citations are committed with the assistant response and link only to source IDs emitted in the answer and present in the fitted prompt. Each reference keeps the original document ID, revision, chunk ordinal and page or line range. The resolver cannot open a file or retarget to a replacement. Source deletion, replacement or memory deletion can leave quotations in previously saved answers.

Contextual attempts have no continuation seed. Regenerate applies current retrieval settings and creates a new assistant response. Ordinary Chat keeps native continuation. Saved contextual answers are excluded from later inference history. Personal memories can be created, searched, edited, deleted and cleared through the main-window IPC. Retrieval ranks lexical overlap across conversations and keeps memories separate from documents and history.

See the [frontend handoff](task-v0.2.0-03-handoff.md) for command arguments, result types and UI states.

## Verification

- Locked release workspace suite with `tauri/custom-protocol` passed 147 tests. The default run skipped 34 opt-in tests. The storage tests exercised a populated schema 7 migration, selected-document isolation, missing and partial indexes, citation validation, replacement and deletion, stale continuation rejection, and persistent memory management.
- Rust formatting, `git diff --check`, and locked release Clippy with all workspace targets and warnings denied passed. The frontend type-check, asset build, and 41 tests passed. Vite reported its existing asset-size warning.
- The model-backed WebView fixture uses temporary profiles and an isolated Linux network namespace. It runs with the existing LFM2.5-1.2B-Instruct Q4_K_M, Qwen3.5-4B UD-Q4_K_XL and verified local BGE-small-en-v1.5 GGUFs. Both chat models passed keyword, semantic and hybrid Chat, including selected-document isolation, a semantic paraphrase, insufficient evidence, missing and partial indexes, cancellation, stale continuation rejection and cross-conversation memory. Both returned the correct code on the planted-instruction paraphrase. Each preserved a 600-token reservation by dropping an oversized chunk from a 1,024-token context. No model downloads were used.
- The unified IPC compatibility fixture passed, including ordinary Chat continuation and Agent routing. The standalone Linux release opened under Xvfb with a fresh schema 8 database, memory retrieval disabled and no foreign-key violations.

## Limits

Memory matching uses lexical overlap and can miss paraphrases. Citation validation establishes source identity and prompt inclusion. The models can still misstate what a source supports or emit citation-like text. Qwen mentioned that a document contained an instruction in an insufficient-evidence answer, without adopting the instruction. LFM emitted a citation-like label on the zero-hit case, and SQLite contained no reference for that answer. Failed or crash-interrupted streams can retain checkpointed prose without verified citations.
