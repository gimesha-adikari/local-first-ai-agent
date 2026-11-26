# v0.2.0 Task 01 implementation report

The local `feature/knowledge-v0.2.0` branch starts at `5ec03bc778d07195e70338ca0ab8cf4d443bd6f5`, verified against remote `main` and the commit referenced by the annotated `v0.1.0` tag. Workspace and desktop package versions are now 0.2.0. The roadmap records public releases starting at v1.0.0. Existing local reports were preserved.

## Schema and recovery

Schema 6 adds `knowledge_documents` for metadata, SHA-256 fingerprints of the selected file bytes, and ready or partial indexing status. `knowledge_chunks` records ordered page or line references. `knowledge_fts` stores the text with matching row IDs and the FTS5 Unicode tokenizer. The storage adapter writes and deletes matching rows in one transaction with `trusted_schema=OFF` and FTS5 secure deletion enabled.

The migration adds these tables atomically and preserves the existing conversation, model, preference and Agent tables. On interruption, SQLite retains the last committed index. An interrupted replacement leaves its previous index intact unless the replacement transaction committed before shutdown. Import progress is session-local. Keep a pre-upgrade database backup if you need to return to the v0.1.0 executable, which accepts schemas only through version 5.

## Verification

| Check | Observed result |
| --- | --- |
| `cargo fmt --all --check` and `git diff --check` | Passed. |
| Locked release Clippy, workspace and all targets, `tauri/custom-protocol`, `-D warnings` | Passed. |
| Locked release Rust workspace tests with `tauri/custom-protocol` | 134 passed and 30 opt-in tests were skipped in the default run. The v5 migration fixture confirmed SQLite 3.54.1 with `ENABLE_FTS5=1`, saved history and settings, restart persistence, ranked queries, duplicate detection, replacement rollback, deletion, file and library limits, and blocked links or non-regular files. |
| Opt-in native PDF tests | Four passed, covering the shared sandbox, cancellation and reaping, malformed or unsupported PDFs, and accurate partial coverage past the Agent preview limit. |
| Frontend tests and production asset build | 41 tests passed. Type-check and Vite build passed. Vite emitted an asset-size warning. |
| Packaged WebView fixture under Xvfb | Passed through Tauri IPC and real GTK dialogs with synthetic TXT, Markdown and PDF files. Covered picker cancellation, retention refusal, a supplied IPC path requiring a picker, duplicates, replacement, source references, filtering, deletion and absence of Agent grants. |
| Standalone locked Linux release build and restart | Built and opened the application window under Xvfb. The isolated profile reopened with schema 6 and the replacement Markdown document. SQLite reported no foreign-key violations. |

The desktop build used the installed GCC system headers for bindgen. No model weights were needed. The [handoff](task-v0.2.0-01-handoff.md) documents IPC shapes, quotas and retention boundaries. The library UI and conversation integration remain future work. Search is chunk-local, and PDFs beyond the page limit may be partial. Historical conversation answers may retain prior quotations after library deletion.
