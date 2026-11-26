# Task 04-B frontend report

Frontend-only follow-up to the [Task 04-A backend handoff](task-04-a-backend-handoff.md).
Nothing was committed or pushed. No Rust, migration, capability or
security code changed. **Real** marks results from the rebuilt bundled
`.deb` with the real LFM2.5 model. **Synthetic** marks unit tests and the
dev-only IPC fixture.

## Implemented

| Area | Change | Files |
| --- | --- | --- |
| Settings layout | Settings now has these sections in order: **Appearance**, **Responses**, **Model**, **Privacy and files**, a collapsed **Advanced settings** section, then the built-in model's license. Responses holds Enter-to-send and a plain-language summary of the settings for newly imported models (“Replies up to 768 tokens · reads 2,048 tokens of conversation · Thinking off”), with **Use recommended** that changes only the form. Model shows the active model, its role, capability chips, guidance and **Manage models…**. Advanced holds output limit, context length, temperature, top-p, top-k, CPU threads, Think by default and the output-budget explanation. Every control maps to an existing preference or `save_model_settings` field. | `Dialogs.tsx`, `styles.css` |
| One recommendation | The static 768-token “recommended values” (`recommendedSettings`, `withRecommendedSettings`, `differsFromRecommended`) is removed. `recommendedChatSettings(trainedContext)` mirrors `recommended_chat_settings`: context is the trained context capped at 8,192, output is half of that up to 4,096, Thinking is off and sampling uses the backend defaults. Models uses the backend's own result for the loaded model. Settings uses the general form (8,192 / 4,096) for new-model defaults. 768 remains only as `shortOutputLimit`, a warning threshold for the low-limit banner and hints, and is no longer labelled as recommended. | `chat.ts`, `Composer.tsx`, `Dialogs.tsx`, `App.tsx` |
| Models dialog | The detail view shows the friendly name and role, capability chips, guidance, facts, then **Recommended settings** with **Use recommended and reload**, **Use recommended**, or **Review in Advanced settings**. Advanced settings is collapsed, and opens for review or an unsaved draft. Saved settings, explicit save and discard, the reload-only rules for context length and threads, and **Save and reload now** are unchanged. | `Dialogs.tsx` |
| Friendly names | The new `models.ts` maps file names to display profiles. `LFM2.5-1.2B-Instruct*` becomes “LFM2.5 1.2B”, role “Built-in lightweight assistant”. `Qwen3.5-4B*` becomes “Qwen3.5 4B”, role “Advanced: Agent and Thinking”. Other files show their file name without `.gguf`. The status pill, composer, Models, details drawer and per-reply model label no longer show GGUF `general.name`, which is a commit hash for LFM2.5. | `models.ts`, `App.tsx`, `Composer.tsx`, `ChatView.tsx`, `DetailsDrawer.tsx` |
| First launch | The bundled verification state is tracked as checking, ready, missing, corrupted or unavailable. The new-thread view shows each state: “Checking the built-in assistant…” while it is hashed, “Starting LFM2.5 1.2B, the built-in assistant…” while it loads, then the suggestions. A missing resource shows “Import a model to begin”, explaining that this copy doesn't include the built-in assistant. A corrupted resource shows a persistent error callout with Reinstall / Import guidance. The Models library shows the same states. That callout replaces the generic dismissible error banner. The composer status follows the same states. No onboarding screens were added. | `App.tsx`, `Dialogs.tsx` |
| Capabilities | The chips come from `supportsThinking` / `supportsAgent` for the loaded model. For an unloaded model they come from the known profile, marked “Expected… Confirmed when it loads”. For LFM2.5: Chat ✓, Thinking ✗, Agent ✗. The guidance explains that LFM2.5's multi-step tool reliability hasn't been established and says to select or import Qwen3.5-4B for Agent tasks and Thinking. It appears in Models, Settings, under the Chat suggestions, and in Agent mode. The disabled Agent mode hint reads “LFM2.5 1.2B supports Chat only. Agent needs Qwen3.5-4B.” Agent stays gated on `supportsAgent`. No LFM Agent compatibility and no automatic routing were added. | `models.ts`, `App.tsx`, `Dialogs.tsx` |
| Qwen3-0.6B | Its guidance entry and the empty-state recommendation are removed. | `Dialogs.tsx`, `App.tsx` |
| License access | **LFM2.5 1.2B license and notices** appears in Settings and in Models for the built-in model. It gives the license name and scope and states the commercial-use restriction: entities at or above US$10 million annual revenue need a separate agreement, non-profits are exempt for non-commercial or research use, and redistribution must include the license. It has an expandable **Full license text**. The text is imported at build time from `third_party/model-licenses/.../LICENSE` (`?raw`, about 10 KB), so it works offline and with no new IPC. `vite.config.ts` allows that folder for the dev server only. | `Dialogs.tsx`, `vite.config.ts` |
| Privacy copy | Rewritten in plain language with the same facts: local inference, no model downloads, unencrypted SQLite, read-only expiring file access that ends on close, file content in saved answers, unsaved raw previews, and deletion limits. | `Dialogs.tsx` |
| Dev fixture | New scenarios: `?fixture=firstlaunch` (only the bundled model, auto-load, commit-hash `general.name`), `corrupted` and `nobundle`. The default scenario now also lists LFM2.5 as an unused bundled model. | `dev/fixtures.ts` |

## Checks

| Check | Result |
| --- | --- |
| `npm test` | 41 passed, 0 failed. New tests cover the unified recommendation formula, friendly names, the dropped Qwen3-0.6B profile, backend flags taking precedence over profiles, and guidance copy |
| TypeScript (`tsc --noEmit`) | Passed |
| Vite production build | Passed. JS 530 kB (161 kB gzip), with the existing >500 kB advisory. Up from 513 kB, partly due to the embedded license text |
| Fixture exclusion | `installFixtures`, `mockIPC`, `Qwen3-0.6B-Q4_K_M`, `nobundle`, `bundledScenarios` and the fixture commit hash are absent from `dist/assets/index-*.js` |
| `git diff --check` | Passed |
| Bundled package | Built once: `tauri build --config src-tauri/tauri.bundled-model.conf.json --bundles deb -- --locked`, `nice`/`ionice`, one Cargo/CMake job, offline, existing caches and staged resource. 113 s, 690 MiB `.deb` |
| Fixture review | In the dev fixtures (**synthetic**), first launch auto-loaded, Settings and Models laid out correctly, license text loaded, recommended-and-reload flowed through, and the corrupted and missing states rendered |

## Real verification

The `.deb` was extracted to a temporary root. Each run used a fresh
temporary profile (`HOME` and XDG directories) inside `unshare -r -n` (no
network), with a private Xvfb display and D-Bus/AT-SPI session. The UI was
driven through AT-SPI actions. The user's real database was not touched.

| Scenario | Observed (**real**) |
| --- | --- |
| First launch | The bundled model was verified, registered and auto-loaded with no import. The status pill read “LFM2.5 1.2B” about 4.7 s after the window appeared. The commit hash was not shown anywhere. The built-in lightweight assistant note and Qwen3.5-4B guidance were shown under the suggestions. |
| Offline Chat | “Write a four-line poem about rain” produced a complete four-line poem in about 2 s, labelled “LFM2.5 1.2B”. The database held one `user` and one `assistant` message, both `complete`. `integrity_check` returned ok. |
| Simplified settings | Appearance, Responses (Custom: 768 / 2,048, **Use recommended**), Model (LFM2.5 1.2B, Chat ✓ Thinking ✗ Agent ✗, guidance), Privacy and files, collapsed Advanced settings, and the license section with the commercial-use restriction. |
| Capability guidance | Models showed the chips, the “Want Agent tasks or Thinking?” callout and the recommendation diff (Context 4,096 → 8,192 · Output limit 768 → 4,096). |
| Apply recommended | **Use recommended and reload** unloaded, saved and reloaded the model. The notice read “Settings saved. LFM2.5 1.2B reloaded with 8,192 tokens of context.” Loaded context showed 8,192 of 128,000 trained and the panel read “In use”. The database row held `{contextLength 8192, maxTokens 4096, threads 4, temperature 0.7, topP 0.8, topK 20, enableThinking false}`. |
| Missing resource | With the GGUF removed, the app started with no models and showed “Import a model to begin”, explaining the built-in assistant isn't included. |
| Corrupted resource | With a 1 MB file in place of the GGUF, the app started, registered nothing, and showed “The built-in assistant couldn't be verified” with Reinstall / Import guidance. Models showed the matching “failed verification” callout. |

Typing into the composer through XTest didn't reach the WebKit view
without a window manager, so the Chat message was sent by clicking a
suggestion and then **Send message**. Both are ordinary UI controls. The
reply and its persistence were real.

Screenshots (**real**, 1280 × 800, `docs/screenshots/task-04-b/`; the X marks the synthetic pointer):

| Screenshot | Shows |
| --- | --- |
| [01-first-launch](screenshots/task-04-b/01-first-launch.png) | Auto-loaded built-in model, friendly name, capability note |
| [02-offline-chat](screenshots/task-04-b/02-offline-chat.png) | Real offline Chat reply |
| [03-settings](screenshots/task-04-b/03-settings.png) | Simplified Settings |
| [04-models-capabilities](screenshots/task-04-b/04-models-capabilities.png) | Capabilities, Qwen3.5-4B guidance, recommendation diff |
| [05-recommended-applied](screenshots/task-04-b/05-recommended-applied.png) | After **Use recommended and reload** |
| [06-missing-model](screenshots/task-04-b/06-missing-model.png) | Missing bundled resource |
| [07-corrupted-model](screenshots/task-04-b/07-corrupted-model.png) | Corrupted bundled resource |

## Unresolved issues

- **Existing-user preservation was not re-run in the real UI.** The frontend never changes model rows or order. It only appends the bundled reference and auto-loads when `autoLoad` is true and nothing is active. Task 04-A verified the backend side. Task 05 should confirm this with an existing profile.
- **Qwen3.5-4B capability display is synthetic only.** No Qwen model was loaded in this task. The chips read the same backend flags.
- **Agent-mode guidance** (the disabled mode hint and the Agent-mode callout) was checked in fixtures and unit tests, not in the packaged app.
- **Recommended defaults set CPU threads to 4**, matching the backend's `recommended_chat_settings`. A user with custom threads sees “Custom” and changes threads only by choosing **Use recommended**.
- **License access is in-app text.** The full LFM license is embedded in the frontend bundle. The installed `LICENSE` and `NOTICE.md` files beside the model are not opened from the UI, because that would need new file-opening permissions.
- **Small viewports.** At 800 × 600 (fixture check) the Models library's “Which model?” box sits below the fold and needs scrolling.
- Bundle size is 530 kB, above Vite's 500 kB advisory. Code splitting was not attempted.
- Nothing is committed. Task 05 owns full regression.
