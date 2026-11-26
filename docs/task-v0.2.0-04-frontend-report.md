# v0.2.0 Task 04 frontend report: knowledge and memory interface

Frontend only, on `feature/knowledge-v0.2.0`. No Rust, schema, capability, dependency or lockfile changes. Uses only the Task 01–03 IPC contracts. Nothing committed or pushed. Existing uncommitted work is preserved.

## Implementation

| Area | What changed | Files |
| --- | --- | --- |
| Shared logic | Typed contracts, limits, copy, readiness checks, coverage wording, citation parsing, memory validation and stale-response tokens. No React or Tauri imports. | `knowledge.ts` |
| Library state | Lives in `App`, so import and index jobs keep polling (250 ms) while you're elsewhere. Picks up jobs still running at startup. Each document's semantic index state comes from a single-document coverage query. A `revision` counter increments on import, replacement, deletion and reindexing. | `useKnowledge.ts` |
| Knowledge destination | Sidebar entry (full and collapsed). Native import and replacement, including the dialog and consent phases and cancelling while processing. Outcome notices, and a document list you can filter by name. Expandable metadata covers format, sizes, passages, PDF pages read and without text, keyword coverage and semantic index state. Keyword, semantic and hybrid search inside documents, with coverage notes. Choosing and unloading the embedding model, with a note that the choice lasts for this session while indexes are saved. Index progress and Cancel. Deletion asks for confirmation. Badges distinguish Keyword only, Semantic ready, Index outdated (incompatible) and Partly readable. | `KnowledgeView.tsx` |
| Composer control | A compact Knowledge pill in Chat mode, off by default. When on, you tick documents explicitly (1–128) and pick Keyword, Semantic or Hybrid. Per-document "Not indexed"/"Outdated index" badges appear for semantic and hybrid. Send and Regenerate are blocked while there's no selection, no loaded model or no indexed selected document. You get explicit fixes (Open Knowledge, Use Keyword instead); the mode never changes by itself. Deleted documents are dropped from the selection; nothing is added. The panel also shows whether Memory is on. | `KnowledgeControl.tsx`, `Composer.tsx`, `App.tsx` |
| Contextual Chat | Send and Regenerate pass the current `knowledge` option; Continue never does. While preparing, the thread shows the pending prompt with "Searching N selected documents…", and Stop is enabled. A `knowledge` event is held until `started` and then stored under that assistant message ID. Generation and conversation checks drop stale events. If preparation is cancelled or fails (`cancelled`, `embedding_*`), the draft is kept and a specific notice is shown. A conversation created for that first message is deleted again if nothing was saved. Edits made while preparing are kept. | `App.tsx`, `chat.ts`, `unified.ts`, `ConversationView.tsx` |
| Continuation | Answers whose `stats.contextUsage` is present offer Regenerate with an explanation and never call `chat_continuation_remaining`. Ordinary Chat keeps native Continue. The generic `invalid_config` copy now mentions answers that used documents or memory. | `App.tsx`, `chat.ts` |
| Sources | After a message completes, references are fetched when fitted `sources > 0`, and fetched again whenever `revision` changes. Only markers whose IDs were all returned (for example `[S1]`, `[S1, S2]`) become buttons. Unknown markers, code and link text stay plain. A source row shows the document name, page or line range, and an excerpt resolved on demand as plain text. Unavailable references are never resolved. Excerpts are dropped on every library change. Markdown keeps `skipHtml`, no images and no URLs. | `Sources.tsx`, `MarkdownMessage.tsx`, `ChatView.tsx` |
| Counts | Fitted counts appear as "Used 2 sources · 1 memory" (memory-only answers show just the memories). Live notes, during the session only, cover zero matches, matched-but-unfitted passages, partial documents, documents no longer available, and unindexed documents (not searched in semantic mode; word matches only in hybrid). | `ChatView.tsx`, `knowledge.ts` |
| Memory destination | Persistent retrieval switch, usable while memory loads or is off. Explicit creation and inline editing with a UTF‑8 byte counter (1–1,024). Case-insensitive search (≤256 bytes) with pages of 50 and Show more. Individual delete and Clear all both ask for confirmation. `memory_limit` copy covers the 512-memory cap. The view explains local unencrypted storage, retrieval across conversations, that nothing is saved automatically, and that earlier answers keep their quotations. No automatic extraction and no per-conversation controls. | `MemoryView.tsx` |
| Style | Reuses the graphite and warm-light tokens, callouts, segmented controls, dialogs and menus. No new dependencies. | `styles.css`, `icons.tsx`, `ui.tsx` |

## Verification (observed)

| Check | Result |
| --- | --- |
| `npm test` | 56 passed (41 existing, 15 new in `knowledge.test.mjs`). The new tests cover import and index lifecycle; document coverage states; request tokens and Chat event ownership; knowledge events not touching turns; the request option never widening the selection; pruning; readiness for each mode; coverage wording; citation splitting and rendering through the real safe Markdown pipeline; unavailable and null source resolution; memory byte, query and paging rules; contextual versus ordinary continuation. |
| `npm run typecheck`, `npm run build`, `git diff --check` | Passed. Vite printed its existing chunk-size warning. |
| Linux release build | `cargo build --release --locked -p local-first-ai-agent --features tauri/custom-protocol` passed with `BINDGEN_EXTRA_CLANG_ARGS` pointing at GCC's system headers and 3 jobs. Rebuilt once after the fixes below. |
| Packaged app | Run under Xvfb with `unshare --net` (no network), a temporary XDG profile, real GTK pickers and XTest input. The bundled LFM2.5‑1.2B Q4_K_M auto-loaded. BGE‑small‑en‑v1.5 F16 came from the existing local file. Synthetic TXT, Markdown, 538 KB TXT and the repository's PDF fixture were used. |

Observed in the app:

- Import and replacement worked through the native picker and the retention dialog. Cancelling the picker showed "Import cancelled. Nothing was added." Outcome notices appeared, the PDF showed "2 pages", and replacement dropped the index to Keyword only.
- Choosing BGE loaded it. Indexing one document finished and updated its badge and the totals. Indexing the 270-passage archive showed live progress ("13 of 270 passages"), and Cancel preserved the prior state.
- Semantic library search found the orchard passage from a paraphrase and reported "3 documents… weren't searched".
- Knowledge was off by default. With nothing selected, Send was blocked with an explanation. Semantic mode with one unindexed document showed the partial-coverage note. "Found 1 matching passage" appeared before tokens arrived, with Stop available.
- LFM answered from the document. One answer wrote `[FERN-742]`, which correctly stayed plain text with no stored reference. A cited answer linked `[S1]`, and its excerpt loaded on demand.
- After replacement, and later deletion, that source showed as Unavailable with the explanation and no excerpt. Regenerate was blocked while semantic mode had no index; after choosing "Use Keyword instead" it created a new attempt with a valid citation and kept the earlier attempt.
- A contextual reply cut off at 6 tokens offered Regenerate and the contextual explanation, without Continue. An ordinary reply showed "Continue generating · 3 left", and Continue appended to the same reply.
- Memory: retrieval switch, create, edit, case-insensitive search ("OOLONG"), cross-conversation recall ("Used 1 memory" after the fix), and individual delete and Clear all, both with confirmation.
- After relaunching on the same profile, the documents and Memory setting persisted. The embedding model was not loaded, as documented; it reloaded and indexed again. The last-row menu then opened upward. Light theme was reviewed live on Knowledge, the composer control, a hybrid cited answer with excerpt, and Memory.

Fixed after the first run, then confirmed in the second run or by tests: the last-row menu was clipped; plural wording ("1 passages", "1 of 3 documents have"); the memory-only label read "Used 0 sources"; the memory editor didn't take focus.

Screenshots in [`screenshots/task-v0.2.0-04/`](screenshots/task-v0.2.0-04/): indexing progress, semantic selection, excerpt, unavailable source, blocked regeneration, contextual and ordinary truncation, memory search and Clear all (dark), and Knowledge, hybrid selection, cited excerpt and Memory (light).

## Deviations and gaps

- The app was launched twice, not once. The second launch checked the fixes, restart persistence and the light theme.
- Not exercised at runtime: Stop during preparation (with these small documents, preparation finishes within milliseconds); the host's `embedding_*` preparation rejections, which the UI blocks before sending; removing the empty conversation after a failed first send; import failures and mid-processing import cancellation; memory paging past 50; narrow layouts. These paths are covered only by type-checking and the pure-logic tests; no component tests exist.
- The popover is anchored at its bottom, so its top moves when the readiness message appears or disappears. It works, but a click aimed from an earlier position can miss.
- GTK's location bar completion garbled long typed paths under XTest. That's a quirk of the test harness, not of the app.

## Backend limitations for Task 05

1. Per-document semantic coverage needs one `knowledge_embedding_coverage` call per document (up to 128 per refresh). A batched per-document result, or an index state in document metadata, would remove this.
2. Saved messages don't record whether an attempt used context. The UI infers it from `stats.contextUsage`. Failed attempts have no stats, so a failed contextual attempt can't be identified; Continue then falls back to the host's `invalid_config`.
3. Coverage events aren't persisted. After a reload, notes about partial, unindexed or unfitted evidence disappear and only the fitted counts remain.
4. There's no memory count or total. The UI can't show progress toward 512 and learns of the cap only through `memory_limit`.
5. Import and index jobs have no push events; progress requires polling.
6. Document metadata has no `revision`. Only `available` on references shows that a document was replaced.
7. Citation linking depends on the model writing `S#` markers. LFM2.5 often brackets values or phrases instead (`[FERN-742]`, `[blue shed]`), so many grounded answers have no linked sources. Stronger prompt formatting or a post-generation reference pass would help.
8. Semantic library search shares the CPU lease, so it returns `busy` while Chat or Agent is generating.

Task 05 not started.
