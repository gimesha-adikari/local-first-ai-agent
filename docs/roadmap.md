# Milestones

| Version | Scope | Status |
| --- | --- | --- |
| v0.0.1 | Cargo workspace, shared core, Tauri desktop, React, and CI | Complete |
| v0.0.2 | Offline GGUF inference, streaming, Thinking, and CPU benchmarks | Complete |
| v0.0.3 | Persistent SQLite Chat, multi-turn context budgeting, and offline desktop acceptance | Complete |
| v0.1.0 | Linux CPU Chat and bounded Agent, scoped TXT and PDF access, per-launch approval, local task and audit history, and optional bundled LFM2.5 | Accepted, merged, and tagged. |
| v0.2.0 | Local knowledge and memory: explicit document import, SQLite full-text and optional local semantic search, grounded Chat with cited sources, personal memory, and categorized Settings | Implementation accepted; finalization in progress. |
| v1.0.0 | First public release | Planned |

## v0.2.0 scope

- Import one TXT, Markdown or PDF file at a time after native retention
  consent. The library holds up to 128 documents and 32 MiB of retained text.
- Keyword search uses SQLite FTS5. Optional semantic and hybrid search use
  the local BGE-small-en-v1.5 model, which the user imports and loads.
- Knowledge-grounded Chat supports exact search, overviews and questions.
  Answers show fitted sources and memories. Ordinary Chat without evidence
  stays ordinary Chat, except for document questions, which receive a fixed
  no-evidence reply.
- Personal memory is opt-in, stores up to 512 explicitly saved notes, and
  supplies up to three relevant memories as background.
- Settings are categorized (General, Chat & Agent, Models, Knowledge &
  Memory, Privacy & Storage, About & Licenses).
- Schema upgrades run through version 11. A pre-upgrade database backup is
  required before running the v0.1.0 executable on the upgraded database.

See the [architecture](architecture.md#local-knowledge) and the
[known limitations](../README.md#known-limitations) for the boundaries.

## Future development backlog

These items are not part of v0.2.0. Each needs its own task and review.

- **Direct file selection inside Chat.** Let a user attach or select a
  document from the Chat composer without opening Knowledge first.
- English-only routing cues and stemming. Add multilingual routing and an
  FTS stemmer or a reviewed alternative.
- Citation reliability for LFM2.5, which often brackets values instead of
  writing `S#` markers. Consider stronger prompt formatting or a
  post-generation reference pass.
- Persist live coverage notes and push progress events for import and index
  jobs, replacing polling.
- Give semantic search its own execution path so it does not return `busy`
  while Chat or Agent is generating.
- Long-term memory profiling. The v0.2.0 ten-cycle run did not establish a
  plateau for application RSS after unload.

Public releases are deferred until v1.0.0. Android, device networking, and
GPU offloading remain deferred.

## Earlier release notes

See the [v0.1.0 release notes](release-notes-v0.1.0.md) for supported features
and known limitations. The v0.1.0 engineering acceptance includes the
Qwen3.5-4B exact-file correction. Apache-2.0 is approved for the original
application source, and the replacement bundle carries the software and model
notices. [Distribution notice verification](task-07-a-distribution-notices.md)
