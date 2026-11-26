# Task 02-B frontend report: continuous conversation interface

The desktop now runs Chat and Agent turns inside one persistent conversation,
using the Task 02-A unified backend. The graphite dark theme, warm light
theme, sidebar, composer and details drawer from Task 01-B are unchanged in
design. No Rust, SQLite migration, security policy, Tauri capability or
dependency file was edited. Release approval remains withdrawn. This report
does not declare release readiness, and Phase 03 was not started.

## Starting state

Work began on `main` at `18d9bb8` with the Task 01-A, 01-B, 01-C and 02-A
changes uncommitted. Before editing, the following were saved in the session
scratchpad (`…/scratchpad/t02b/`):

- the porcelain status (84 paths);
- the tracked diff (12,500 lines);
- the untracked inventory (89 paths);
- a tarball of `apps/desktop/src`.

At the end, the tracked diff of every path outside the frontend still matched
the starting diff byte for byte. Only `App.tsx`, `Dialogs.tsx`, `main.tsx`,
`styles.css` and `tsconfig.json` changed among tracked files. The 48 dirty
backend and dependency paths are untouched.

## Implemented functionality

- **One conversation, many turns.** New thread opens an empty workspace. The
  first submitted message creates the conversation, whether it's Chat
  (`create_conversation` then `send_message`) or Agent (`create_conversation`
  then `start_agent_turn`). Every later Chat or Agent request stays in that
  conversation. The Chat/Agent selector only sets the mode of the next turn.
- **One sidebar entry per conversation.** Recent lists `list_conversations`
  summaries only. Agent turns never create sidebar threads. A conversation
  with a running turn shows a spinner and "Agent running". Rename, delete,
  title and timestamp updates, and backend search across saved Chat and Agent
  text all work.
- **Ordered turn rendering.** `ConversationView` renders the
  `UnifiedTurn[]` from `get_unified_conversation` in order:
  - **Chat turns** show the user message and their assistant attempts. Earlier
    attempts collapse into "Previous attempt". Reasoning, notices, copy and
    the model name are kept.
  - **Agent turns** each have their own activity, native results, approval,
    answer, errors and footer.
- **Per-turn ownership.** Live state is keyed by turn ID and reconciled with
  that turn's journal (`get_agent_task`). Native results, steps and errors
  never cross turns or conversations.
- **Saved and transient information are labelled differently.**
  - Native results appear under "Native results · Returned by local tools,
    not written by the model".
  - The model's text is "Model answer · Interpretation of the native results ·
    Saved in this conversation", or "No tools were run. This is the model's
    own reply, not a verified result."
  - When `nativeResultsAvailable` is false and the journal shows a succeeded
    step, the thread says "Native result details are no longer available" and
    explains that raw results are kept only for recent turns of the current
    session. Nothing is fabricated.
- **Regeneration stays with its Chat turn.** It is offered only when the
  newest turn is a Chat turn. A regenerated attempt appends to that turn.
  Agent turns have no regenerate control, and nothing re-runs native
  operations.
- **Continuous Agent flow.** The composer accepts the next request as soon as
  a turn ends. The placeholder changes to "Ask the agent a follow-up…" or
  "Reply…" inside a thread.
- **Distinct progress states.**
  - Model processing: spinner with the current activity.
  - Native execution: the active step label, for example "Searching selected
    files".
  - Waiting for approval: an amber clock and "Waiting for your approval".
  - Elapsed and remaining time are shown, and the low-level event log stays
    behind the activity chevron.
- **Navigation during active work.**
  - Other conversations stay readable and fully isolated.
  - A dock callout offers "Return to “…”" and "Cancel turn" from anywhere, and
    Stop in the composer cancels the active turn too.
  - The status line names the conversation that's busy.
- **Approvals.**
  - The inline card shows the exact trusted target and the live operation ID
    is used to respond.
  - Approval, denial, expiry and cancellation have distinct copy and
    outcomes.
  - While an approval is pending, the composer's mode, model, + and options
    menus close and are disabled, so nothing covers Approve or Deny.
  - The card scrolls itself into view, including at 1024 × 768.
- **Permissions.**
  - Grant chips, the file and folder pickers, revocation and expiry refresh
    are unchanged. Revocation refreshes the open thread, because results tied
    to a revoked resource become unavailable.
  - `scope_kind_mismatch` guidance names the still-selected file and suggests
    reading it directly, or selecting a folder to search. It never asks the
    user to re-select a resource that is already granted.
  - A permission denial while grants exist notes that earlier messages never
    grant access.
- **Context omission.** The live `excludedTurns` value is shown on its turn:
  "N earlier turns were left out of the model's input to fit its context.
  They remain saved in this conversation."
- **Legacy standalone history.** Journal records that don't belong to a known
  conversation appear in a collapsed "Execution records" section, apart from
  Recent. The record view states that standalone records never stored a
  request, answer or native result. The normal composer never calls
  `start_agent_task`.
- **Privacy disclosures.**
  - Settings › Privacy and storage covers:
    - local storage of prompts and answers;
    - unencrypted SQLite;
    - answers that can contain file information;
    - raw native results not saved;
    - session-only grants not restored after restart;
    - deletion scope, remaining content-free audit entries, and unchanged
      backups.
  - The delete dialog and the File access dialog repeat the relevant points.
  - No physical-media erasure or backup deletion is claimed.

### Fixes found during acceptance

- **Dialog initial focus.** React's `autoFocus` renders no attribute, so the
  Task 01-B dialog moved focus off the rename field, and the real-app rename
  failed silently. Fields now opt in with `data-autofocus`. Rename was
  re-verified in the real app.
- **Scroll on open.** Opening a conversation left the scroll at the top. A
  `ResizeObserver` now keeps a pinned thread at the newest turn while journal
  details load.
- **Recovered timing.** A recovered turn's elapsed time now uses the journal
  start time, and its remaining time uses the turn's saved timeout.
- **Copy for continuous threads.** Expired-approval and model-failure guidance
  now suggest a follow-up turn instead of "start a new task".
- **Mismatch outcome label.** A scope mismatch is labelled "Not run: the
  request didn't match the selection", not as a generic denial.

## Unified conversation architecture

```
Composer ──(mode = next turn's kind)──► App.submit()
   │ chat  → ensureConversation → send_message / regenerate (Channel<ChatEvent>)
   │           └─ applyChatEventToTurns() streams into the right Chat turn
   │ agent → ensureConversation → useAgent.start(conversationId, prompt, timeout)
   │           └─ start_agent_turn → AgentTurnIdentity {conversationId, turnId, taskId}
   ▼
useAgent (single controller, one listener: "agent-turn-event")
   • buffers events that arrive before start_agent_turn returns
   • ownsEvent(): conversation + turn + task must all match the active owner
   • applyTurnEvent(): merges observations by operationId, keeps max excludedTurns
   • terminal finished/failed → settle → refresh saved conversation + journal
   • remount: get_active_agent_turn → recover owner, approval, timing; reconcile with get_agent_task
   • cancel / respond use the exact active identity (and approval operationId)
   ▼
ConversationView ── for each UnifiedTurn ──► ChatTurnView | AgentTurnView
                     agentTurnDisplay(turn, liveTurn, journalDetail, {running, loadedAt})
```

- **Pure, tested core.** `unified.ts` holds the logic with no React or Tauri
  imports: identity matching, the turn event reducer, chat-stream merging,
  regeneration eligibility, per-turn display composition, scope-aware
  guidance and omission copy.
- **Availability after reload.** `agentTurnDisplay` treats the backend's
  `nativeResultsAvailable` as authoritative for any turn that ended before the
  conversation snapshot was read. Session memory therefore never shows
  results the backend has evicted or tied to a revoked grant.
- **Decimal-string IDs.** Turn and task IDs stay strings end to end, including
  values above `Number.MAX_SAFE_INTEGER` (tested).

## Backend contract integration

| Command / event | Use |
| --- | --- |
| `create_conversation` | First message of a new thread (Chat or Agent). An empty conversation created for an Agent turn that fails to start is deleted again |
| `list_conversations { query }` | Recent list and debounced search over saved Chat and Agent text |
| `get_unified_conversation { id }` | Thread rendering. Refreshed after Chat completion, Agent terminal events, rename, instructions and revocation. Also read in the background for up to 30 recent conversations to learn linked task IDs (only IDs kept) |
| `start_agent_turn { conversationId, prompt, taskTimeoutSeconds }` | Every Agent submission. Subscribed before invoking. Early events buffered and matched to the returned identity |
| `agent-turn-event` | The only Agent event channel. The compatibility `agent-event` is never subscribed (a fixture emits both; only one set renders) |
| `get_active_agent_turn` | Remount recovery of owner, approval target, operation ID and expiry |
| `get_agent_task { taskId }` | Journal state, executions, usage, audit and available session results per Agent turn (latest 24 per open thread). Legacy records |
| `cancel_agent_task { taskId }` | Exact active task, from any view. A `false` reply reconciles from the journal and accepts that turn's late terminal event once |
| `respond_agent_approval { taskId, operationId, approve }` | Exact active task and operation ID. Approve is disabled when the target is missing or the approval has expired |
| `rename_conversation`, `set_system_prompt`, `delete_conversation` | Header menu and drawer. Deletion drops that conversation's session state and linked IDs |
| `send_message`, `regenerate` | Same conversation ID across mode switches. Regenerate only when the newest turn is Chat |
| `list_agent_tasks` | Read-only, for legacy execution records |
| `start_agent_task` | Not called by the UI |

## Changed-file inventory

All under `apps/desktop/`.

| File | Change |
| --- | --- |
| `src/unified.ts` | New. Unified types, ownership, turn reducer, chat merge, display composition, guidance |
| `src/unified.test.mjs` | New. 7 regression tests |
| `src/ConversationView.tsx` | New. Ordered turn renderer |
| `src/useAgent.ts` | Rewritten as the unified controller (`agent-turn-event`, `start_agent_turn`, recovery, exact-identity cancel and approval) |
| `src/AgentView.tsx` | Per-turn `AgentTurnView` (activity, saved activity, approval, answer, unavailable-results notice, guidance) and `LegacyRecord` |
| `src/App.tsx` | Unified conversation state, chat streaming into turns, Agent turn start, navigation, return and cancel control, drawer turn selection, linked-task scan, scroll following |
| `src/ChatView.tsx` | `ChatTurnView` with attempts and model attribution. Unused flat renderer removed |
| `src/Sidebar.tsx` | Conversations-only Recent. Separate collapsible Execution records |
| `src/DetailsDrawer.tsx` | Unified conversation facts and the selected Agent turn's task details |
| `src/Composer.tsx` | `lockMenus` closes and disables menus during approval |
| `src/Dialogs.tsx` | Privacy and delete disclosures, access copy, `data-autofocus` fields |
| `src/ui.tsx` | Dialog initial-focus fix |
| `src/agent.ts` | Mismatch outcome label, follow-up wording in guidance |
| `src/agent.test.mjs` | One new test. One assertion updated for the new expiry copy |
| `src/styles.css` | Turn label, context note, waiting state, records section, privacy list, sidebar scroll |
| `src/main.tsx` | Awaits the dev-only fixture installer |
| `src/dev/fixtures.ts` | **Development only, synthetic.** Rewritten for the unified IPC (keyword-driven turns, early events, compatibility events, cache eviction, recovery, expiry, mismatch, omission, restart). Excluded from production bundles (grep-verified) |
| `tsconfig.json` | `allowImportingTsExtensions` so `unified.ts` can import `agent.ts` under both `tsc` and Node's test runner |
| `docs/task-02-b-frontend-report.md`, `docs/screenshots/task-02-b/` | This report and its evidence |

## Test results

| Check | Result |
| --- | --- |
| `npm test` | **28 passed**, 0 failed (20 at start, 8 new) |
| `npm run typecheck` | Passed |
| `npm run build` (Vite production) | Passed: 43.4 kB CSS, 453.4 kB JS (137.6 kB gzip) |
| Fixture exclusion | No fixture symbols in `dist/assets` |
| `git diff --check` | Passed |
| Linux release build | `cargo build --release --locked -p local-first-ai-agent --features tauri/custom-protocol -j 1` under `nice -n 15 ionice -c3`, offline, single CMake job: passed, about 64 s, three times |
| Acceptance database (closed) | Schema 3, `integrity_check` ok, 0 foreign-key violations |

New tests in `unified.test.mjs`:
- identity matching across conversation, turn and task, with unsafe-integer
  IDs;
- a turn ignoring another conversation's events, deduplicating observations
  and keeping the largest omission;
- native results retained after a `model_context_limit` failure;
- Chat streaming that never touches Agent turns, regeneration that appends
  under the latest Chat turn and is refused after an Agent turn, and
  string task IDs;
- after reload, unavailable payloads are hidden rather than invented, and
  saved answers and interrupted turns render;
- mismatch guidance that names the granted file, never asks to re-select it
  and never leaks grant IDs;
- omission copy.

The new test in `agent.test.mjs` covers mismatch, declined and expired
outcome labels.

## Acceptance

Real results come from the rebuilt release binary. In a user and network
namespace (`unshare -rn`, loopback down, no route, recorded in the run), it
ran with its own Xvfb display, its own D-Bus session and a temporary XDG
profile. It used the existing local **Qwen3.5-4B UD-Q4_K_XL** at a 4,096
context, 4 CPU threads, Thinking off and the default 300 s turn limit. The
synthetic files were `alpha_ledger.txt`, `budget_ledger.txt`, `notes.txt` and
an empty `archive/` folder, granted through the native folder picker. The UI
was driven through AT-SPI accessible names, with XTest and xwd from a driver
inside the namespace. Synthetic results come from the dev fixture in WebKitGTK
2.52 (the Linux Tauri engine) and are labelled **SYNTHETIC**.

### Real multi-turn conversations (Qwen3.5-4B)

| # | Conversation · prompt | Native work | Answer | Model calls / tools / retries | Seconds |
| --- | --- | --- | --- | --- | ---: |
| 1 | A · Find .txt files whose names contain ledger in the selected folder. Report the names in native order. | `files.search` succeeded: `alpha_ledger.txt`, `budget_ledger.txt` | Both names in native order. Correct | 2 / 1 / 0 | 104.1 |
| 2 | A · Read the first matching file from your previous search. What does it say? (navigated to a new thread and back while running) | `files.read` of `alpha_ledger.txt` succeeded | Quoted the file correctly | 2 / 1 / 0 | 125.2 |
| 3 | A · Summarize the main point of that file in one sentence without rereading it. | None | Correct one-sentence summary | 1 / 0 / 0 | 60.1 |
| 4 | A · (Chat) Answer in one short sentence: what does the orchard use the collected rainwater for? | None (Chat) | "…to irrigate the young apple trees." Used saved Agent context | — | 11.3 |
| 5 | B · Calculate 137 times 89 with the calculator. | `math.calculate` returned 12,193 | 12193 | 1 tool (journal deleted later) | 85.2 |
| 6 | B · What number did the calculator just return? Use the previous result. | None | "The calculator returned 12193." | No tool | 44.6 |
| 7 | B · Without using any tools: what did the orchard ledger file say earlier in this conversation? … | None | Said this conversation has no such information. **Isolation held** | No tool | 53.8 |
| 8 | B · Write a detailed description of a fictional lighthouse … Do not call any tools. **Cancelled about 9 s in** | None | Turn cancelled. Composer ready | — | 0.73 from click to terminal UI |

Usage for rows 1–3 was read from the closed SQLite profile. Rows 5–8 were in
conversation B, which was later deleted as part of the deletion check, so
their journal rows are gone by design. Their screenshots remain.

### Scenario matrix

| # | Scenario | Evidence | Result |
| --- | --- | --- | --- |
| 1 | Three consecutive Agent requests in one thread | Real rows 1–3 | Pass. One sidebar entry, three linked turns (SQLite: 3 links in conversation 1) |
| 2 | Search, read first match, summarize | Real rows 1–3 | Pass. The summary made no tool call |
| 3 | Calculator, then a question about its result | Real rows 5–6 | Pass |
| 4 | Chat → Agent → Chat in one thread | Real row 4 after rows 1–3. SYNTHETIC `f03` and the live fixture run | Pass. Mode changes never created a conversation |
| 5 | Two conversations, isolated context and results | Real row 7. SYNTHETIC navigation run | Pass |
| 6 | Navigate away during execution and return | Real row 2 (`desktop-r02-away`). SYNTHETIC `f05` | Pass. Return and cancel available. The other thread wasn't updated |
| 7 | Cancel a follow-up turn | Real row 8. SYNTHETIC cancel from another conversation | Pass |
| 8 | Inline approval, denial and expiry | SYNTHETIC `f06` (recovered approval at 1024 × 768), `f07` (denied), `f08` (expired) | Pass in the UI. **No real approval was run this round.** Task 01-C exercised the same backend approval path on the real desktop |
| 9 | Restart with persisted Chat and Agent messages | Real: restarted the rebuilt binary on the same profile (`desktop-r14`, `desktop-r15`) | Pass. Prompts, Agent answers and the Chat reply persisted. Model unloaded. No grants restored (`desktop-r17`) |
| 10 | Earlier native results unavailable after restart | Real `desktop-r15`. SYNTHETIC `f12` | Pass. "Native result details are no longer available". No invented contents |
| 11 | Search, rename, deletion | Real `desktop-r09` (search for "irrigate" found only conversation A), rename re-verified after the focus fix, `desktop-r21`/`r22` (delete, then "12193" has no match). SQLite shows the deleted conversation's messages, links and tasks gone | Pass |
| 12 | Legacy standalone history accessible | SYNTHETIC `f13` | Pass in the UI. The real fresh profile had no pre-migration records |
| 13 | Native results visible after synthesis failure | SYNTHETIC `f09` | Pass in the UI. Covered by tests. Task 01-C reproduced the real case |
| 14 | Context omission displayed accurately | SYNTHETIC `f10` (`excludedTurns: 3`). Tests | Pass in the UI. The real runs reported 0 omissions |
| 15 | Light and dark at 1024 × 768, 1280 × 800, 1600 × 900 | SYNTHETIC `t-dark-1024`, `f01` (1280 dark), `t-dark-1600`, `t-light-1024`, `t-light-1280`, `t-light-1600`. Real light and dark at the 1120 × 780 window (`desktop-r18`, `r19`) | Pass. No overflow or clipped controls. The real window can't be resized without a window manager |

Remount recovery of an active turn and its pending approval used the dev
fixture (`f06`), where the turn starts before the UI mounts and its early
events are missed. Recovery restored the owner, the exact target, the
operation and the countdown.

### Screenshots

**Real desktop**, `docs/screenshots/task-02-b/desktop-*.png`:

| File | Shows |
| --- | --- |
| `desktop-s03` | Qwen3.5-4B loaded at 4,096 context in the isolated profile |
| `desktop-r01-search` | Turn 1: native search card and the model answer |
| `desktop-r02-away`, `desktop-r02-read` | Running turn seen from another thread. Turn 2's native file preview and answer |
| `desktop-r03-summary` | Turn 3 with no tools, then the answer |
| `desktop-r04-chat` | Chat follow-up in the same thread, with model attribution |
| `desktop-r11-thread-top` | Thread top with turn 1 |
| `desktop-r05-calc`, `desktop-r06-calc-followup`, `desktop-r07-isolation` | Second conversation: calculator, recall, isolation |
| `desktop-r08-cancel-running`, `desktop-r08-cancelled` | Live follow-up, then the cancelled turn |
| `desktop-r09-search` | Search across saved Agent and Chat text |
| `desktop-r12-drawer`, `desktop-r13-drawer-task` | Details drawer: conversation facts and the selected turn's usage, execution, permission record and event log |
| `desktop-r14`, `desktop-r15` | After restart: saved thread, unavailable-results notice |
| `desktop-r16`, `desktop-r17` | After restart: no model, no grants |
| `desktop-r18`, `desktop-r19` | Dark theme on the real binary |
| `desktop-r20-privacy-settings`, `desktop-r21-delete-dialog`, `desktop-r22-search-after-delete` | Privacy copy, delete disclosure, search after deletion |

**SYNTHETIC fixture**, `docs/screenshots/task-02-b/f*.png` and `t-*.png`:

| File | Shows |
| --- | --- |
| `f01`, `f02` | Three-turn ledger thread |
| `f03` | Chat, Agent and Chat turns mixed |
| `f04` | Live follow-up progress |
| `f05` | Navigation away during work |
| `f06` | Recovered approval at 1024 × 768 with menus locked |
| `f07`, `f08` | Denied and expired approvals |
| `f09` | Native results after synthesis failure |
| `f10` | Context omission |
| `f11` | Scope-kind mismatch guidance |
| `f12` | Restart with unavailable results |
| `f13` | Legacy execution record |
| `f14`, `f15` | Privacy settings and delete disclosure |
| `t-*` | Theme and size matrix |

## Restart and privacy behavior

Observed on the real binary, in the same isolated profile:

- After restart, every saved prompt, Agent answer and Chat reply rendered in
  order, under the same conversation and title.
- Agent turns whose native step succeeded showed the unavailable-results
  notice instead of their earlier file list and preview.
- Grant chips and access entries were empty, and the model was unloaded.
- The Dark preference persisted.
- Deleting a conversation removed it from Recent and from search. SQLite
  confirmed its messages, turn links and tasks were removed, and the remaining
  conversation was intact.

The UI makes no claim about encrypted storage, physical erasure or backups.
It states that the database is ordinary unencrypted SQLite, and that
content-free audit entries can remain.

## Known limitations

- **No real approval this round.** Approval, denial and expiry were verified
  in the UI with synthetic fixtures and unit tests. The real-desktop approval
  path was exercised in Task 01-C, and the backend approval code didn't change
  in this task.
- **No real synthesis failure or omission.** The real runs never hit a
  synthesis failure or a context omission, so those states are evidenced by
  fixtures and tests.
- **Omission is live-only.** The `excludedTurns` note appears only while this
  window received the turn's events. It isn't persisted for Agent turns, so a
  reopened thread can't show it.
- **Execution records depend on a scan.** Journal entries are kept out of
  "Execution records" only for the 30 most recent conversations. Linked tasks
  from older conversations can still appear there, because the journal list
  doesn't identify linked tasks.
- **Journal detail is capped.** It is loaded for the newest 24 Agent turns of
  an open thread. Older turns show "Loading saved activity…" until scrolled
  into a refreshed view.
- **Recovery scope.** Remount recovery was exercised with the fixture only;
  reloading the real WebView mid-turn wasn't practical in this harness.
- **Fixed real window size.** The real window is fixed at 1120 × 780 under
  Xvfb without a window manager. The 1024, 1280 and 1600 matrix used WebKitGTK
  captures of the same frontend.
- **Answer quality.** The 0.6B model was not part of this run. A completed
  turn reflects runtime outcome, not answer quality.

## Required backend follow-ups (for Codex)

1. **Link flag on journal summaries.** Add `conversationId` or a `linked` flag
  to `AgentTaskSummary`, or a standalone-only filter on `list_agent_tasks`.
  The frontend could then list legacy records without scanning conversations.
2. **Persist the omission count.** Store `excludedTurns` for Agent turns, for
  example in assistant `stats` or on the turn, so reopened threads can show
  accurate context-omission notes.
3. **Inline task status.** Include task state and error code in each Agent
  `UnifiedTurn`, to avoid one `get_agent_task` call per turn when opening a
  long thread.
4. **Richer active-turn data.** Add `tool`, `taskTimeoutMs` and the start
  timestamp to `ActiveAgentTurn`. The UI currently infers the approval tool
  and derives timing from the journal and the turn.
5. **Saved error messages.** The saved task `error` is a code only. A
  persisted fixed message would let old turns show the same text as live
  ones.

No backend defect blocked this integration.
