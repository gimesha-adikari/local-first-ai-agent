# v0.2.0 Task 05 integration report

**Verdict: PARTIAL / HOLD.** The release candidate is local to the working tree. See the acceptance matrix for the remaining gaps.

## Baseline and architectural findings

The branch was `feature/knowledge-v0.2.0` at `5ec03bc` with uncommitted Tasks 01–04. The starting frontend passed 56 tests and type-checking. The starting locked Rust suite passed 147 tests with 34 opt-in cases skipped. The roadmap places the first public release at v1.0.0. This task adds no dependency, model, device integration, or release action.

The schema 8 attempt table already blocked contextual continuation, but the flag was absent from IPC messages. The frontend relied on success-only generation stats and kept coverage in React state. Storage kept only passages with recognized model markers. The library refresh fetched list pages and then called aggregate embedding coverage once per document. Polling could overlap requests and repeatedly render the same job state.

The existing constraints were retained. Extraction and replacement use the native consent flow and SQLite transactions. Dense retrieval, embedding model changes, indexing, Chat and Agent generation share the existing desktop lease. Keyword search remains independent. Progress stays session-local, with a single non-overlapping poll per active job. Published indexes survive restart and speculative work is discarded.

## Implementation, migration and IPC

Schema 9 adds attempt metadata and a citation flag to the existing context tables. It preserves schema 8 citations and existing conversations, preferences, models, indices and memories. Older attempts have an explicit contextual marker and unknown historical coverage. The downgrade path is a pre-upgrade database backup.

An admitted contextual turn and its historical coverage are written together. Successful prompt decoding checkpoints the fitted counts and identities of the passages supplied. A failed fit retains coverage with fitted counts unavailable. A stream interrupted after prefill retains its evidence and recovers citations from its saved text. This metadata contains no copies of document excerpts or memory text. Preparation errors before turn admission leave no attempt. Enabled memory with no match and no document search follows ordinary Chat behavior. Regenerate reads the current settings and retains earlier attempts.

| Contract | Change |
| --- | --- |
| Assistant `Message.context` | Optional contextual-attempt object with `coverage`, `memoryEnabled`, and nullable `fitted` counts. Coverage carries effective mode, retrieved count, partial and absent documents, missing and incompatible dense indexes, and unsearched IDs. |
| `knowledge_library()` | One snapshot of up to 128 documents with `revision` and `embeddingState` (`ready`, `missing`, or `incompatible`). Ordinary document metadata also gains `revision`. |
| `chat_supplied_evidence({messageId})` | Fitted references, each with `cited` and current `available`. The existing `chat_source_references` command remains cited-only. |
| `chat_resolve_evidence({messageId,citationId})` | Plain text only when that passage belongs to the fitted prompt and its original revision still exists. The cited-only resolver keeps its previous contract. |
| `memory_page({query?,beforeId?,limit})` | `{items,total,hasMore}` in one response. The total is the entire saved collection independent of filtering. Existing memory commands remain available. |

The composer derives readiness from the snapshot. Search and excerpt reads discard stale responses. The source list labels uncited passages, keeps excerpts scrollable and restricts inline links to validated citations. The popover has a viewport-bounded height. Memory shows the total and uses the server's pagination result. The document revision already used for embedding and citation validity is exposed to the frontend. Replacing or deleting a document never rewrites historical coverage.

## Verification and release gates

The CPU was an Intel Core i5-8500 with six cores and 15,148 MiB RAM. Builds used two jobs and the local release cache. The app used CPU inference, local LFM2.5 and the existing supported BGE GGUF. The desktop and native runs used synthetic data in isolated XDG profiles and a Linux network namespace. The host restarted during verification, so temporary pre-restart captures and the saved startup comparison binary were lost. The worktree was retained.

| Gate | Observed result |
| --- | --- |
| Locked Rust workspace, release profile, Tauri custom protocol | **151 passed**, 0 failed, 35 ignored in the default run. |
| Opt-in native PDF checks | **4 passed**. Poppler and Bubblewrap exercised partial coverage, malformed and unsupported inputs, and parser cancellation and reaping. |
| Opt-in packaged desktop fixtures | **3 passed**. Contextual Chat with bundled LFM and BGE, Task 05 integration at 128 documents with the same local models, and existing unified Chat and Agent IPC compatibility. A supplemental native-picker run also passed cancellation during import reading. |
| Rust formatting, locked release Clippy (all targets, warnings denied), Git whitespace check | Passed. |
| Frontend test, type-check and production assets | **57 passed**. Type-check and Vite passed with the existing chunk-size warning. |
| Standalone Linux release | Built, opened under Xvfb with bundled LFM, and closed through the window protocol with exit 0. Reopened synthetic profiles had schema 9 and no foreign-key violations. |

### Citation reliability on bundled LFM2.5

The final 128-document fixture produced “The orchard gate code is [FERN-742].” and separately “FERN-742” with zero valid citation markers. The fitted PDF passage was available through the evidence resolver, marked **Uncited**, and displayable after restarting the app. The retained contextual fixture emitted valid [S1] markers and rejected the planted decoy in keyword, semantic paraphrase and hybrid samples. LFM still emitted citation-like text on a zero-hit answer. That answer had no resolvable evidence. Storage tests covered unknown and partially invalid citation groups.

Several alternative instructions improved formatting in isolated runs but made LFM accept or promote a planted decoy on another question. The shipped prompt retains the earlier evidence-grounding instruction and explicitly handles an empty evidence list. The reliability improvement is inspectable fitted evidence and strict marker validation, with no citation-repair inference. These checks are synthetic samples. A model can misattribute a claim even when it uses a valid identifier.

### Runtime acceptance matrix

| Area | Executed checks | Remaining coverage |
| --- | --- | --- |
| Native import and revisions | Standalone TXT, Markdown, PDF, 103-page partial PDF, native picker cancellation, unsupported-extension rejection and invalid UTF-8 rejection. A consented 3.9 MB PDF import cancelled during reading through the host IPC and left the library unchanged. Failed replacement preserved the prior semantic index. Successful replacement cleared that index, then deletion removed the replacement. Historical answers and coverage survived. | The standalone Cancel button during PDF extraction was not timed reliably. The native picker, consent, cancellation IPC, and terminal state were exercised together during reading. |
| Index, library and ranking | Real BGE selection and rejection of an invalid GGUF, index build, cancellation and prior-generation retention tests, restart persistence, abrupt termination at 2 of 159 passages with no new index after reopening, mixed coverage, all three retrieval modes and the full 128-ID selection. BGE unload retained saved indexes and dense search returned embedding_unavailable. | Extended repeated indexing and unloading memory profiling were outside the measured sample. |
| Chat and citations | Selected-only evidence, zero hits, partial coverage, bounded output, context-limit failure with durable metadata, valid citations and uncited evidence, excerpt loading, replacement/deletion invalidation, stop during preparation and generation, truncation and regeneration. Ordinary continuation passed the unified fixture. | Enter with an invalid selection preserved the draft. A real first send of 80,000 UTF-8 bytes was rejected, preserved the draft and removed its empty conversation. The standalone composer Stop timing while dense preparation was active was not reproduced. The packaged IPC run stopped that path before a turn was saved. |
| Personal memory | Native create, enable switch, count, restart, and clear-all confirmation. Real-model IPC covered cross-conversation recall and regeneration after editing and deleting a memory. Capacity, UTF-8 bounds, case-insensitive search, paging over 50 and clear-all were exercised. | The 512-item workload used IPC for repeatability. |
| App, resources and permissions | Light and dark themes, 440 px narrow layout, keyboard toggle and Escape/Enter popover navigation, switching to Memory and back during a 159-passage index build, offline startup and clean reopen, ordinary Chat, simulated Agent compatibility with real desktop IPC, semantic rejection while Chat held the lease, semantic rejection while a deterministic Agent task held the lease, independent keyword retrieval during that Agent task, and no added Agent grants. | The bundled LFM deliberately leaves Agent unavailable. The Agent contention check used the existing deterministic provider through real desktop IPC. Frame latency and long-run memory stability were not profiled. |

### Performance

One final packaged run used 128 synthetic documents, two compatible vector windows and a local BGE worker. Retrieval and refresh timings include Tauri IPC and use the WebView timer, rounded to the observed millisecond resolution.

| Measurement | Observed |
| --- | --- |
| Previous refresh pattern, replayed against the same fixture and compatible endpoints | 130 calls, median **16 ms**, range 13–124 ms across five runs. |
| New library snapshot | **1 call**, median **1 ms**, range 0–2 ms across five runs. |
| Keyword, semantic, hybrid retrieval | Medians **1 / 13 / 10 ms** respectively across nine calls per mode. Query texts differed by mode. |
| Index build and cancellation | Two one-window documents completed in **64 ms** including completion polling. A large archive index cancelled before publication. This sample is too small to extrapolate throughput. |
| Model load and Chat preparation | BGE **376 ms**, LFM **422 ms** at the fixture's 1,024-token context, and **2 ms** for keyword Send IPC to the Started event. |
| Standalone startup and application-process RSS | Window mapped in **182–226 ms** in the observed isolated launches. RSS was about **156 MiB** with no model, **1,365 MiB** with bundled LFM, and **1,439 MiB** after LFM and BGE settled in the earlier native run. In a later native run, BGE unload reduced process RSS by **63 MiB** at the 1.2-second sample. Measurements exclude WebKit child processes. |

There is no comparable pre-Task-05 startup or model RSS measurement. The old library comparison replays its exact IPC pattern within the same fixture. Time from window mapping to model readiness, repeated-cycle allocator drift and frame latency during indexing were not measured.

## Security and privacy

The native runs used an isolated namespace with loopback only and no traffic on that interface in the observed standalone snapshot. Document import continues to require the native picker and consent. Evidence commands return no local paths. The host uses quoted document data, and the frontend renders excerpts as plain text. Selection filters stay explicit, and semantic modes do not silently fall back. Import, retrieval and memory creation cannot grant Agent permissions. Historical references cannot resolve deleted or replaced indexed text. Saved answers can still contain quotations. The UI describes the local database as unencrypted. The captures use disposable synthetic data.

## Screenshots

- [Light library after restart, partial PDF and persisted index](screenshots/task-v0.2.0-05/01-library-partial-pdf.png)
- [Grounded answer with an expanded uncited passage](screenshots/task-v0.2.0-05/02-evidence-uncited.png)
- [Clear-all confirmation](screenshots/task-v0.2.0-05/03-memory-clear-confirmation.png)
- [Memory count and enabled setting in light mode](screenshots/task-v0.2.0-05/04-memory-light.png)
- [Knowledge popover in the 440 px window](screenshots/task-v0.2.0-05/05-knowledge-narrow-keyboard.png)
- [Unavailable original source with historical coverage after deletion and restart](screenshots/task-v0.2.0-05/06-evidence-unavailable.png)
- [A 159-passage semantic index build in progress](screenshots/task-v0.2.0-05/07-indexing-progress.png)
- [Blocked keyboard submission leaves the draft in place](screenshots/task-v0.2.0-05/08-keyboard-draft.png)

## Changed files

| Layer | Task 05 files |
| --- | --- |
| Core and inference | crates/agent-core/src/{chat,chat_context,inference}.rs, crates/agent-core/tests/chat.rs, crates/inference/src/llama.rs |
| Storage | crates/storage/migrations/009_attempt_evidence.sql, crates/storage/src/{chat_context,chat_context_tests,knowledge,lib,memory,semantic,tests}.rs |
| Desktop host and fixture | apps/desktop/src-tauri/build.rs, capabilities/main.json, src/{chat_context,commands,desktop_acceptance,knowledge,main}.rs, src/{task05_ipc_acceptance,unified_ipc_acceptance}.js within the desktop host |
| Frontend | apps/desktop/src/{App,ChatView,KnowledgeControl,KnowledgeView,MemoryView,Sources,ui}.tsx, {chat,knowledge,useKnowledge}.ts, knowledge.test.mjs and styles.css |
| Deliverables | This report and docs/screenshots/task-v0.2.0-05/ |

## Deviations and release readiness

The implementation retained bounded polling. After identifying a model regression, it retained the earlier grounding instructions for nonempty evidence. The host restart required a targeted Rust artifact rebuild. No dependency or runtime changes were needed.

**PARTIAL / HOLD.** The database, host contracts, native model behavior and principal UI flows have direct evidence. The standalone cancellation timing and extended resource conditions in the matrix remain release acceptance gaps. Work stopped at Task 05. No commit, push, merge, tag or release was made.
