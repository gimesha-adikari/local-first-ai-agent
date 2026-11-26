# v0.2.0 Task 06-C — Final hardening

**Verdict: PASS for this task, with the limitations below.** Earlier uncommitted work was preserved. No dependency, model, commit, push, merge, tag or release. Work stopped before Task 06-D.

## Fixes

| Area | Reproduced problem | Change |
| --- | --- | --- |
| Documents-only grounding | LFM wrote the rain poem by copying an earlier ordinary answer from history. | Grounded Chat with no relevant passage now gets a **fixed reply without calling the model** (`no_evidence_answer`): question, exact search ("The text “walrus” wasn't found…") and overview variants. Evidence answers get **no earlier turns** (only the system prompt). Documents-only never adds personal memories. |
| Automatic mode | An explicit document question without evidence was answered as ordinary Chat. | The router adds `refers_to_documents` (document nouns, "according to", citation or source words, follow-ups). Such questions stay grounded and get the fixed reply. Unrelated requests are still ordinary Chat. |
| Follow-ups | "make it shorter" or "explain more" could not see the earlier contextual answer. | A short revision request **re-routes the previous document question** (same intent: question, search or overview), retrieves current evidence from the current selection, and appends the instruction. The saved user text is unchanged. Chains of refinements walk back to the original question. No earlier answer or evidence is reused, so deleted, replaced, deselected or disabled evidence cannot return. |
| Ordinary Chat coverage | Coverage was lost on restart. | Schema 11 `chat_route_coverage` stores it apart from contextual attempts. `Message.routeCoverage` carries it, and history and Continue still treat the answer as ordinary. |
| Model consolidation | Duplicates merged on a sampled fingerprint alone. | The sampled key only nominates candidates. Rows merge only when their full SHA-256 values (`models.content_sha256`) match. The bundled row uses its pinned verified digest. Import compares full digests before reusing a row. Unreadable files stay separate. Hidden rows, files and historical references are kept. |
| Last Chat model | It was not restored. | `model_runtime.last_model_id` is set on each successful load, and `prepare_bundled_model().restore` returns it (or its listed replacement). The UI loads it at launch. An explicit unload is respected. A missing file shows a startup notice and nothing loads. |
| Embedding state | An in-session choice reported "restored at launch". | New `selected` restore state: "Ready · remembered for next launch". |
| Escape in the Knowledge popover | Clicking "All" disables that button, focus drops to the page, and Escape did nothing. | The popover also listens for Escape at document level while open, closes, and returns focus to its trigger. |
| NOTICE | The app heading said 0.1.0. | Now 0.2.0 in `NOTICE` and the `copyright` heading. |

IPC (additive): `KnowledgeSelection` is unchanged. `Message.routeCoverage?`, `BundledModel.restore?: {model, available}` and `EmbeddingModelStatus.restore: "selected"` were added. Schema 11 adds one table and two columns. Downgrading requires the pre-upgrade backup.

## Verification

Intel i5-8500, `CARGO_BUILD_JOBS=2`, existing release cache.

| Check | Result |
| --- | --- |
| Locked workspace tests (release, `tauri/custom-protocol`) | **172 passed, 0 failed, 42 opt-in ignored.** New tests cover refinements, document-reference routing, fixed no-evidence text, persisted routing coverage with refinement chains, full-digest consolidation, and last-model restore with copies and unloading. |
| fmt, locked Clippy `-D warnings` (workspace all targets and `inference --features llama`), `git diff --check` | Passed. |
| Frontend tests / `tsc` / Vite | 61 passed / passed / passed. |
| Real LFM2.5 and Qwen3.5-4B, offline, temperature 0 | `real_refinement_uses_current_evidence_only` and `real_routed_document_chat` passed on both. Refinement answers cited port 8080 with no rain text. LFM wraps facts in brackets ("[8080]"), a known citation-format quirk. |
| Real BGE | Calibration and verified-unit tests passed (2). Restore tests passed (2), including a same-size forged file rejected and forgotten. |
| Full-digest consolidation on real files | A synthetic pair with equal sampled keys but different middles stayed separate. Two real copies of LFM2.5 (bundled and USB) merged after hashing (11.4 s). |
| Final desktop build: release, Xvfb `:97`, X11, `unshare -rn`, disposable XDG profile, synthetic docs (personal database and desktop untouched) | **Original review cases:** "fins run in this selected file" → exact search, 1 match. "what it contain" / "what it do" → partial overview, 4 of 286 passages. Rain poem → ordinary Chat, label kept after restart. **New:** "make it shorter" → shorter cited overview from fresh passages. Documents-only poem → fixed no-evidence reply. "What does the document say about the capital of France?" in Automatic → fixed reply. Escape after "All" closed the popover. Keyboard-only Settings: Tab plus Enter reached Chat & Agent and About. BGE showed "remembered for next launch", then "restored at launch". Qwen restored at launch, and an unloaded state was respected. A missing last-model file showed a notice. Closing the window at 18/285 indexed passages exited in **0.09 s**: no partial index, integrity and foreign-key checks OK, library intact on reopen. |

The desktop check found one defect: the restore code had not been applied (an unasserted source edit). It was fixed and re-verified in the final build. Screenshots: `docs/screenshots/task-v0.2.0-06-c/`.

## Remaining limitations

- Routing cues remain English heuristics. A refinement must be short (≤ 8 words) with no new subject.
- Refinements re-answer rather than edit the earlier answer, so wording can change beyond the requested revision.
- Evidence answers drop conversation history entirely. Pronouns rely on the follow-up rule merging the previous question.
- The first consolidation of large unhashed candidate copies delays bundled-model preparation once (about 11 s for a 730 MB copy on USB).
- LFM2.5 still sometimes formats facts as bracketed pseudo-citations.
- Not re-run: long-term memory profiling and the full Rust acceptance suites (avoided per task constraints).
