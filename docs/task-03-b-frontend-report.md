# Task 03-B frontend report

Frontend-only follow-up to the [Task 03-A backend handoff](task-03-a-backend-handoff.md).
Nothing was committed or pushed. No Rust, migration, permission-policy or
PDF-isolation code changed. **Real** marks results from the rebuilt Linux
release binary with actual local models. **Synthetic** marks unit tests,
the dev-only IPC fixture and static render checks.

## Changes implemented

| Area | Change | Files |
| --- | --- | --- |
| Continuation | The newest Chat reply that ended `truncated` with `token_limit` is checked with `chat_continuation_remaining`. When eligible, the cut-off notice shows **Continue generating** with the remaining count. `continue_chat` streams into the same assistant message by ID. `started` replaces the attempt with the saved prefix and `delta` appends only new text. Stop uses `stop_generation`. Typed errors such as `context_limit`, `no_model` and `invalid_config` for legacy, exhausted or ineligible replies get specific inline copy and hide the button. Regenerate stays a separate action. The composer's one-turn output override, when set, is sent as the continuation `maxTokens` ceiling. | `chat.ts`, `ChatView.tsx`, `ConversationView.tsx`, `unified.ts`, `App.tsx`, `icons.tsx` |
| Output budget | `stats.outputBudget` is typed. A reply shows “N of M output tokens fit after the prompt and history…” only when the effective budget was below the requested ceiling. The cut-off notice uses the latest segment's effective budget, not the cumulative token count. | `inference.ts`, `chat.ts`, `ChatView.tsx` |
| Recommended settings | The Models dialog calls `recommended_chat_settings` when it opens and shows a read-only panel for the loaded model with context, output, threads and Thinking. It lists changes from the saved values, explains the ceiling and reservation rules, and notes the trained-context clamp or an unchecked model. Nothing is saved implicitly. The explicit actions are **Save and reload model** (unload, save, load, because the host rejects context or thread changes while a model is loaded), **Save recommended settings** when no reload is needed, and **Review in the form first**. A draft that changes context or threads on a loaded model offers “Save and reload now”. The low-limit banner now opens Models instead of silently writing 768. Model errors now appear near the top of the dialog. | `Dialogs.tsx`, `App.tsx`, `chat.ts`, `styles.css` |
| PDF results | `files.read` results that have a validated `pages` array render as a PDF card with “Pages a–b of n”, extracted page count, size, per-page text as inert plain text, and per-page clipping. Separate warnings cover unread pages, missing page text and clipped text. A footer says the card shows document text, not a model summary. Malformed or out-of-range shapes fall back to the raw view. TXT keeps the existing card. Under a model answer that follows a partial PDF read, a note states the pages it can rely on. | `agent.ts`, `NativeResults.tsx`, `AgentView.tsx`, `styles.css` |
| PDF errors | Fixed guidance for `pdf_encrypted`, `pdf_no_text`, `pdf_invalid`, `pdf_unsupported`, `pdf_limit`, `pdf_unavailable` and `invalid_arguments`, in failure cards and turn outcomes. | `agent.ts`, `NativeResults.tsx` |
| File search | The card shows the returned count and the applied filter: “Extension filter: .pdf (final suffix, any letter case)” or “No extension filter”. | `agent.ts`, `NativeResults.tsx` |
| Markdown | `remark-gfm` 4.0.1 (exact) is enabled on the existing offline options. Tables render inside a focusable horizontal scroll region. Skipped HTML, blocked images and the inert URL transform are unchanged, so GFM autolinks stay plain spans. | `markdown.ts`, `styles.css`, `package.json`, `package-lock.json` |
| Conversation metadata | Opening a conversation no longer fetches journals for up to 24 Agent turns. Saved state, error and omission counts come from `UnifiedTurn`. A journal loads only for turns whose native results are still held, when the details drawer opens, or from a new **Show steps** link. The “Loading saved activity…” placeholder is gone. | `App.tsx`, `unified.ts`, `AgentView.tsx` |
| Dev fixture | The dev-only fixture handles the three new commands. A prompt that contains “truncate” yields a continuable reply. Production bundles still exclude it. | `dev/fixtures.ts` |
| Tests | New `document.test.mjs` and an updated expectation in `agent.test.mjs` | |

`remark-gfm@4.0.1` depends on `unified@^11`, `remark-parse@^11` and
`mdast-util-gfm@^3`, the versions `react-markdown@10.1.0` already uses. It
declares no engine or peer constraint. `npm ls` shows the shared tree deduplicated.

Two confirmed defects found during verification were fixed:

- The first implementation saved settings before reloading. The host rejected that with “Unload the model before changing context length or CPU threads”. The error appeared below the visible part of the dialog, so the button seemed inert. The action now unloads first and errors are shown near the top.
- The existing Jump-to-latest arrow and the new Continue arrow pointed up. Tailwind's `rotate-180` and the app's own `.rotate-180` both applied, cancelling each other out. Both buttons now use a dedicated down-arrow icon.

## Test and build results

| Check | Result |
| --- | --- |
| Frontend tests (`npm test`) | 39 passed, 0 failed (10 new in `document.test.mjs`) |
| TypeScript | Passed |
| Vite production build | Passed. The JS bundle is 513 kB (155 kB gzip). Vite's 500 kB advisory warning appears, partly because of the GFM parser |
| Fixture exclusion | Fixture-only strings (`installFixtures`, `mockIPC`, fixture model names, fixture continuation text) absent from `dist/assets/index-y0rDp9tn.js` |
| `git diff --check` | Passed |
| Linux release executable | `tauri build --no-bundle` with `nice -n 15 ionice -c3 taskset -c 0`, one Cargo/CMake job, offline. Passed in 76 s. The build ran twice: once after the planned changes, and once more after the two verification fixes above. |

Synthetic coverage in `document.test.mjs`:

- Eligibility for continuation.
- Same-ID updates without duplicated text, including a later turn left untouched and a stray delta ignored.
- Thinking that resumes across segments.
- Continuation error copy.
- The effective-budget note.
- Recommended-setting diffs.
- GFM tables, invalid delimiter rows and the retained link, HTML and image restrictions.
- PDF validation, coverage, warnings and the answer scope note.
- Search filter labels.
- PDF error guidance.
- On-demand journal selection.

A static render of a seven-column GFM table at a 1024 × 768 viewport, using
the production CSS, wrapped to fit at the normal thread width (716 px region,
no overflow). In a 212 px container it scrolled inside its region (560 px
content), and the document never overflowed (**synthetic**). In the dev
fixture, a continuable reply took two continuations with one message, the
count fell from 3 to 2 to 1, and the budget note appeared (**synthetic**).

## Real verification

All runs used the final release binary in a user and network namespace
(`unshare --user --net`, no default route), with a private Xvfb display,
D-Bus session and temporary XDG profile under the session scratch directory.
The user's real database was never opened. The models were the existing
local `Qwen3-0.6B-Q4_K_M.gguf` and `Qwen3.5-4B-UD-Q4_K_XL.gguf` with four
threads and Thinking off. The UI was driven through AT-SPI and XTest. SQLite
was read only after the app closed, because the app holds an exclusive lock.

| Scenario | Model and setup | Observed (**real**) |
| --- | --- | --- |
| Truncation | 0.6B, 2,048 context, saved output 48 | Reply saved `truncated` / `token_limit`. The notice offered **Continue generating · 3 left** beside Regenerate. |
| Continuation | Same message, two Continue clicks | The count fell to 2 left, then 1 left. The UI text and database showed one user and one assistant message (ID 2). The original prefix was intact and new text was appended. Cumulative `generatedTokens` was 144, three 48-token segments. `outputBudget` {requested 48, effective 48, context 2,048}. No extra turn. Regenerate stayed separate. |
| Cancellation | New profile, one-turn ceiling 1,024 via Response options, Stop about 5 s into a continuation | The UI showed “Stopped”, about 1.1 s from click to settled UI. The same assistant message was saved `cancelled` with the received suffix (196 cumulative tokens, 883 characters). The saved model limit stayed 48. |
| Recommended settings | 0.6B loaded at 2,048 / 48 | The panel showed 8,192 context, 4,096 output, 4 threads, Thinking off, and the change list. **Save and reload model** reloaded the model with “8,192 of 40,960 trained” loaded context. The panel then read “Saved settings match the recommendation”. The database held `{contextLength 8192, maxTokens 4096, threads 4, enableThinking false}`. The other model's settings were untouched. |
| PDF-only search | 4B, 4,096 context, granted `pdf-fixture` folder: `01-orchard.pdf`, `pdf-notes.zip`, `report.pdf.zip`, `pdf-notes.txt`, `readme.txt` | One `files.search` succeeded. The card showed “1 file returned · Extension filter: .pdf”, listing only `01-orchard.pdf`. The ZIP and TXT lookalikes were excluded. The model then had one proposal rejected and the turn **timed out at the default 300 s**, with no written answer. The native result stayed visible with timeout guidance. |
| Read and summarize a two-page PDF | Follow-up in the same thread | One `files.read` returned **“Pages 1–2 of 2 · 2 pages extracted · 1.8 KiB”**, with Page 1 and Page 2 text and no coverage warning, which is correct for full coverage. The model answer, labelled as interpretation of native results, accurately quoted both pages with page numbers. 140 s, `completed`. |
| Follow-up without rereading | “Use the earlier extracted pages without rereading it” | **Zero** native executions in the journal. The answer correctly restated both points. 75 s, `completed`. |
| Persistence | Closed profiles | `integrity_check` ok, no foreign-key violations. Journal: task 1 `timed_out` (search succeeded), task 2 `completed` (read), task 3 `completed` (no tools). |

The PDF replay is **partially successful**. Coverage, reading, summary and
non-rereading follow-up passed. The search step found the right file but
the 4B model did not finish that turn within the time limit. As instructed,
it was not retried. The partial-coverage warning and the answer scope note
did not trigger in the real run, because the reader covered the whole
document. They are covered by synthetic tests only.

Representative screenshots (**real**, 1280 × 800, `docs/screenshots/task-03-b/`):

| Screenshot | Shows |
| --- | --- |
| [02-truncated-continue](screenshots/task-03-b/02-truncated-continue.png) | Cut-off notice with Continue generating (3 left), Regenerate and the low-limit banner |
| [04-stopped](screenshots/task-03-b/04-stopped.png) | Continuation stopped and saved in the same reply |
| [05-recommended-settings](screenshots/task-03-b/05-recommended-settings.png) | Recommendation panel with changes and explicit actions |
| [06-recommended-applied](screenshots/task-03-b/06-recommended-applied.png) | After Save and reload: 8,192 loaded context, settings match |
| [07-pdf-search](screenshots/task-03-b/07-pdf-search.png) | PDF-only search with the applied extension filter, then the timeout |
| [08-pdf-read](screenshots/task-03-b/08-pdf-read.png) | Native PDF card “Pages 1–2 of 2” and the separate model answer |
| [09-pdf-followup](screenshots/task-03-b/09-pdf-followup.png) | Follow-up answered without a new native read |

## Unresolved issues

- **The real search turn timed out.** Qwen3.5-4B took the full 300 s on “Find PDF files in the selected folder” after a rejected proposal, although the native search was correct. The turn followed a long 0.6B Chat reply in the same thread. It was not retried.
- **Partial-coverage presentation is synthetic only.** The two-page fixture was fully read, so the omitted-page, clipped-page and answer-scope notes were not seen with a real model. A real multi-page (more than four pages) PDF run is still needed.
- **Continuation after a context-limit failure** was not reproduced in the real UI. The inline `context_limit` copy is covered by unit tests, and Task 03-A verified the backend behavior.
- **Two different “recommended” concepts coexist.** The existing “Restore recommended values” link (sampling defaults with a 768-token output) and the low-limit banner threshold of 768 remain next to the new 8,192 / 4,096 recommendation. This belongs to the Task 04-B settings redesign.
- **Notice wording.** After a reload the notice reads “reloaded with a 8,192-token context”. This is minor copy, left unchanged to avoid another rebuild.
- **Bundle size.** The production JS is 513 kB, above Vite's advisory 500 kB. GFM is part of that. Code splitting was not attempted.
- **Real window size.** The real window was fixed at 1280 × 800. Table overflow at 1024 × 768 was checked with a static render and the production CSS, not in the packaged app.
- Nothing is committed. Task 03-A and this work remain uncommitted in the working tree. Task 05 owns full regression.
