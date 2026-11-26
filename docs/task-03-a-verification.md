# Task 03-A backend verification

This work extends the existing persistent Chat and Agent workspace on
Linux. The next assignment owns document presentation and continuation
controls. Release approval remains withdrawn.

## Preflight

The starting checkout was `main` at `18d9bb8ac302c297acf0aaf671ea93631d8b4914`.
The porcelain status, tracked binary diff and untracked inventory were
recorded under `/tmp/agent-03a.XlTSEF/` in `status`, `start.patch` and
`untracked.nul`. Previously untracked paths remain present. The original
workspace and stylesheet were preserved. Preflight observed about 12 GiB
free on root and 8.5 GiB available RAM. The existing Qwen3.5-4B and
Qwen3-0.6B files were used in place.

## Findings and implementation

- **Saved Chat settings.** The app used 768 as its normal default and
  retained earlier model-specific limits such as 256. The native fitter
  reserved the entire requested output before deciding which history fit.
  Normal Chat now budgets against the actual tokenizer and rendered chat
  template, retaining recent history subject to a smaller output
  reservation. It exposes requested and effective output. Explicit
  Send and Regenerate overrides retain their existing strict behavior.
  The optional recommendation offers 8,192 context and up to 4,096 output,
  with four threads and Thinking off, and clamps for shorter trained
  context. No existing settings are rewritten.
- **Continuation.** The pinned `llama-cpp-2` 0.1.161 binding and
  llama.cpp revision `d81235049384534c167caea52b85a694f6103d14` accept
  native token prefill. The backend stores the bounded, exact sampled
  token sequence and any partial UTF-8 tail for truncated Chat attempts.
  On continuation it renders the same assistant generation prompt,
  checks a model/prompt fingerprint, prefills the already-generated
  tokens, and decodes the new suffix. It does not send a new user turn
  or resample the old response. Qwen3.5's template-provided opening
  Thinking marker is inserted into displayed output only on the first
  segment. The backend checks same-attempt ownership and enforces three
  continuation requests and 8,192 total sampled tokens. This is a fresh
  native context with replayed prefill, so latency increases with prefix
  length.
- **Persistence.** Schema 4 adds the Chat replay table and a nullable
  Agent omission count. Legacy Chat messages and journal records remain.
  Interrupted continuations recover as interrupted with their last
  checkpoint and cannot run automatically. Context failure before new
  output keeps the old truncated text and statistics. Deletion removes
  the replay row through its message foreign key.
- **Incorrect PDF search.** The offline pre-fix Qwen3.5 probe for “Find
  PDF files in the selected folder” produced
  `{"scope_id":"fixture_scope","name_contains":"pdf"}`. Validation
  accepted it with `extension: null` and the default maximum of 100.
  That proposal asks for a substring, so it can return `pdf-notes.zip`
  and other lookalikes. The native suffix comparison itself was correct.
  The model could then wrongly describe an unfiltered list as PDFs.
  Clarified tool descriptions, examples and observation guidance now
  distinguish basename matching and final-extension filtering. The
  identical post-fix probe produced `{"scope_id":"fixture_scope","extension":"pdf"}`.
  Native results record the applied filter and validate returned suffixes.
  The original manual execution did not retain its full decision
  payload. These observations are from the synthetic reproduction.
- **Agent metadata.** The task summary exposes its conversation and turn
  link. The desktop can list standalone history without opening up to
  thirty conversations. Unified turn summaries carry typed status and
  error, safe display copy, and durable omitted-context counts. Active
  execution exposes its observed start, time budget, current tool and
  exact pending approval. Historical rows keep null for information they
  never measured.
- **Markdown.** The installed parse pipeline interpreted a correct pipe
  table as a paragraph. The renderer has no GFM plugin; a malformed
  fixture without a separator also becomes a paragraph. The original
  manual answer text was unavailable for syntax inspection. The handoff
  describes the minimal renderer integration, with offline content
  controls retained.

## PDF dependencies and resource boundary

The native adapter uses the installed Poppler `pdfinfo` and
`pdftotext` utilities through fixed Rust subprocess arguments and
Bubblewrap. The installed Poppler package was
`26.01.0-2ubuntu0.3`, with GPL-2 or GPL-3 terms in its distribution
copyright file. Bubblewrap was `0.11.1-1ubuntu0.3` (LGPL-2-or-later),
and util-linux `2.41.3-3ubuntu2.2` supplied `prlimit`.
No new Cargo or npm dependency or bundled parser was added. Current
upstream development and malformed-PDF fixes are documented in the
[Poppler project](https://poppler.freedesktop.org/). Distributions must
maintain their system parser packages.

MIT-licensed lopdf was evaluated as a Rust alternative. Its current
manifest lists Rust 1.88, edition 2024, and compression and parsing
dependencies including flate2, brotli-decompressor, weezl and nom.
A Rust library in the application process would still require a hard
resource boundary for untrusted compressed input. This change uses the
already-installed process-isolated parser. [lopdf manifest](https://raw.githubusercontent.com/J-F-Liu/lopdf/main/Cargo.toml)

The existing read-only picker and `openat2` checks pin the authorized
regular file and block symlinks, path escapes, hard-link substitutions,
and changed inputs. Rust reads at most 8 MiB. The child receives
anonymous read-only bytes at a fixed sandbox path. Its mounts expose
the parser, runtime libraries, and font/Poppler data. It has no home,
workspace, or host network. Bubblewrap restricts namespaces, and seccomp
blocks process creation and sockets for the parser. Configured resource
ceilings are 256 MiB address space and two CPU seconds per parser, within
the owning tool's six-second wall deadline. The metadata pipe is
16 KiB, extraction output is 64 KiB, and diagnostics are 8 KiB.
There are at most 256 document pages and four pages per request.
Returned text has a combined 4,096 UTF-8-byte and wire-size ceiling.
The adapter tracks page coverage and clipping. [Bubblewrap isolation](https://github.com/containers/bubblewrap/blob/main/README.md)

PDF processing extracts text without executing JavaScript, links,
embedded attachments, or document actions. It returns typed errors for
encrypted, damaged, image-only, unsupported and over-limit inputs.
Missing dependencies, unsupported architectures or unavailable
isolation fail closed. This boundary reduces exposure to parser bugs.
It does not certify the parser or Linux kernel against vulnerabilities,
and namespace availability depends on the distribution. Raw file bytes
and native text are never written to the conversation database.
Saved model summaries may quote extracted content.

## Context performance

The same synthetic 572-token prompt and 128-token ceiling were used for
two sequential offline Qwen3.5-4B runs. Both used four CPU threads,
greedy sampling, and generated 102 tokens. Peak resident memory is
reported by `/usr/bin/time -v`.

| Configured context | Model load | Time to first token | Prompt tokens/s | Decode tokens/s | Generation elapsed | Peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 4,096 | 1.17 s | 15.62 s | 36.86 | 7.46 | 29.16 s | 3,972,168 KiB |
| 8,192 | 1.09 s | 14.74 s | 39.22 | 6.83 | 29.53 s | 4,104,828 KiB |

The larger context used about 129.55 MiB more resident memory with this
prompt. These are single observations on the test machine. Full-context
prefill at 8,192 tokens remains unmeasured. Previously loaded weights may
have been in the filesystem cache.

## Engineering checks

Cargo and CMake used one job pinned to one allowed CPU, `nice -n 15`,
idle-priority I/O, and offline Cargo resolution. Workspace and desktop
checks reused the release artifacts.

| Check | Observed result |
| --- | --- |
| Rust formatting | Passed |
| Locked release workspace Clippy with warnings denied | Passed |
| Rust release workspace tests | 110 passed, zero failed, 25 opt-in cases ignored in the ordinary run |
| Frontend tests | 29 passed |
| TypeScript | Passed |
| SQLite | Synthetic migrations from earlier schemas, interruption, deletion, integrity and foreign-key tests passed |
| Git whitespace validation | Passed |
| Vite and Linux Tauri release | Passed. Executable opened on private Xvfb with networking disabled |
| Fresh release database | Schema 4 and integrity checks passed in the isolated profile |
| Packaged WebView IPC | Passed. 21 scoped and 21 compatibility Agent events, linked metadata, same-ID Chat continuation, cancellation, and deletion |

## Chat acceptance

| Scenario | Evidence |
| --- | --- |
| Recommendation and legacy limits | Packaged IPC returned a 4,096/2,048 recommendation for a fixture whose trained context was 4,096. Its saved 768 remained unchanged. A one-token initial override remained explicit. |
| Longer direct output | Actual offline Qwen3.5-4B generated 1,024 tokens (4,417 UTF-8 bytes), with Thinking off, 8,192 context and four threads. It reached its explicit 1,024-token limit after 138.15 seconds at 7.50 decode tokens/s. |
| Continuation and prefix | Actual Qwen3-0.6B generated a 48-token prefix. Two continuations added 32 new tokens apiece, reaching 80 then 112. The old text stayed intact. No prefix was re-emitted. The later prefills contained 94 and 126 tokens. |
| Bounded requests | SQLite and IPC tests proved same assistant ID, no added user message, a three-request cap, and rejection of a stale attempt or another model. |
| Stop | The real-model continuation cancelled after one new token. A separate packaged IPC run saved the received suffix and the cancelled state in the original message. |
| Interruption and restart | A fresh connection recovered a checkpointed continuation as interrupted, with its Markdown prefix and suffix intact and no replay. |
| Context exhaustion | Qwen3-0.6B excluded 11 recent-history candidates in the long Chat probe, prefilled 832 tokens and reported a 768-token effective ceiling. Mandatory input beyond capacity emitted no output and returned `context_limit`. The packaged IPC fixture also confirmed preservation of the prior truncated reply on continuation preflight failure. |
| Reasoning and answer | Qwen3-0.6B preserved its existing Thinking opener across replay and emitted a closing delimiter followed by the correct answer. It generated 351 new tokens and ended normally. |
| Existing Chat regression | The offline `foundation_chat_prompts` opt-in passed with the original small GGUF. |

The first synthetic 900-token test prompt ended naturally on Qwen3-0.6B
before hitting its cap. The replay probe used a smaller forced
truncation so it could exercise two continuations. The first oversized
input fixture fit inside 2,048 context, so it was lengthened for the
mandatory-context rejection. The corrected opt-in tests passed.

## PDF and Agent acceptance

| Scenario | Evidence |
| --- | --- |
| Exact extension versus substring | Native mixed-folder tests covered lowercase and uppercase PDFs, real ZIP fixtures, TXT, names containing `pdf`, nesting and symlinks. The exact suffix filter excluded all lookalikes. |
| PDFs and page references | Actual isolated Poppler tests passed single- and multi-page extraction, exact page ranges, coverage and text clipping. |
| Rejected documents | Actual parser tests passed malformed, scanned/image-only, encrypted including default-empty-password, and oversized fixtures. |
| Authorization | Native tests passed unknown, revoked and expired grant rejection, symlink rejection and path-boundary controls. |
| Document prompt injection | The native/model preview tests treated injected instructions and control markers as data. File text did not grant capabilities. |
| Parser cancellation | An isolated native test detected the active parser supervisor, then verified stop and reaping. |
| TXT regression | Ordinary filesystem tool tests preserved existing pinned-scope and bounded UTF-8 reads. |
| Linked and standalone metadata | Synthetic migration and storage tests distinguish linked and unlinked records. Packaged IPC caught exact turn ownership, cancellation, saved status and context omission. |
| Migration, deletion and audit | Existing conversations and executions survived schema 3 to 4. The deletion and recovery tests passed integrity and foreign-key checks. |
| Existing approval behavior | Exact approval ownership, expiry and anti-replay regressions passed. This assignment used deterministic approval tests. |

### Real PDF conversation

The opt-in runtime test used the existing Qwen3.5-4B model with 4,096
context, four CPU threads, and greedy constrained Agent decisions. It used
the production permission engine, file adapters, bounded session context,
and synthetic SQLite conversation. Each prompt had its own execution
journal. Networking was disabled in a private namespace. The case does not
depend on the future document presentation UI.

| Prompt | Native evidence | Model answer and state | Decisions / retries | Seconds |
| --- | --- | --- | ---: | ---: |
| Find PDF files in the selected folder. | One scoped search, `extension: "pdf"`. Returned only `01-orchard.pdf` and `02-image.PDF`. The symlink was skipped and traversal marked incomplete. | Correct names and coverage warning. `completed`. | 2 / 0 | 104.34 |
| Read the first document from that search and summarize its text. Cite the page numbers. | One scoped `files.read` returned page 1 of 2. The native result marked partial coverage. | The model accurately cited page 1 and disclosed the missing coverage. Runtime `completed`; the full-document-summary acceptance condition failed. | 2 / 0 | 118.83 |
| What are the main points of that first document? Use the earlier extracted pages without rereading it. | Zero native dispatches. | Correctly recalled rainwater irrigation from page 1 and cited the source. `completed`. | 1 / 0 | 60.96 |

The three turns persisted in one conversation and the follow-up did not reopen
the file. The opt-in failed because the read requested only page one. The
tool description now directs the model to omit a page range unless the user
asks for pages and clarifies that a document ordinal selects a file. The final
formatting and Clippy checks passed after that adjustment. At the user's
request, verification stopped before the final workspace rebuild and real-model
replay completed. The workspace, packaged IPC, and Linux release results above
refer to the preceding build, which differs in that tool description and
synthetic proposal logging. Full coverage on the final wording is unverified.


## Changed-file inventory

| Area | Files |
| --- | --- |
| Chat budget, replay and tests | `crates/agent-core/src/chat.rs`, `crates/agent-core/src/inference.rs`, `crates/inference/src/llama.rs`, `crates/inference/tests/worker.rs`, `crates/storage/src/continuation.rs`, `crates/storage/src/lib.rs`, `crates/storage/src/tests.rs`, `crates/storage/migrations/004_chat_continuation.sql` |
| Native file tools and PDF fixtures | `crates/tools/src/filesystem.rs`, `crates/tools/src/lib.rs`, `crates/tools/src/pdf.rs`, `crates/tools/tests/filesystem_scopes.rs`, `crates/tools/tests/fixtures/conversation.pdf`, `empty-password.pdf`, `image-only.pdf`, `protected.pdf` in the same fixture directory |
| Agent tool protocol and tests | `crates/agent-core/src/agent/decision.rs`, `crates/agent-core/src/agent/error.rs`, `crates/agent-core/src/agent/mod.rs`, `crates/agent-core/src/agent/registry.rs`, `crates/agent-core/src/agent/runtime.rs`, `crates/agent-core/src/agent/store.rs`, `crates/agent-core/src/agent/tool.rs`, `crates/agent-core/tests/agent_decision.rs`, `crates/agent-core/tests/agent_registry.rs`, `crates/agent-core/tests/agent_runtime.rs` |
| Unified task storage | `crates/storage/migrations/004_agent_turn_status.sql`, `crates/storage/src/agent.rs`, `crates/storage/src/agent_tests.rs`, `crates/storage/src/conversation.rs` |
| Desktop contracts and tests | `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/capabilities/main.json`, `apps/desktop/src-tauri/src/agent.rs`, `apps/desktop/src-tauri/src/agent_contract.rs`, `apps/desktop/src-tauri/src/agent_conversation.rs`, `apps/desktop/src-tauri/src/agent_conversation_tests.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src-tauri/src/desktop_acceptance.rs`, `apps/desktop/src-tauri/src/main.rs`, `apps/desktop/src-tauri/src/unified_ipc_acceptance.js` |
| Frontend contract compatibility | `apps/desktop/src/App.tsx`, `apps/desktop/src/AgentView.tsx`, `apps/desktop/src/agentTypes.ts`, `apps/desktop/src/unified.ts`, `apps/desktop/src/unified.test.mjs`, `apps/desktop/src/useAgent.ts`, `apps/desktop/src/dev/fixtures.ts` |
| Reports | This report and the [backend handoff](task-03-a-backend-handoff.md) |

## Remaining limitations

- Task 03-B owns the document result view, continuation controls, the settings
  recommendation UI, and GFM table rendering. The workspace design remains in place.
- A model may still request a narrower PDF range than the user intended. Render
  native coverage independently of the assistant's interpretation. One call
  reads up to four pages and cannot establish the contents of omitted pages.
- The Linux parser relies on distribution-supplied Poppler and Bubblewrap and
  fails closed when its isolation is unavailable. The actual desktop test used
  a private Xvfb display and an isolated database. Distribution bundles were
  outside this verification.
- Task 03-A remains uncommitted and release approval remains withdrawn.
