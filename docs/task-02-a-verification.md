# Task 02-A persistent multi-turn Agent verification

Task 02-A extends the local backend for unified conversations. The redesigned
workspace still uses its existing Agent flow until Task 02-B adopts the new
commands. Release approval remains withdrawn.

## Starting state and architecture

The work began on `main` at `18d9bb8ac302c297acf0aaf671ea93631d8b4914` with
the Task 01-A, 01-B and 01-C changes already present. The starting porcelain
status, tracked binary diff, and NUL-delimited untracked inventory were
saved in `/tmp/agent-02a.6U1Hzn/` under `status`, `start.patch`, and
`untracked.nul`. The original Agent dashboard was already removed. The
worktree retains the conversation-first design and styles.

- Existing conversations and ordered messages remain the storage for user
  prompts and assistant attempts. The stable turn ID is its user message
  ID. Chat regeneration stays under that turn. One Agent user/assistant
  pair links to its independent task journal, timeout and usage.
- The start transaction writes the user message, pending assistant,
  execution and link before the worker can run. Terminal finalization
  updates the journal, assistant and conversation activity atomically.
  A crash or failed final write leaves the turn recoverable as interrupted.
  A pending native dispatch becomes uncertain, with no replay.
- Agent context reads at most twelve recent saved turns. The prior messages
  and session observations are JSON data with model control markers escaped.
  The llama.cpp path counts the real templated prompt with the loaded
  tokenizer, reserves the decision output, and omits older context as needed.
  The unified event channel reports omissions. The existing constrained
  JSON grammar and native budget and execution boundaries remain in use.
- The session cache can retain observations for up to eight result-bearing
  turns. Model context uses bounded previews. Earlier file results can
  identify the first result in native order while the associated scope is
  still current. Revocation and expiration remove their eligibility. Only
  current scopes appear in trusted scope hints. Historical launch notes
  omit application capability IDs. New launches still need an exact,
  single-use human approval.
- The scope-kind mismatch observed in Task 01-C now produces a specific
  typed denial and guidance. A selected file needs no relative path.
  A selected directory requires a path for reading. The host does not
  reinterpret incompatible arguments or expand a grant.

## SQLite migration and deletion

Schema 3 adds a nullable model basename to message attempts and
`conversation_agent_turns`, with foreign keys to the conversation,
same-conversation user and assistant rows, and the task journal. The
exclusive startup transaction applies the migration and recovery together.
Version 2 chat and Agent data keep their prior identities and content.
Earlier standalone tasks remain unlinked; the migration has no prompts
or answers from which to construct those conversations. Existing model
references and preferences stay in their prior tables. Backing up the
database before opening it with a new build is the rollback path if a
pre-migration executable is needed; the old executable rejects newer
schema versions.

Deleting a conversation in the current app transaction removes messages,
the Agent links, task rows, events, and executions. The host evicts its
session cache. Task-linked permission audit entries retain their sequence,
timestamps, action, tool and reason with task, scope and operation
references stripped. This preserves audit sequence continuity without
retaining conversation content. Standalone task history and independent
session audit entries stay separate. SQLite secure deletion is enabled
for the live database. Ordinary SQLite remains unencrypted, and this
operation makes no claim about backups or physical-media erasure.

Saved model answers can contain information quoted or summarized from a
selected file. Raw observations and file previews survive only in bounded
session memory. Restart creates new grants and approval state, so a
follow-up that needs unavailable source material must ask the user to
select it again. The earlier answer can still be used as saved text.
Tests used only new or synthetic SQLite profiles.

## Engineering checks

Native builds used one Cargo and CMake job on one allowed CPU with
`nice -n 15`, idle I/O priority, the existing release target, and Cargo
offline mode. Preflight observed about 12 GiB available on root. Both
existing GGUF files were used at their original paths.

| Check | Observed result |
| --- | --- |
| Rust formatting | Passed |
| Locked release workspace Clippy, warnings denied | Passed |
| Locked release workspace tests | 99 passed, 0 failed, 16 opt-in tests ignored in the ordinary run |
| SQLite migration and integrity | V2 migration, mixed turns, recovery, deletion, integrity and foreign keys passed |
| Frontend tests and TypeScript | 20 tests passed. TypeScript passed |
| Vite and Linux Tauri release | Passed. Started the 1120 × 780 main window on private Xvfb with networking disabled |
| Fresh release database | Schema 3, integrity check passed, no foreign-key violations |
| Packaged desktop IPC fixture | Passed in private, offline Xvfb. Three linked turns, 21 scoped events and 21 compatibility events. |

The first smoke harness matched a 10 × 10 GTK helper window too early.
The replay matched the actual main window and checked its isolated database.

## Deterministic acceptance

| Scenario | Observed evidence |
| --- | --- |
| Three consecutive Agent turns in one conversation | All three linked to one conversation with distinct message and task IDs. Their saved order and answers survived reopening. |
| Find, read the first file, and summarize | Actual scoped filesystem adapters found two synthetic files and read the first native match. A scripted provider produced the summary using bounded session context without another read. |
| Calculator follow-up | The native calculator returned 4. The next turn saw the retained result and replied without dispatch. |
| Device follow-up | See the real-model matrix below. |
| Chat to Agent and back | The store read five mixed turns in their original order. Chat saw the saved earlier Agent answer. Regenerate rejects an Agent user turn. |
| Conversation isolation | A second conversation received no earlier conversation messages or observations. |
| Revocation between turns | Prior raw preview and scope were omitted. A forced stale read was denied before native dispatch. |
| Expired grant between turns | An expired ID was omitted from trusted hints and a forced proposal was denied. |
| Cancellation of a follow-up | The provider observed the cancellation flag, task reached `cancelled`, and no tool ran. |
| Restart | Synthetic on-disk history reopened with prompts, replies and links. An unresolved dispatch recovered as uncertain, and a pending assistant recovered as interrupted. |
| Source unavailable after restart | The new session had no old scope or synthetic private marker. The scripted provider asked the user to select the file again. The marker was absent from SQLite conversation search. |
| Deletion and integrity | Conversation and linked task retrieval failed after deletion. Detached metadata-only audit remained. Integrity and foreign-key checks passed. |
| Long conversation | Fifteen saved turns produced a twelve-turn candidate window with three omitted. Saved messages remained intact. |
| Native result followed by synthesis failure | A native calculation succeeded once. A simulated model context error ended the task, and the next turn could reference the native value. |
| Approved launch and unrelated turn | A test-only simulated launch consumed one permission and recorded one dispatch. The next task saw the recorded dispatch without a capability ID, made no launch call, and received no approval. No application was opened by this deterministic test. |

## Changed files

| Area | Files |
| --- | --- |
| Persistent history and journaling | `crates/storage/migrations/003_conversation_agent_turns.sql`, `crates/storage/src/conversation.rs`, `crates/storage/src/lib.rs`, `crates/storage/src/agent.rs`, `crates/storage/src/agent_tests.rs` |
| Agent domain and context | `crates/agent-core/src/chat.rs`, `crates/agent-core/src/agent/decision.rs`, `crates/agent-core/src/agent/error.rs`, `crates/agent-core/src/agent/runtime.rs` |
| Desktop IPC and compatibility | `apps/desktop/src-tauri/src/agent.rs`, `apps/desktop/src-tauri/src/agent_conversation.rs`, `apps/desktop/src-tauri/src/agent_contract.rs`, `apps/desktop/src-tauri/src/agent_conversation_tests.rs`, `apps/desktop/src-tauri/src/desktop_acceptance.rs`, `apps/desktop/src-tauri/src/unified_ipc_acceptance.js`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src-tauri/src/main.rs`, `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/capabilities/main.json`, `apps/desktop/src/agent.ts` |
| Handoff and verification | This report and [the backend handoff](task-02-a-backend-handoff.md) |

## Offline Qwen3.5 acceptance

The opt-in integration ran the existing Qwen3.5-4B GGUF at 4,096 context
and four CPU threads with greedy, constrained decisions and Thinking
disabled. It used the same orchestration, context builder, local storage,
permission engine and native adapters as the desktop backend. A fresh
network namespace had loopback down and no default route. The SQLite
profile and file fixtures were synthetic. This run exercised the backend
independently of Task 02-B's pending conversation view.

| Prompt | Native operation | Observed answer and task state | Model calls / retries / excluded turns | Seconds |
| --- | --- | --- | --- | ---: |
| Find .txt files whose names contain ledger in the selected folder. Report the names in native order. | One successful search | Both fixture names in order. `completed`. | 2 / 0 / 0 | 91.80 |
| Read the first matching file from your previous search. What does it say? | One successful read of the first fixture | Chose `alpha_ledger.txt` and reported Juniper Orchard's rainwater irrigation. `completed`. | 2 / 0 / 0 | 100.73 |
| Summarize the main point of that file in one sentence without rereading it. | Zero dispatches | Correct one-sentence summary. `completed`. | 1 / 0 / 0 | 48.19 |
| Calculate 137 times 89 with the calculator. | One successful calculator call | `12193`. `completed`. | 2 / 0 / 0 | 93.74 |
| What number did the calculator just return? Use the previous result. | Zero dispatches | Correctly recalled `12193`. `completed`. | 1 / 0 / 0 | 44.95 |
| Inspect this device with the system tool. Which operating system runs on it? | One successful device information call | Correctly reported Linux. `completed`. | 2 / 0 / 0 | 96.98 |
| Which operating system did that tool report just now? | Zero dispatches | Correctly recalled Linux. `completed`. | 1 / 0 / 0 | 53.10 |

All seven turns persisted in the same conversation, with four successful
native dispatches across eleven model decisions and zero retries. The
model loaded in 15.03 seconds. The opt-in test ended successfully in
544.89 seconds, including model load and fixture setup.

## Other opt-in results

- The Qwen3-0.6B Chat regression passed. It exercised greetings, poem and
  Rust output, a technical explanation, multi-turn recall, a tiny Thinking
  budget, and cancellation. This verifies the existing regression
  assertions. It does not rate the prose or code beyond those assertions.
- A separate Qwen3-0.6B Agent probe used a 2,048-token context. The real
  tokenizer excluded all 12 deliberately oversized prior turns and fit
  the latest decision at 1,046 prompt tokens with 128 reserved. Mandatory
  context that exceeded capacity returned `ContextLimit` before dispatch.
- The packaged WebView test used scripted decisions and the actual Tauri
  IPC, policy, calculator and database. It received scoped observations and
  the unchanged compatibility events for the same tasks. It verified that
  a different task ID could not cancel the active execution, cancellation
  finalized the correct turn, and deletion removed the conversation from
  the list. The fixture runs only from the opt-in test executable.

## Remaining limitations

- The existing desktop composer still calls the standalone Agent command.
  Task 02-B is responsible for switching it to the unified API, rendering
  mixed turns in one view, and displaying saved-history and privacy cues.
  The packaged IPC fixture exercises the new API without changing that UI.
- Restart and permission cases used synthetic databases and deterministic
  providers. The application-launch replay check recorded a simulated
  dispatch, with no external application opened in this assignment.
- The context includes bounded previews and a recent suffix. An old file
  may need fresh selection, and a model can still misunderstand a reference
  or make an incorrect prose claim. Only execution records establish that a
  native operation ran. Backend scope and approval checks apply even if the
  model cites old data or proposes the wrong operation.
- The Xvfb smoke checked the Linux executable and the isolated schema.
  Distribution bundles and the user's private database were outside the run.

The [backend handoff](task-02-a-backend-handoff.md) contains exact request,
response and event shapes for Task 02-B. The work remains local and
uncommitted.
