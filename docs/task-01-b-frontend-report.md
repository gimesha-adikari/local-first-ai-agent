# Task 01-B frontend report: conversation-first desktop

This report covers the frontend redesign on `main` at `18d9bb8`. Task 01-A's
uncommitted backend work was already in the tree. The backend handoff and the
Rust source were treated as the source of truth for IPC. No Rust, Cargo,
SQLite, capability or permission-policy file was edited. The starting status
and frontend diff were recorded before editing. The old frontend's
uncommitted edits are kept in
[`task-01-b-frontend-baseline.patch`](task-01-b-frontend-baseline.patch), so
replacing the old Agent dashboard loses nothing.

Final product approval belongs to the user. This report describes what was
built and observed. It is not a sign-off.

## Design decisions

- **One workspace, not two apps.** The permanent three-column Agent dashboard
  is gone. Chat and Agent share a single shell with a sidebar, a compact
  header, a central thread, one composer and an optional details drawer. Mode
  is a composer control, not a separate screen.
- **Unified, honest navigation.** The sidebar's Recent list merges saved
  conversations with Agent tasks from the journal, newest first. Agent rows use
  a distinct icon. A task started in this session shows its prompt as its
  title. Older tasks appear as "Agent task · date" because the backend does
  not store prompts. Opening one says that only execution metadata survives a
  restart.
- **Final answer first.** Token rates, prompt timings, task IDs, budgets,
  audit records and the raw event log moved to the details drawer. The thread
  shows the answer, reasoning on request, clear status notices and native
  results.
- **Native results separate from model output.** Native results appear under
  a "Native results" heading labelled "Returned by local tools, not written by
  the model", with a badge on each card. The model's text follows under "Model
  answer: Interpretation of the results above". Failures, truncation and
  uncertain outcomes are never smoothed over.
- **Visual system.** A restrained graphite dark theme and a warm-neutral light
  theme are built from semantic tokens (`--canvas`, `--sidebar`, `--surface`,
  `--border`, `--text`, `--text-secondary`, `--accent` and others) using
  `light-dark()`. The `data-appearance` attribute pins System, Light or Dark.
  The requested starting colors are used for dark. Light uses `#f7f6f2`,
  `#efeee8`, `#ffffff`, `#22221f`, `#5b5a54`, and a darker sage accent
  `#55624a` for contrast. Radii are 6–16 px. There are no gradients and no
  bright accent buttons. The send button uses the text color, as in the
  reference.
- **Contrast checks (WCAG relative luminance):** dark primary text on canvas
  is about 14:1. Secondary `#a3a39f` is about 7:1, tertiary `#8c8c87` about
  5.3:1, and accent `#a1ad96` about 8:1. Light primary is about 15:1,
  secondary `#5b5a54` about 6.6:1, and tertiary `#6e6d67` about 5.0:1 on
  `#f7f6f2`. White on the light accent is about 6.5:1.
- **Typography and assets.** System font stacks only (Inter, Cantarell,
  Ubuntu, Noto Sans or system-ui; JetBrains Mono, Ubuntu Mono or DejaVu Sans
  Mono). Icons are new inline SVG line drawings in `icons.tsx`. No fonts,
  images or runtime CDN requests.
- **Motion.** 120–200 ms fades and chevron rotations only. They are disabled
  under `prefers-reduced-motion`.

## Implemented functionality

### Shell and navigation (`App.tsx`, `Sidebar.tsx`, `DetailsDrawer.tsx`)

- Collapsible sidebar with an icon rail when collapsed. Below 860 px it
  overlays the content with a scrim. It contains New thread, inline Search
  (Escape clears it), Models, Recent and Settings.
- Search filters conversations through `list_conversations`. Agent tasks are
  filtered client-side by their visible title.
- The header shows the thread title and a status pill: model state dot,
  "Local" and the model name. The pill opens Models. A conversation menu offers
  Rename, Instructions and Delete. A details toggle opens the drawer.
- The details drawer sits beside the content above 1280 px and overlays it at
  or below 1280 px. It contains conversation facts and instructions,
  last-response statistics, model facts, task state, usage, executions, the
  permission record, the event log and a file-access summary. Escape closes
  it.
- New thread opens an empty workspace with mode-specific suggestions that fill
  the composer. Without a model, it shows an Import or Load call to action.
  The first chat send creates the conversation, so no empty conversations are
  saved.

### Chat (`ChatView.tsx`, `chat.ts`, `MarkdownMessage.tsx`)

- Direct responses by default. Every `send_message` and `regenerate` now
  passes an explicit `options.enableThinking`. It is `true` only when the
  composer's Thinking toggle is on and the loaded model supports thinking.
- Streaming Markdown parses the accumulated content. A collapsible
  Reasoning/Thinking disclosure sits above the answer and uses the existing
  stream-safe `<think>` parser. Reasoning is shown verbatim as plain text, not
  Markdown. Code ligatures are disabled, so `->` stays literal.
- Code blocks have a language label and a Copy code button.
- Stop and Regenerate are available. Earlier attempts collapse into "Previous
  attempt · Stopped, Cut off or Failed".
- Status notices cover every non-complete state:
  - Truncated with an answer: "Response cut off at the output limit".
  - Truncated with reasoning only: "Thinking used the whole N-token output
    budget".
  - Failed with `end_of_generation` and no answer: "The model finished without
    an answer", with guidance to regenerate or turn thinking off.
  - Generic failure, Stopped and Interrupted.
  - A last user message without a saved reply gets "Generate a response".
- Truncated notices offer **Regenerate with N tokens**. This is a one-turn
  `options.maxTokens` override within 1–4096, below the loaded context and
  leaving 512 tokens of room.
- IPC rejections still reconcile through `get_conversation`, so a durable
  failed row is shown.
- Excluded context turns appear as a quiet note under the reply.

### Generation limits

- A saved limit below 768 shows a dismissible banner: "Replies are limited to
  256 tokens". **Use recommended 768** saves only `maxTokens` for the active
  model, and only when the user clicks it. **Keep 256** hides the banner for
  the session. Nothing is changed automatically.
- The composer options popover (sliders icon) offers a one-reply output limit
  that clears after the turn starts, with the saved value as placeholder.
- Models and Settings have **Restore recommended values**: output 768,
  temperature 0.7, top-p 0.8, top-k 20, thinking off. It edits the form only;
  Save is still required. Context length and CPU threads are kept because they
  depend on hardware and use.

### Agent conversation (`AgentView.tsx`, `NativeResults.tsx`, `useAgent.ts`, `agent.ts`)

- The original request is shown as a user turn. A live line shows the current
  activity with elapsed and remaining time, for example "Working out the next
  step…" or "Waiting for your approval…".
- Planning, Validating, Authorized, Executing and Observing are condensed into
  meaningful steps such as "Searched selected files", "Waiting for approval to
  launch KCalc" and "A proposed step failed validation and was not run". A
  chevron expands the timestamped low-level event list.
- Native results are merged by `operationId` from live `observation` events,
  `finished.nativeResults` and, after completion, `get_agent_task` recovery.
  They are rendered whenever present, including for `failed` tasks and null
  answers. Structured views cover:
  - Calculator: large formatted value.
  - Device information: grid with "Not reported" for null values, including
    GPU.
  - File search: count, monospace paths, "Show all", and a truncation warning
    worded as possibly incomplete.
  - File preview: size, scrollable preview, copy, and a "Preview only"
    warning when truncated.
  - Application discovery: names only, opaque IDs hidden, with a truncation
    note.
  - Application launch: "Launch request dispatched for KCalc", noting that
    this is not confirmation that a window opened.
  - Native failures and unexpected shapes are shown as received, never
    reinterpreted.
- Errors appear in a callout. It shows the backend message plus guidance for
  `model_context_limit`, `decision_output_limit`, `invalid_arguments`,
  `permission_denied`, `timed_out` and the other codes. When native results
  exist, it adds "The native results above are verified and remain valid.
  They won't be run again automatically." The outcome line reads, for example,
  "Native step succeeded, answer failed". Nothing is retried automatically.
- A model answer with no native results is labelled "No tools were run. This
  is the model's own reply, not a verified result."
- A denied approval ends with "You declined to launch …" and a neutral "You
  declined the request" outcome.
- `journalIncomplete` shows a warning not to treat the history as a complete
  record.
- Event handling keeps the original race-safe flow: subscribe first, queue
  events until the ID returns, and use `finished` or `failed` as terminal
  signals. Cancellation works during approval.
- Older tasks show saved executions with accurate wording ("outcome is
  uncertain", "was not run"), the state and the persisted error. The view
  explains that request, answer and result contents aren't stored.

### Approvals

- The approval request appears as an inline card at the end of the stream
  instead of a modal. It shows the exact Rust-supplied `displayName`,
  `desktopFile` and `executable`, plus a countdown from `expiresInMs` that
  turns amber in the last 10 seconds.
- Actions are Cancel task, Deny and **Approve once**. Approve is disabled
  when the target is missing or the request has expired. Approval sends the
  exact live `taskId` and `operationId`. Backend errors such as expiry are
  shown in the card.

### Composer (`Composer.tsx`)

- Auto-resizing input up to 240 px.
- Controls: Chat/Agent mode menu with an explanation when Agent is
  unavailable, model menu (imported models, loaded check, load or switch on
  select, Manage models…), Thinking toggle (Chat mode, supported models only)
  and options popover (output override in Chat, task time limit 1–600 s in
  Agent).
- In Agent mode only, a + menu offers Select a file…, Select a folder… and
  Manage access…. No file controls appear in Chat mode, because chat doesn't
  use grants.
- Active grants appear as chips with the folder or file icon, basename,
  "Read-only" and a revoke ×. The tooltip gives the remaining time.
- A single Send/Stop button: Stop cancels a running Agent task or stops
  generation. A status line sits underneath.
- Typing stays enabled while a response runs. Draft text is kept across mode,
  model, thinking and option changes and across navigation. Enter-to-send
  follows the saved preference.

### Permissions (`Dialogs.tsx` AccessDialog, chips, drawer)

- Uses `AgentScopeDisplay` as delivered: `displayName`, `kind`, `access`,
  `state` and `remainingMs`. The countdown is extrapolated from response time.
  The list refreshes at the next expiry and after picker or revoke actions.
- `id` is used only for `revoke_agent_scope`. It never appears in copy,
  tooltips or prompts; a test checks this.
- File access dialog: native file and folder pickers, active grants first,
  then revoked or expired grants dimmed with their state. Pickers are disabled
  while another operation runs (the backend reserves the operation gate).
  Revoke stays available.

### Models and settings

- **Models dialog.** Library list with loaded indicator and Import model…
  (native GGUF picker). The selected model shows Load, Switch (unload, then
  load) or Unload. Loaded models show name, architecture, parameters, file
  size, loaded and trained context, thinking support, Agent compatibility and
  load time. Per-model settings have field hints, a reload-required note,
  Restore recommended, Save and Discard. Load errors appear in a callout and
  status is re-read after failure. Guidance: Qwen3-0.6B for lightweight chat
  and development, Qwen3.5-4B for Agent tasks (4096-token context suggested).
  Matching file names get that label. No paths are hardcoded and nothing is
  downloaded. The library and detail panes can hold future download or
  routing features.
- **Settings.** Appearance (System, Light, Dark), Enter sends, defaults for
  newly imported models with Restore recommended, and the database location.
  Save is enabled only when something changed.

### Accessibility and responsiveness

- Landmarks: navigation, main, the details drawer as `aside`, and dialogs.
  Icon buttons carry `aria-label` and a tooltip. Menus are `role="menu"` with
  menu items, Arrow, Home, End and Escape keys, and focus return to the
  trigger. Popovers are non-modal and close on Escape. Toggles use
  `aria-pressed` or `role="switch"`. Appearance uses a radio group.
- Dialogs use native `showModal`, which traps focus and makes the page behind
  inert. They return focus on close and start focus on the dialog so the
  close button isn't highlighted.
- Visible `:focus-visible` rings use the focus token. Live regions announce
  generation and task activity.
- Layout uses `grid-template-rows: minmax(0, 1fr)` to keep the composer
  pinned. The thread is capped at a 760 px reading width. Long names
  ellipsize. Code, tables and previews scroll inside their own boxes. There is
  no horizontal page overflow.

## Backend contract integration

| Contract item | Frontend use |
| --- | --- |
| `send_message` / `regenerate` `options` | Always sent. `enableThinking` explicit, `maxTokens` only for a valid one-turn override (`chatRequestOptions`) |
| `ChatState`, `GenerationStats.reason` | `responseNotice` covers `truncated` (with and without an answer), `failed` + `end_of_generation`, `cancelled`, `interrupted` |
| IPC rejection may leave a durable failed row | Reconciled with `get_conversation` after every send or regenerate |
| `ModelInfo.supportsThinking` | Gates the Thinking toggle and Agent mode (the backend requires it in `start_agent_task`) |
| `save_model_settings` | Explicit only: banner action and Models dialog Save |
| `start_agent_task { prompt, taskTimeoutSeconds }` | Timeout from the options popover, validated 1–600 |
| `agent-event` incl. `finished.nativeResults` | `applyAgentEvent` merges results by `operationId`. `get_agent_task.nativeResults` recovers missed events |
| `approval_required.target` | Inline card shows the exact trusted target. Approve is disabled without a target or after expiry |
| `respond_agent_approval` | Exact live `taskId` + `operationId` |
| `AgentScopeDisplay` | Basename, kind, read-only, state and remaining time. `id` used only to revoke |
| Native result keys (`paths`, `content`, `file_bytes`, `applications`, `app_id`, `dispatched`, device snake case) | Typed defensively in `resultView`; unexpected shapes fall back to a raw view |
| Audit records without `scopeId` | Drawer shows action, tool and reason only |

## Changed-file inventory

All paths are under `apps/desktop/src` unless stated.

| File | Change |
| --- | --- |
| `App.tsx` | Rewritten shell: navigation, views, chat and agent flows, model operations, banners |
| `Sidebar.tsx` | New. Collapsible navigation and unified Recent list |
| `DetailsDrawer.tsx` | New. Contextual technical details |
| `ChatView.tsx` | New. Messages, reasoning, notices, actions |
| `Composer.tsx` | New. Shared composer |
| `AgentView.tsx` | New. Conversational Agent thread, activity, approval, outcome |
| `NativeResults.tsx` | New. Structured native result cards |
| `useAgent.ts` | New. Agent events, tasks, approvals and grants hook (replaces the panel's state) |
| `agent.ts` | New. Pure Agent presentation logic and event reducer |
| `agentTypes.ts` | New. TypeScript mirror of the Agent IPC contract |
| `ui.tsx` | New. Menu, Popover, Dialog, Switch, Segmented, Callout, IconButton |
| `icons.tsx` | New. Local SVG line icons |
| `Dialogs.tsx` | Rewritten: Models, Settings, File access, conversation edit dialogs |
| `chat.ts` | Added `ChatOptions`, `chatRequestOptions`, `responseNotice`, recommended-settings helpers, `suggestedOutputLimit` |
| `MarkdownMessage.tsx` | Code header with language label; icon copy button |
| `styles.css` | Rewritten design tokens and components |
| `main.tsx` | Loads dev-only fixtures in a plain browser |
| `dev/fixtures.ts` | New. **Development only** synthetic IPC for browser inspection. Not in production bundles (verified by grep of `dist/`) |
| `agent.test.mjs` | New. 8 regression tests |
| `chat.test.mjs` | 3 new regression tests |
| `Agent.tsx` | Removed. The old three-column dashboard; its uncommitted edits are preserved in the baseline patch |
| `docs/task-01-b-frontend-report.md` | This report |
| `docs/task-01-b-frontend-baseline.patch` | Starting frontend diff (preservation) |
| `docs/screenshots/task-01-b/*.png` | Screenshots |

No dependencies were added. `package.json` and `package-lock.json` are not
changed by this task. A temporary local preview launcher was removed after
use. The release binary in `target/release` was rebuilt; that is a build
artifact outside Git.

## Screenshots

### How these were captured

The numbered screenshots are **actual renders of the implemented React UI**.
They were captured with WebKitGTK 2.52, the engine the Linux Tauri app uses,
from the Vite dev server inside a private Xvfb display. In a plain browser,
`main.tsx` installs `dev/fixtures.ts`, a development-only IPC fixture. It
supplies synthetic models, conversations, streaming text, Agent events, grants
and native results. These screenshots therefore show real UI code paths
driven by **synthetic data**, not model output or real device facts. For
example, the device figures and file names are fixtures. The integrated
desktop section below shows the release binary itself.

| # | Required view | File |
| --- | --- | --- |
| 1 | Empty workspace | ![](screenshots/task-01-b/01-empty-workspace.png) |
| 2 | Completed chat response (thinking on, reasoning collapsed, code block) | ![](screenshots/task-01-b/02-chat-response.png) |
| 3 | Agent task in progress | ![](screenshots/task-01-b/03-agent-in-progress.png) |
| 4 | Completed Agent task with native results | ![](screenshots/task-01-b/04-agent-completed-native-results.png) |
| 4b | Native results after model synthesis failure (`model_context_limit`, truncated search) | ![](screenshots/task-01-b/04b-native-results-after-model-failure.png) |
| 5 | File permissions (one active, one revoked) | ![](screenshots/task-01-b/05-file-permissions.png) |
| 5b | Composer + menu and grant chips | ![](screenshots/task-01-b/05b-composer-access-menu.png) |
| 6 | Application approval | ![](screenshots/task-01-b/06-application-approval.png) |
| 7 | Model management | ![](screenshots/task-01-b/07-model-management.png) |
| 8 | Settings | ![](screenshots/task-01-b/08-settings.png) |
| 9 | Light theme (Agent result) | ![](screenshots/task-01-b/09-light-theme.png) |
| 9b | Light theme (chat) | ![](screenshots/task-01-b/09b-light-chat.png) |
| 10 | Smaller window, 1024 × 768, collapsed rail and 256-token banner | ![](screenshots/task-01-b/10-smaller-window-1024x768.png) |
| 10b | Truncated response with one-turn larger limit | ![](screenshots/task-01-b/10b-truncated-response.png) |
| 11 | 1600 × 900 with the details drawer beside the content | ![](screenshots/task-01-b/11-wide-1600x900-details.png) |
| 12 | No model loaded | ![](screenshots/task-01-b/12-no-model.png) |

### Integrated desktop

The release binary was rebuilt with the new frontend embedded:
`npm run build`, then `cargo build --release --locked -p local-first-ai-agent
--features tauri/custom-protocol -j 1`. The build used `CARGO_BUILD_JOBS=1`,
`CMAKE_BUILD_PARALLEL_LEVEL=1`, the README's `BINDGEN_EXTRA_CLANG_ARGS`, and
`nice`. Each build took about 1 min 15 s.

The binary ran in a private Xvfb display with a private D-Bus session and a
**temporary XDG profile**, so the user's conversation database was not
opened. The settings screenshot shows the temporary database path. The window
was driven with X11 XTest input from a scratch script. Models were imported
through the real native GTK picker from the existing local files Qwen3-0.6B
Q4_K_M and Qwen3.5-4B UD-Q4_K_XL. Inference ran on the CPU with 4 threads. The
folder grant used a synthetic folder of three `.txt` files and one `.json`
file. **Everything below is real backend and model output.**

| File | What it shows (observed) |
| --- | --- |
| ![](screenshots/task-01-b/desktop-01-empty-no-model.png) | Fresh profile: empty workspace, "Import a model to begin". System appearance resolved to light under Xvfb |
| ![](screenshots/task-01-b/desktop-02-model-loaded.png) | Qwen3-0.6B imported through the native picker and loaded: real GGUF facts (qwen3, 596 M, 378 MiB, 2,048 of 40,960 context, thinking supported, 5.2 s load) |
| ![](screenshots/task-01-b/desktop-03-real-chat.png) | Direct-mode chat (no reasoning block), Rust code block with Copy code, no metrics in the thread. Captured before the ligature fix, so `->` shows as an arrow glyph; see desktop-16 |
| ![](screenshots/task-01-b/desktop-04-real-thinking-turn.png) | Thinking toggled on for one turn: collapsible Reasoning separate from the answer "17 times 23 is 391" (correct). Captured before the plain-text reasoning fix |
| ![](screenshots/task-01-b/desktop-05-real-agent-in-progress.png) | Live Agent task: "Working out the next step… 0:03 · 4:56 left", spinner in the sidebar row, Stop available, grant chip `grant-fixture · Read-only` |
| ![](screenshots/task-01-b/desktop-06-real-agent-completed.png) | Calculator task completed in 18 s: native result 12,193, separate model answer "12193" |
| ![](screenshots/task-01-b/desktop-07-real-app-discovery.png) | Real `apps.list`: 46 applications as names, with `truncated` respected ("may be incomplete"). The 0.6B model answered with opaque IDs and didn't request a launch, matching the v0.1.0 finding. The native result is shown independently |
| ![](screenshots/task-01-b/desktop-08-real-model-switch.png) | Switching to Qwen3.5-4B at a 4096 context: "Switching…" and "Loading model…" states (load took 38.6 s) |
| ![](screenshots/task-01-b/desktop-09-real-approval.png) | Qwen3.5-4B ran discovery, then requested a launch after about 100 s. Inline approval with the exact Rust-supplied target: KCalc, `/usr/share/applications/org.kde.kcalc.desktop`, `/usr/bin/kcalc`, "Expires in 0:58" |
| ![](screenshots/task-01-b/desktop-10-real-denied.png) | After **Deny**: task Denied, discovery result still shown. No KCalc window existed in the display (checked with `xwininfo`). The step and guidance wording seen here was then corrected; see below |
| ![](screenshots/task-01-b/desktop-11-real-settings.png) | Real Settings dialog with Dark selected |
| ![](screenshots/task-01-b/desktop-12-real-file-access.png) | Real grant from the native folder picker: `grant-fixture · Folder · Read-only · 47 min left`. No scope ID visible |
| ![](screenshots/task-01-b/desktop-13-real-revoked.png) | After Revoke: grant shows Revoked and the composer chip is gone |
| ![](screenshots/task-01-b/desktop-14-real-dark-agent.png) | Saved Dark appearance applied to the completed calculator task |
| ![](screenshots/task-01-b/desktop-15-real-after-restart.png) | After restarting the rebuilt binary on the same profile: earlier tasks listed as "Agent task · Nov 25" and opened with the "From an earlier session" notice and saved execution steps only. Dark appearance persisted; the model was unloaded, as expected |
| ![](screenshots/task-01-b/desktop-16-real-saved-chat-dark.png) | Saved chat after restart in the final build: `->` rendered literally (ligatures off) and reasoning shown verbatim (`17*20`, not Markdown emphasis) |

Real-run findings and what changed:

1. Code fonts turned `->` into an arrow glyph. Ligatures are now disabled for
   `code` and `pre`.
2. Raw reasoning rendered `17*20 … 17*3` as italics. Reasoning is now plain,
   verbatim text.
3. With Qwen3-0.6B, "Search the selected folder for text files with ledger in
   the name" ended **Completed** with no tool run. The answer was a
   Python-dict-like tool-call string. The UI shows it only as text and runs
   nothing. Answers without native results now carry the label "No tools were
   run. This is the model's own reply, not a verified result." The
   backend-side follow-up is listed below.
4. After a denied launch, the step still read "Waiting for approval…" and the
   guidance talked about selecting files. The step now reads "You declined to
   launch KCalc". A denied outcome is titled "You declined the request: The
   action you declined was not run", in a neutral tone. A regression
   assertion covers this.

Fixes 1 and 2 were rechecked in the rebuilt binary (desktop-16). Fixes 3 and
4 are covered by tests and the production build, but were not re-run against
a real model after the change.

## Test results

| Check | Result |
| --- | --- |
| `npm test` (Node test runner) | 19 passed, 0 failed (8 new Agent tests; 3 new chat tests) |
| `npm run typecheck` (`tsc --noEmit`, TypeScript 7.0.2) | Passed |
| `npm run build` (typecheck + Vite 8.3.8 production build) | Passed: 42.1 kB CSS, 438.4 kB JS (133.0 kB gzip) |
| `git diff --check` on `apps/desktop/src` | Passed |
| Release desktop build with embedded frontend | Passed (see Integrated desktop) |
| Fixture exclusion | `grep` for fixture symbols in `dist/assets` found nothing |

New regression tests:

- `agent.test.mjs`:
  - Native results stay visible after a `failed` + `model_context_limit`
    finish with a null answer, and truncation is preserved.
  - Terminal results merge with live observations by operation ID.
  - Events for another task are ignored.
  - The approval keeps its exact trusted target, expiry and launch target
    mapping, and clears after execution. A denial reads "You declined to
    launch KCalc" with denial-specific guidance.
  - Progress condensation never shows ended steps as still running, and
    execution labels say "uncertain" and "not run" accurately.
  - Structured result views respect truncation and reject malformed shapes.
  - **Permission labels use sanitized metadata and never contain the
    revocation reference.**
  - Usage parsing and timeout validation.
- `chat.test.mjs`:
  - **Explicit per-turn thinking**: direct by default, opt-in only with model
    support, and override validation.
  - Response notices for truncated (answer and reasoning-only), failed
    without an answer, generic failure and stopped.
  - Recommended-limit helpers don't mutate saved settings, and suggested
    overrides respect the context.

Layouts were checked at 1024 × 768, 1280 × 800 and 1600 × 900, in dark and
light. The checks covered sidebar collapse, composer visibility (a grid-row
overflow found and fixed during review), long-thread scrolling with "Jump to
latest", menus, popovers, dialogs, the drawer, keyboard Enter-to-send, and
Escape for menus, the drawer and dialogs.

## Known limitations

- Fixture screenshots use synthetic data. Real-model behavior depends on the
  model. The Task 01-A and v0.1.0 verification reports cover model accuracy.
- An Agent task's prompt and answer are known only for tasks started in the
  current window. After restart, tasks show as "Agent task · date" with
  saved executions only, because the backend persists neither.
- Native result contents survive only in this window's memory and the
  backend's latest-task cache. Restart clears them, as the UI states.
- Each Agent request runs as a new task. There are no follow-up turns inside
  an Agent task, because the backend has no task conversation.
- Model facts (architecture, parameters, context and compatibility) are
  known only for the loaded model. `ModelReference` has no file metadata.
- Search matches conversation titles (backend) and visible Agent task titles
  (client side) only.
- The Thinking toggle is per composer session. It is sent explicitly with
  every turn and starts off at launch. The model's saved "Think by default"
  setting is labelled as a saved preference only, following the handoff's
  direct-by-default rule.
- Window maximization was not separately captured. The layout is fluid and
  was checked at 1600 × 900. The integrated desktop ran at its configured
  1120 × 780 window without a window manager.
- The real-model desktop pass covered calculator, application discovery, the
  launch approval with denial, grant and revoke, chat and thinking. It did
  not show a successful real file search, because the 0.6B model didn't
  dispatch it (finding 3). The file search and preview displays, and the
  native-results-after-failure display, are evidenced by fixture screenshots
  and tests. Task 01-A verified those native payloads.
- The dark theme was visually checked on the real binary only after switching
  the preference. Under Xvfb, the System setting resolves to light.

## Required follow-up backend changes (for Codex)

1. **Persist Agent task prompt and final answer** (or a title) in the journal
   and return them in `AgentTaskSummary` or `AgentTaskDetail`. This enables
   real Agent transcripts and searchable titles after restart.
2. **Optional persistence of bounded native result payloads.** Today only
   metadata survives a restart. If desired, store the bounded observation
   with explicit retention so past results can be reviewed.
3. **Model file metadata without loading.** For example `inspect_model`
   returning architecture, parameter count, trained context and thinking
   support from GGUF headers. The library could then show compatibility
   before loading.
4. **Search over Agent tasks** in `list_agent_tasks` once titles are
   persisted.
5. **Model reference removal** (`forget_model`), so imported entries can be
   removed from the library. No command exists today, so the UI offers none.
6. **Final answers that are really tool requests.** Qwen3-0.6B produced a
   final decision whose answer text was a tool-call-shaped string, and the
   task finished **Completed** without dispatching anything. Consider having
   the decision validator reject or flag final answers that echo the tool
   envelope, or mark tasks that complete with zero tool calls when the prompt
   expected one. The frontend renders such text only as text and labels it as
   unverified.
