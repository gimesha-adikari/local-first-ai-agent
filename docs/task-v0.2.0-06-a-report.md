# v0.2.0 Task 06-A — Backend corrections

**Result: implemented and verified at the Rust, storage and native-model level. The UI was smoke-launched only.** Earlier uncommitted work was preserved. Nothing was committed, pushed or released. No dependency or model was added.

## Root causes

| Review case | Cause |
| --- | --- |
| Searching for "run" returned unrelated PDF pages | Dense retrieval returned the top passages by cosine with no relevance floor. Every indexed page always came back. |
| "fins run in this selected file" found nothing useful | Chat keyword ranking ORed every term, including "this", "selected" and "file". Nothing interpreted the request as a search. |
| "what it contain" / "what it do" returned no evidence | Almost every word is a stopword, so no passage could match. Nothing read the selected document directly. |
| A rain poem got a "no document evidence" refusal | Any selection forced grounded mode, and an empty result told the model to refuse. |
| Duplicate LFM2.5 in Models | The profile had the bundled row and an imported copy on the BACKUP drive at a different path. Model rows had no content identity. |
| Imported copy labelled built-in | The frontend set "built-in" from a file-name pattern. |
| Settings, embedding model and unload choices | Per-model settings had no automatic/custom state. The embedding choice was not persisted, and an explicit unload was not remembered. |

## Changes

**Retrieval relevance.** `agent_core::semantic` adds `RELEVANCE_FLOOR = 0.56` and `RELEVANCE_MARGIN = 0.15`. Semantic search keeps hits at or above the floor and within the margin of the best hit. Hybrid search applies only the floor before reciprocal rank fusion, so exact keyword matches keep their rank. Natural-language keyword ranking now ignores document words and keeps a passage only when it shares at least half of the significant terms. Library keyword search keeps its literal AND behaviour. When nothing qualifies, no passages are returned.

**Intent routing.** The new `agent_core::chat_route` combines four bounded signals instead of matching phrases:
- small cue lexicons (search verbs, summary words, creative and social words)
- content terms left after removing function, generic and document words
- references to the selected documents
- the previous document question, for short follow-ups

A search verb counts only at the start of a request or after a polite lead-in. A one-edit typo counts only when the request also names the documents. The host then checks retrieval relevance.

| Intent | Evidence | Prompt |
| --- | --- | --- |
| `search` | FTS phrase match in the selection, document order, up to 4 hits. No dense model needed. | The routed search text replaces the raw request, which avoids typos such as "fins". |
| `overview` | Passages read directly: round-robin across documents, then evenly spaced, starting with the first passage, up to 4. | States "Excerpts supplied: N of M passages" and asks the model to say when the overview is partial. |
| `question` | Relevance-filtered retrieval. | Unchanged Task 05 grounding instruction. |
| `general` | None. Automatic grounding only, when no passage qualifies. | Ordinary Chat. |

`grounding: "documents"` keeps the earlier strict behaviour. Under automatic grounding, an unusable dense model or index ranks by keyword instead, and coverage reports it in `requestedMode`. Follow-ups re-retrieve from current documents. Contextual answers are still left out of later prompts, so deleted, replaced or deselected evidence cannot return through history.

**Model management (schema 10).** `010_model_management.sql` adds `models.settings_mode`, `models.content_key` and a single-row `model_runtime` table.
- **Content identity.** `content_key` is the file size plus SHA-256 of the first and last MiB. At startup, readable rows get a key. Rows with the same key collapse into one listed row: the bundled row first, otherwise the most recently used. Hidden rows, their files and attempts that reference them stay intact. An automatic survivor adopts the latest custom settings of its copies. Importing a copy returns the existing row.
- **Settings classification.** The upgrade marks rows that still hold a recommendation (2048/4096/8192) as automatic and all others as custom.
- **Automatic settings.** Automatic models get a fresh recommendation for the file and machine on every load and model switch. Saving different settings makes them custom. `reset_model_settings` restores automatic.
- **Remembered choices.** The verified embedding path is remembered in Rust. Each launch restores it on the embedding worker after full checksum and metadata verification. A missing file keeps the choice for a later launch. A mismatched file is forgotten. Explicitly unloading the embedding or chat model is remembered, and the bundled model is not auto-loaded afterwards.

## IPC changes

| Contract | Change |
| --- | --- |
| `send_message` / `regenerate` `options.knowledge` | Optional `grounding: "auto" \| "documents"`, default `auto`. |
| `KnowledgeCoverage` (event and saved `Message.context`) | Optional `intent`, `grounding`, `overview[{documentId, suppliedChunks, totalChunks}]` and `requestedMode`. Older saved attempts omit them. |
| Behaviour | Under `auto`, the `embedding_*` preparation errors no longer reject. `general` turns are ordinary attempts: coverage is sent live but not saved, and the answer stays in later prompts. |
| `ModelReference` | Adds `bundled` and `automaticSettings`. |
| New commands | `list_models()` returns the deduplicated list. `reset_model_settings({modelId})` returns a `ModelReference`. Both are granted to the main window only. |
| `load_model` / `save_model_settings` | Signatures unchanged. Settings equal to the saved ones mean "use this model's settings". Different settings become custom. |
| `choose_model` | Returns the existing row for a copy of listed weights. New imports start automatic. Global defaults no longer seed new imports. |
| `prepare_bundled_model` | `autoLoad` is true only when the bundled model is the sole listed model and the user has not explicitly unloaded. |
| `knowledge_embedding_status` | Adds `restore`: `not_remembered`, `pending`, `restored`, `unloaded`, `missing`, `incompatible` or `failed`. |

Minimal frontend updates: types, built-in label from `bundled`, list refresh after load and import, an "Answer only from documents" switch, a "Use automatic settings" link, non-blocking dense notes under `auto`, intent coverage lines, and restore polling.

## Verification

Intel i5-8500 host, `CARGO_BUILD_JOBS=2`, release cache reused. Dependencies did not change and only the cached native objects were linked.

| Check | Result |
| --- | --- |
| Locked workspace tests, release, `tauri/custom-protocol` | **168 passed, 0 failed, 40 ignored** (baseline 151 / 35). New: router, relevance, search, overview, follow-up, prompt, schema 10, deduplication, settings mode, runtime choices, restore states and host routing. |
| `cargo fmt --check`, locked Clippy `-D warnings` (workspace all targets, and `inference --features llama`), `git diff --check` | Passed. |
| Frontend tests / `tsc` / Vite build | **57 passed** / passed / passed. Vite kept its existing chunk-size warning. |
| BGE calibration (`verified_bge_relevance_floor_*`, real BGE small en f16) | Passed. 11 positive and paraphrased queries scored 0.588–0.797 on the right passage. 7 unrelated queries peaked at 0.371–0.545. Margins are about 0.03 on both sides. |
| `real_routed_document_chat`, LFM2.5 1.2B and Qwen3.5-4B, temperature 0 | Passed on both. Search quoted the matching passage with [S1]. Both overviews described the orchard guide with [S1]/[S2] citations. The poem was ordinary four-line verse. On one overview, Qwen misread "closes at seven" as 17:00. |
| BGE restore (`restore_reverifies_the_remembered_bge_file`) | Restored in 2.8 s from the USB drive. A same-size forged file returned `incompatible` and was forgotten. Model-free missing, unloaded and unremembered states passed. |
| Real profile, disposable copy (deleted afterwards) | Upgraded to schema 10. Qwen3-0.6B (768-token output) stayed custom. The imported BACKUP LFM2.5 collapsed into the bundled row. |
| Automatic settings on real GGUFs | LFM2.5 (128k trained) and Qwen3.5-4B (262k trained) both got 8,192 context, 4,096 output and 4 threads, keeping Thinking from the saved row. |
| Linux release build and isolated launch under Xvfb | Built in 54 s. First launch: schema 10, bundled row automatic with a content key, auto-loaded. After seeding an explicit unload and a remembered BGE path, the second launch showed "No model loaded" and kept the BGE choice. The first launch inherited the Wayland session, so its window may have appeared on the host desktop for about 18 s. |

## Remaining limitations

- Routing cues and the floor are English-only and calibrated on 18 queries with BGE small en. Unicode61 FTS has no stemming, so "run" does not match "running".
- Overviews sample at most 4 passages (the citation slots). Large documents get a truthful partial overview, not a full summary.
- `general` coverage is live only. A follow-up such as "make it shorter" after an overview or search cannot see the earlier answer.
- Content identity is sampled, not a full hash. Files that cannot be read now, such as on an unmounted drive, are not deduplicated until they can be read.
- The Models, Knowledge and Chat UI paths were not exercised interactively. Categorized Settings and final UI work remain for Task 06-B.
