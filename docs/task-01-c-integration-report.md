# Task 01-C integrated application verification

This report covers the Linux desktop worktree at `18d9bb8` with the existing
Task 01-A and Task 01-B changes. The starting working-tree diff, status and
hashes were recorded before editing. The conversation-first workspace is
the desktop under test. Verification ran on November 25, 2026. Release
approval remains pending manual review.

The starting dirty-tree inventory, binary diff and file hashes were saved
under `/tmp/agent-01c.XzmWCt/`. Of the 106 starting paths checked, 92
still match byte for byte or are still absent. The 14 source paths edited
for this task are listed below. In particular, the prior removal of
`apps/desktop/src/Agent.tsx` and the new stylesheet remain as found.

## Findings and repairs

- The decision parser checked the outer `final` envelope, then accepted
  almost any nonblank string as an answer. A bare tool envelope in that
  string could therefore produce a completed journal row with zero
  dispatches. The final-answer validator now rejects a standalone tool
  envelope, including the supported Python-style quoted form. It never
  converts that text into an executable call. Ordinary answers and examples
  introduced in prose or fenced as code stay in the answer channel. A
  correction uses the existing retry budget, and exhausted invalid
  decisions return a specific recovery message.
- Task 01-B recorded the ledger prompt and described the embedded
  Python-dict-like answer. The original model response was unavailable for
  replay. In a fresh run of the earlier release, the same prompt reached
  `scope_missing` after one file-search proposal in 240.61 seconds. The
  journal shows zero dispatches. That trial reproduced the misleading
  denial copy, while the automated regression reproduces the described
  malformed final-answer shape.
- A native application catalog proves discovery. It supplies no evidence
  of launch. The decision prompt now calls out the remaining launch step
  and asks the model to say when it launched nothing. The thread describes
  normal completion as a model reply and states when discovery produced
  no launch record. The trusted approval and execution records still
  determine whether the application was dispatched.
- The desktop host drops its active task lease before it sends the terminal
  event. A cancel issued in that interval could return false and cause the
  frontend to discard the coming event. The frontend now accepts that late
  terminal event for the same task, keeps other task ownership intact, and
  can show the saved terminal status while reconciling.
- The approval broker now refuses an expired operation before delivering
  the reply to the worker. The inline countdown starts with the remaining
  time after target resolution. Deterministic tests exercise expiry and
  replay independently of the display. The policy's own expiry and
  single-use checks also remain authoritative.
- The first real approval-expiry trial ended safely, but showed guidance
  for increasing the overall task timer. The host now reports
  `approval_expired` separately from a task deadline. The workspace explains
  that the user can start a new task to request a fresh approval. The
  independent approval window and single-use policy stay in force.
- Live chat keeps its conversation while it owns a generation. During an
  Agent task, saved chat can be opened read-only, and a return-to-task
  control keeps a running task or pending approval reachable. The task's
  Stop control retains the same owner through navigation.
- A file permission denial previously looked like the user had clicked
  Deny. Explicit launch refusal now has its own `approval_declined` code,
  and file denial uses permission guidance. Model change controls now honor
  the same operation lock as their commands. A one-turn output override is
  cleared when either a send or a regeneration starts.

### IPC and privacy review

| Contract | Source and regression evidence |
| --- | --- |
| Chat send and regenerate | `send_message` and `regenerate` both use `options.enableThinking` and optional `options.maxTokens` on their own chat event channel. The UI keeps Thinking as an explicit choice and clears the one-reply token override on stream start. The final saved message reconciles the stream. `failed`, `truncated`, `cancelled` and `interrupted` stay distinct from `complete`. |
| Agent ordering and identifiers | `agent-event` delivers tagged events with decimal-string `taskId` and string `operationId`. The frontend queues events that arrive before the start invocation returns and uses operation IDs to merge live, terminal and recovered native results. Terminal events clear the matching approval and only release their own task ownership. |
| Grants and approval | Tauri creates file and folder grants from native picker selections and returns sanitized labels, type, read-only access, state and remaining lifetime. React revokes by session reference. A launch reply is bound to a task and single operation. Rust distinguishes an explicit `approval_declined`, an `approval_expired` wait, and the overall `timed_out` deadline. |
| Saved Agent entries | Earlier sessions have the journal status, event and execution metadata. Prompts, model answers, and private file contents stay out of this journal. A missing live terminal event can reconcile to saved status without synthesizing an answer. |
| Production fixtures | The Vite fixture import is inside `import.meta.env.DEV` and requires the absence of Tauri internals. The built production assets were checked for fixture installer signatures. The scripted Rust provider is compiled only in the test executable. |

## Engineering verification

Native checks used one Cargo and CMake job pinned to CPU 0, `nice -n 15`,
idle I/O priority, the existing release cache and Cargo offline mode.

| Check | Observed result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| Locked release workspace Clippy with warnings denied | Passed |
| Locked release workspace tests | 92 passed, 13 opt-in tests ignored in the ordinary run |
| Frontend tests | 20 passed |
| TypeScript | Passed |
| Vite production build | Passed |
| Linux Tauri release with embedded frontend | Passed |
| `git diff --check` | Passed |
| Fixture separation | Production assets contain no fixture module or installer signatures. The Rust desktop mock is test-only. |

Release verification used the executable at `target/release/local-first-ai-agent`.
The last build used `tauri build --no-bundle`, so the desktop executable was
exercised directly. Distribution packages and installer behavior were outside
this run. The early chat and simple Agent trials used earlier rebuilt
executables. A later frontend change disabled Regenerate while the model
is unloaded. The saved-chat replay verified that change, and the file and
launch matrix ran with it. The separate approval-expiry verification below
targets the final error-classification change.

Reproduce the engineering checks from the project root with the installed
Rust, Node and native Tauri dependencies.

```bash
export CARGO_BUILD_JOBS=1 CMAKE_BUILD_PARALLEL_LEVEL=1 CARGO_NET_OFFLINE=true
export BINDGEN_EXTRA_CLANG_ARGS="-idirafter $(gcc -print-file-name=include)"
cargo fmt --all --check
nice -n 15 ionice -c3 taskset -c 0 cargo clippy --release --workspace --all-targets --locked --features tauri/custom-protocol -- -D warnings
nice -n 15 ionice -c3 taskset -c 0 cargo test --release --workspace --locked --features tauri/custom-protocol
npm --prefix apps/desktop test
npm --prefix apps/desktop run typecheck
nice -n 15 ionice -c3 taskset -c 0 npm --prefix apps/desktop run tauri -- build --no-bundle --config '{"build":{"beforeBuildCommand":"npm run build -- --emptyOutDir=false"}}' -- --locked
git diff --check
```

## Test boundaries

Runs used temporary XDG directories, synthetic file collections, an isolated
Xvfb display and private D-Bus. The two supplied GGUFs were read from their
existing local locations. Most trials used a private network namespace with
loopback down and no default route. Discovery and launch trials preserved
the system UID mapping required by the trusted catalog and used an inherited
seccomp filter that rejected non-Unix socket domains. Neither application
data nor model paths were supplied by a production fixture.

## Desktop chat acceptance

The model was Qwen3-0.6B Q4_K_M with a 2048-token context, four CPU
threads, and the saved sampling settings of temperature 0.7, top-p 0.8
and top-k 20. The greeting used the saved 256-token limit. The following
direct turns used the explicit upgrade to 768, which SQLite confirmed.
Chat uses no Agent tool calls or retries.

| Prompt or interaction | Mode and limit | Answer and saved state | Live desktop evidence | Seconds |
| --- | --- | --- | --- | ---: |
| Say hello to a new user in one short sentence. | Direct, 256 | PASS. A short greeting, `complete`. | Enter submitted the prompt and streaming ended. | 2.10 |
| Write a four-line poem about rain. Include the word rain. | Direct, 768 | PASS. Four lines about rain, `complete`. | Direct answer rendered in the thread. | 20.92 |
| Write a tiny Rust program using fn main to print HELLO_FROM_RUST. Output only the code. | Direct, 768 | FAIL. The model emitted `print(...)` instead of Rust's `print!(...)`. SQLite correctly records that the model finished. | The incorrect answer appeared unchanged. The generated program was not executed. | 10.44 |
| In two short sentences explain Rust ownership. | Direct, 768 | FAIL. The answer incorrectly said ownership prevents transfer to another variable, and introduced an inaccurate claim about responsibility. State `complete`. | The desktop displayed the generated answer. | 80.93 |
| My dog is named Pico. Reply only with OK. | Direct, 768 | PASS. `OK.` was saved. | The turn streamed and finished. | 16.82 |
| What is my dog named? Reply only with the name. | Direct, 768 | PASS. Recalled `Pico.` from the prior turn. | The answer was visible and survived restart. | 16.48 |
| Calculate 17 times 23 and give the result. | Thinking, 768 | Incomplete first pass. The automation closed its private app after the wait limit. Restart recovered `interrupted` reasoning. | The interrupted notice and regenerate affordance were visible in the saved thread. | >100 |
| What is 2 plus 2? Think briefly and then give the answer. | Thinking, one-turn 512 | PASS. `4`, normally ended after 270 tokens. | The Reasoning disclosure was separate from the answer. | 12.53 |
| Write a short poem about the sea. | Thinking, one-turn 1 | PASS for exhaustion handling. One thinking token, no answer, saved as `truncated` with `token_limit`. | The UI explained that the whole output budget went to thinking. | 0.59 |
| Regenerate the sea poem with Thinking off. | Direct, 768 | PASS. A sea poem ended normally after 58 tokens. The earlier attempt stayed in history. | The suggested regeneration action completed a new answer. | 2.60 |
| Write a very long numbered story about a lighthouse. Include at least 1000 words. Then click Stop. | Direct, 768 | PASS for cancellation. Nine generated tokens were saved as `cancelled`. | Stop reached the terminal UI in 0.22 seconds from the click. | 1.10 |
| Restart and reopen the saved chat. | Model unloaded after restart | PASS. The code, Pico fact, messages and interrupted state remained visible. | Dark appearance persisted. Regenerate was disabled until model load in a replay on the final binary. | N/A |

The failures above are answer-quality failures. The response status is
about generation and does not certify code or factual accuracy.

## Desktop Agent acceptance

Unless called out separately, these trials used Qwen3.5-4B UD-Q4_K_XL,
4,096 context tokens, four CPU threads, greedy constrained JSON decisions,
Thinking disabled, up to 512 tokens per decision, and a 480-second task
deadline set in Response options. User approval had a separate ceiling
of 60 seconds. The native journal distinguishes authorized dispatches
from permission denials, malformed proposals and model replies. Wall
times measure composer submission through the terminal thread; the running
desktop shared the host with its normal background load.

| User prompt | Native selection and execution | Answer or policy verdict | Terminal state and live UI | Model calls / retries / unnecessary calls | Seconds |
| --- | --- | --- | --- | --- | ---: |
| Use the local calculator to work out 137 times 89. Report the result. | One `math.calculate`, result 12,193. | PASS. Model returned 12,193. | `completed`. One Calculator card and a separate model reply. | 2 / 0 / 0 | 91.12 |
| What is 19 times the sum of 23 and 7? Use the calculator. | One `math.calculate`, result 570. | PASS. Correct grouping and result. | `completed`. Native 570 and model 570 appeared separately. | 2 / 0 / 0 | 94.88 |
| Inspect this device using the available tool. What operating system is it running? | One `device.info`, Linux x86_64. | PASS. Model answered Linux. | `completed`. Device information card and model reply. | 2 / 0 / 0 | 86.31 |
| Say hello in one short sentence. This is a greeting only. | Correctly selected no tool. | PARTIAL. The greeting was appropriate, but had two sentences. | `completed`. The actual thread said “No tools were run” and “Model replied.” | 1 / 0 / 0 | 40.32 |
| Explain what a tool_call JSON envelope is. Include an example for math.calculate in a fenced code block. This is documentation only. Do not invoke any tool. | Correctly selected no tool. | PARTIAL. It explained the envelope and supplied the calculation example, with literal escaped newlines in place of a fenced block. | `completed`. Plain model text was kept as content and never dispatched. | 1 / 0 / 0 | 202.83 |
| List the locally installed desktop applications using the discovery tool. Is KCalc on the list? Do not launch anything. | One `apps.list`, 45 entries with a native limit warning. Zero launch calls. | PASS for discovery and target identification. The model confirmed KCalc and also exposed its session catalog ID. | `completed`. Native discovery and answer appeared independently, with “No application launch was dispatched.” | 2 / 0 / 0 | 182.47 |
| Read ledger.txt on this computer and tell me its contents. | One `files.read` proposal. `scope_missing` before dispatch, zero native results. | PASS for denial. The model could not read the file. | `denied`. Permission guidance pointed to the native selection control. | 1 / 0 / 0 | 46.65 |
| Find text files whose names contain ledger in the selected directory. Use the file search and list the returned names. | One `files.search`. Four matching names, including the JSON fixture. The symlink was skipped. | PARTIAL. The model called the tool and listed its matches, but called the JSON fixture a text file when the expected files were `.txt`. | `completed`. Native list and incomplete-traversal warning were visible. | 2 / 0 / 0 | 106.39 |
| Search the selected folder for filenames containing ledger with a txt extension. Return the exact names and report any incomplete traversal. | One `files.search`. Returned `budget_ledger.txt`, `ledger-é.txt`, `ledger.txt`. The JSON file and symlink were excluded. | PASS. The model named all three and reported that traversal could be incomplete. | `completed`. One native result and separate model interpretation. | 2 / 0 / 0 | 110.68 |
| Search for text files containing ledger in the folder I just revoked. Report what the tool returns. | First proposal rejected as `invalid_arguments`. Zero dispatches after revocation. | PASS for revocation. The corrected model answer explicitly said the access had been revoked. | `completed` model reply. The thread said the proposal failed validation and no tools ran. | 2 / 1 / 0 dispatched | 107.57 |
| Use the selected file and tell me its synthetic code. | One `files.read` proposal. `scope_kind_mismatch` before dispatch on a live selected-file grant. | FAIL for task completion. The model chose the right tool but used arguments that did not match a single-file grant. The authorization check behaved correctly. | `denied`. No native content or model answer was shown. | 1 / 0 / 0 | 51.72 |
| Read the one selected file named ledger.txt. Use its exact file grant with the file-reading tool. Since it is a selected file and not a directory, leave the relative path empty. Return only its synthetic code. | One `files.read` with the exact-file grant succeeded. Native text was `Synthetic ledger code ORBIT-3795.` | PASS for the read and code accuracy. The answer included the label alongside the code. | `completed`. One native preview and separate model reply. | 2 / 0 / 0 | 112.75 |
| Use the file search to find names containing ledger in the selected folder and subfolders. Request up to 100 results. State only the number returned and whether the native result was truncated. Do not repeat the file names. | One `files.search`. The 100-file fixture yielded 83 bounded paths. | PASS. The model reported 83 paths and truncation. | `completed`. The native count and warning stayed separate from the answer. | 2 / 0 / 0 | 113.00 |
| First use the local calculator to calculate 137 times 89. Then use the device information tool to find this device’s operating system. Report both. | `math.calculate` followed by `device.info`, one successful dispatch each. | PASS. The model returned 12,193 and Linux. | `completed`. Exactly two native cards appeared. | 3 / 0 / 0 | 132.94 |
| Describe a fictional station in detail. Write at least 400 words. Do not call tools. Open Models, navigate away, return and cancel during model processing. | No tool calls and no generated tokens before cancellation. | PASS for cancellation. Model changes were locked, and returning to the task kept its cancellation control. | `cancelled`. Click to terminal UI took 0.776 seconds. Restart reopened an earlier-session journal entry. | 1 / 0 / 0 | 2.55 |
| Search the selected folder for files with ledger in the name, recursively. Request up to 100 results. Summarize the matches with a few examples. **Context reduced to 1,600 tokens.** | One `files.search` succeeded with 83 paths. Synthesis then reached `model_context_limit`. | PASS for preservation and recovery guidance. No model summary was produced or claimed. | `failed`. One native result card survived the error. The app suggested a shorter task or larger context. There was no automatic redispatch. | 2 / 0 / 0 | 66.89 |
| Search the selected folder for text files with ledger in the name. **Qwen3-0.6B at 2,048 context.** | One `files.search` succeeded, returning `budget_ledger.txt` and `ledger.txt` from the small fixture. | PASS. The model named both matches. This real replay did not exercise the malformed-answer correction path. | `completed`. Native list and model reply appeared separately. | 2 / 0 / 0 | 24.20 |
| Discover installed applications, then open KCalc if you find it. Ask for my approval before launch. **Qwen3-0.6B at 2,048 context.** | One successful `apps.list`. The model never proposed `apps.launch`. No approval arose and no KCalc window existed. | FAIL for the full request. The model asked for confirmation in prose, but the trusted approval flow never started. | `completed` model reply. The desktop explicitly said “No application launch was dispatched.” | 2 / 0 / 0 | 23.21 |
| Open KCalc. Use local application discovery to find its actual ID, then request a launch for KCalc only. Do not select any other app. **Click Deny.** | `apps.list` succeeded. A valid `apps.launch` proposal requested approval for the exact KCalc entry and executable. Audit recorded `user_rejected`, with zero launches. | PASS for denial. KCalc stayed closed and there was no model success claim. | `denied`. Activity read “You declined to launch KCalc” and the discovery result stayed visible. | 2 / 0 / 0 | 114.44 |
| Same KCalc request. **Let the approval expire.** | `apps.list` succeeded. The trusted KCalc approval went unanswered; zero launches. | PASS for authorization. The first run exposed incorrect time-limit guidance. | `timed_out`. The approval cleared, KCalc stayed closed, and the journal showed only the discovery dispatch. | 2 / 0 / 0 | 166.54 |
| Same KCalc request. **Click Approve once.** | `apps.list` succeeded, then exactly one authorized `apps.launch` succeeded. | PASS. KCalc opened in the isolated X11 display and the model reported that it launched. | `completed`. The approval cleared, launch appeared as a separate native result, and the visible window independently confirmed it. | 3 / 0 / 0 | 184.12 |
| Same KCalc request. **Let the approval expire on the final release build.** | `apps.list` succeeded; KCalc stayed closed. No launch was authorized or dispatched. | PASS for authorization and corrected recovery guidance. | `timed_out` with `approval_expired`. The thread advised starting a new task to request another approval. | 2 / 0 / 0 | 163.90 |

The first calculator attempt under concurrent host load did not return
a terminal response before the automation closed the application at its
wait limit. Its journal still showed `planning`, with no native dispatch.
The table records the subsequent serial replay. A transient accessibility
focus failure after the documentation trial occurred before the initial
no-grant prompt was submitted. The application discovery trial had already
reached terminal state before the screenshot encoder failed under the
socket filter. That capture failure did not alter discovery or the journal.
The no-grant and launch cases were replayed. The chat database integrity
and Agent journal integrity and foreign-key checks passed in the closed
acceptance profiles examined.

## Opt-in real-model tests

- `foundation_chat_prompts` passed for Qwen3-0.6B. This is a native test
  with its own deterministic prompts and settings, separate from the chat
  desktop's answer-quality failures.
- `foundation_agent_fits_large_native_observation` passed on Qwen3.5-4B.
  The full synthetic prompt was 5,353 tokens and the preview fit at 1,451.
  The model got the verified count of 80 and still overgeneralized which
  groups contained results. The native observation remains authoritative.
- `local_agent_decode_cancellation` passed on Qwen3.5-4B. Native provider
  return took 138.74 ms after cancellation in this run. That measurement
  excludes desktop event delivery and does not establish a worst case.

## Visual verification

The display size and actual desktop window size were checked independently
of the web content. The 1024 × 768 run exercised the model dialog, keyboard
traversal into model settings, Escape, and saving settings. A separate wheel
scroll moved the model Save settings button from y=756 to y=689, fully into
view. The 1280 × 800
run exercised the light theme, sidebar collapse, bottom composer, and model
switching between the two local GGUFs. The 1600 × 900 run exercised the dark
theme and visible collapsed Reasoning disclosure. The acceptance profiles
were private to the test. Native Wayland rendering and a full keyboard or
screen-reader accessibility audit were outside this run.

| Actual desktop capture | Evidence |
| --- | --- |
| [Legacy limit at 1280 × 800](screenshots/task-01-c/01-legacy-limit-light-1280.png) | The saved 256-token configuration offers an explicit upgrade. |
| [Separate Thinking at 1600 × 900](screenshots/task-01-c/03-thinking-dark-1600.png) | Reasoning disclosure above the answer in dark appearance. |
| [Output exhaustion at 1024 × 768](screenshots/task-01-c/04-thinking-truncated-1024.png) | One-token reasoning warning and direct regeneration affordance. |
| [Model dialog at 1024 × 768](screenshots/task-01-c/06-model-dialog-1024.png) | Settings and action controls in the small viewport. |
| [Model dialog after scrolling](screenshots/task-01-c/28-model-dialog-scrolled-1024.png) | Save settings is visible above the bottom of the small viewport. |
| [Settings at 1024 × 768](screenshots/task-01-c/07-settings-save-1024.png) | Dark is selected and Save settings is visible. |
| [Saved chat at 1280 × 800](screenshots/task-01-c/08-chat-after-restart-1280.png) | Earlier chat restored after restart, with the model unloaded. |
| [Light workspace at 1280 × 800](screenshots/task-01-c/21-light-1280.png) and [collapsed sidebar](screenshots/task-01-c/22-collapsed-1280.png) | Same workspace and composer before and after collapse. |
| [Dark workspace at 1600 × 900](screenshots/task-01-c/23-dark-1600.png) and [at 1024 × 768](screenshots/task-01-c/24-dark-1024.png) | Normal desktop layout at both extremes. |
| [Agent greeting at 1280 × 800](screenshots/task-01-c/09-no-tool-1280.png) | The app distinguishes a model reply from a native result. |
| [Ledger search at 1280 × 800](screenshots/task-01-c/10-ledger-count-1280.png) | The query without an explicit extension included the JSON fixture and excluded the symlink. |
| [Large native file result at 1280 × 800](screenshots/task-01-c/11-large-native-1280.png) | Count, list and truncation are visible in the native result card. |
| [Selected file at 1280 × 800](screenshots/task-01-c/29-selected-file-explicit-1280.png) | Exact-file access succeeds with a scope-appropriate model request. |
| [Model controls during Agent processing](screenshots/task-01-c/27-agent-model-locked-1280.png) and [earlier Agent session](screenshots/task-01-c/12-agent-after-restart-1280.png) | The active model is protected during work, and the saved journal reopens after restart. |
| [Native result after synthesis failure](screenshots/task-01-c/13-native-results-after-failure-1280.png) | A verified search remains visible beside actionable context guidance. |
| [Small-model ledger replay](screenshots/task-01-c/14-small-ledger-replay-1280.png) | The exact reported prompt made a real authorized search in the later replay. |
| [Partial launch request](screenshots/task-01-c/25-small-partial-launch-1280.png) | Model prose asks for confirmation while the execution-based note says no launch was dispatched. |
| [Approval at 1024 × 768](screenshots/task-01-c/16-approval-1024.png), [1280 × 800](screenshots/task-01-c/18-approval-1280.png) and [1600 × 900](screenshots/task-01-c/19-approval-1600.png) | The exact trusted desktop entry, executable and decision controls are present. The mode menu was open in the 1024 capture and overlaps part of the countdown. |
| [Declined launch](screenshots/task-01-c/17-declined-1280.png) and [actual KCalc window](screenshots/task-01-c/20-kcalc-window.png) | The denial and approval trials had distinct native outcomes in the private display. |
| [Expired approval on the final build](screenshots/task-01-c/30-expired-approval-guidance-1280.png) | The terminal thread keeps discovery visible and gives approval-specific recovery guidance. |
| [Small-model answer defects](screenshots/task-01-c/small-model-incorrect-answers.png) | The generated Rust and ownership explanation were incorrect in the live chat. |

## Changed-file inventory

| Area | Files |
| --- | --- |
| Decision validation and task runtime | `crates/agent-core/src/agent/decision.rs`, `crates/agent-core/src/agent/error.rs`, `crates/agent-core/src/agent/runtime.rs` |
| Decision and runtime regressions | `crates/agent-core/tests/agent_decision.rs`, `crates/agent-core/tests/agent_runtime.rs` |
| Desktop approval lease and IPC regression | `apps/desktop/src-tauri/src/agent.rs`, `apps/desktop/src-tauri/src/agent_contract.rs` |
| Desktop reconciliation and interaction gating | `apps/desktop/src/useAgent.ts`, `apps/desktop/src/App.tsx`, `apps/desktop/src/Composer.tsx`, `apps/desktop/src/Dialogs.tsx` |
| Agent result and denial presentation | `apps/desktop/src/AgentView.tsx`, `apps/desktop/src/agent.ts`, `apps/desktop/src/agent.test.mjs` |
| Verification evidence | This report and PNGs in `docs/screenshots/task-01-c/` |

## Remaining limitations and frontend handoff

- A completed generation or Agent task describes the runtime, independent
  of whether the response satisfies the user. Qwen3-0.6B produced an invalid
  Rust example and an inaccurate explanation, and Qwen3.5 produced malformed
  formatting for a syntax explanation. The decision validator recognizes a
  narrow standalone tool-envelope shape. It deliberately leaves ordinary
  prose to the model and never derives authority from it.
- Application launch evidence depends on the native execution record. An
  application catalog, even one that contains the requested target, proves
  only discovery. Keep that distinction visible above or alongside the
  model's text. Outside the isolated test, an accepted OS dispatch alone
  cannot prove the external application finished loading.
- Agent prompts, model answers and private observations remain session-only.
  Earlier entries can show saved execution and audit metadata. An expired
  or revoked session grant requires a new native selection. There is no
  automatic recovery dispatch for a previous side effect.
- The selected-file replay required clearer wording about the exact-file
  grant. The ordinary prompt caused a scope-kind mismatch, and the current
  generic permission guidance can ask the user to select an already granted
  resource. Exact-file and directory arguments must remain distinct in the
  next model-reliability work. The grant's one-hour desktop expiry was not
  waited out. Automated policy tests cover scope expiration and approval
  replay, and the real desktop exercises application approval expiry.
- For the next Claude Code assignment, keep the current design and make the
  verified outcome, work still missing, and next action easier to scan in a
  long thread. Use typed task, execution, approval and grant states for copy.
  Keep a separate visual affordance for the model reply and make loading,
  waiting for the model, and waiting for the user distinct in keyboard and
  screen-reader flows. Recheck focus and visibility of the final action in
  the smallest supported viewport, including with a composer menu open over
  the approval countdown.
- Storage retention, private Agent history, model downloads, model metadata
  inspection, model-reference deletion, and native Qwen tool calling remain
  deferred. Existing user profiles were outside the test boundary.
