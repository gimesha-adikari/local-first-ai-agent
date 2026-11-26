# v0.2.0 Task 06-B — Settings and interface

**Result: implemented and interactively verified, with the limitations below.** Frontend only. No backend changes, dependencies, commits or pushes. Earlier uncommitted work was preserved.

## Changes

- **Categorized Settings** (`Settings.tsx`): General, Chat & Agent, Models, Knowledge & Memory, Privacy & Storage, About & Licenses, with a sidebar that becomes a horizontal tab bar below 760 px. Drafts live in the dialog, so switching categories keeps unsaved General and per-model changes. Unsaved categories show a dot, and a save bar handles General. Advanced parameters stay collapsed, and licenses are under About. The Models entry points open the Models category. The obsolete "settings for newly imported models" control was removed.
- **Models** (`ModelsPanel` in `Dialogs.tsx`):
  - A "Loaded now" banner shows the model, context and threads.
  - Built-in/Imported and Automatic/Custom badges appear in the list and the details.
  - Automatic models say no action is needed. Saving changes makes them custom.
  - Custom models offer "Reset to automatic" (with or without reloading) and show how they differ from the current recommendation.
  - Context and thread changes on a loaded model show a reload notice and "Save and reload".
  - The library refreshes through `list_models` after import, load, reload and reset.
- **Knowledge**:
  - The composer has labeled Ranking and Document context (Automatic / Documents only) controls.
  - Each answer shows a coverage headline: ordinary Chat without evidence, exact search with match or no match, full or partial overview (N of M passages), and requested vs effective ranking mode.
  - Coverage for ordinary-Chat routes is kept in session memory, because the backend doesn't save it.
  - A shared embedding card shows remembered, restoring, restored, unloaded, missing, incompatible and failed states. The copy saying the model must be chosen each session was removed.
- **Fixes**: field inputs no longer stretch, and the knowledge popover fits its content.

## Verification

- 61 frontend tests passed (4 new); TypeScript and the Vite build passed.
- Built the release binary, then drove it under Xvfb `:97` with XTEST, `GDK_BACKEND=x11`, no Wayland or D-Bus variables, `unshare -rn` (no network) and a disposable XDG profile. The personal database and desktop session were not touched.

| Path | Observed |
| --- | --- |
| Categories | Drafts survived category switches: a Qwen temperature edit plus an unsaved Dark theme, both dotted, then saved. |
| Models | Import of Qwen3.5-4B listed it as Imported · Automatic. Saved custom → Custom. Switch → banner updated. "Reset to automatic and reload" → Automatic, reloaded. Context edit → reload notice, and validation blocked output ≥ context. |
| Knowledge, using LFM2.5 | The poem answered as ordinary Chat. "fins run…" → exact search, 1 match. "search for walrus" → no match. "what it contain" → partial overview, 4 of 11. Semantic mode without indexes → "Semantic requested · Keyword used". Documents only → "answer limited to the documents". |
| Embedding | Chosen → Ready. Relaunch → restored. Unload + relaunch → Unloaded. File moved + relaunch → not found. Same-size forged file → "no longer matches", and the choice was forgotten. |
| Themes and widths | Graphite dark and warm light both checked. A 440 px window was checked for Settings, the composer and the popover. |

Screenshots are in `docs/screenshots/task-v0.2.0-06-b/`: Models in light, dark custom configuration, coverage headlines, partial overview, missing embedding file, and narrow Settings.

## Limitations and backend items for 06-C

- With more than one model listed, no chat model loads at launch. Consider remembering the last loaded model.
- Documents only: LFM wrote the poem anyway, copying an earlier ordinary-Chat answer from history. Grounding compliance needs prompt or history handling.
- The backend reports `restore: "restored"` after an in-session choice too, so the UI says "loads automatically at launch".
- Ordinary-Chat coverage is not saved, so it disappears after a restart.
- `third_party/software-notices/NOTICE` still says 0.1.0.
- The final release build includes every fix above, and its narrow popover was re-checked. Earlier screenshots come from intermediate builds. Escape didn't close the popover in one Xvfb attempt; I didn't investigate further.
- Not exercised: long answers at narrow widths, keyboard-only navigation of Settings, and the Agent access dialog from Settings.
