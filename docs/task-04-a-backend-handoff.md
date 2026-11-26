# Task 04-A backend handoff

LFM2.5-1.2B-Instruct is the bundled default model for Linux packages.
Chat works offline on first launch. Agent mode stays **disabled** for
LFM2.5 because multi-step scoped file workflows were not reliable (see
[verification](task-04-a-verification.md)). Qwen3.5-4B remains the
optional advanced model for Thinking and Agent tasks. Nothing was
committed or pushed.

## Bundled model

| Item | Value |
| --- | --- |
| File | `LFM2.5-1.2B-Instruct-Q4_K_M.gguf`, 730,895,168 bytes, `general.file_type` 15 (Q4_K_M), architecture `lfm2` |
| SHA-256 | `b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5`, equal to the LFS object ID on Hugging Face |
| Provenance | `LiquidAI/LFM2.5-1.2B-Instruct-GGUF`, revision `8ed288026e23958ad9dfa92d53ed773a8eee7125`, unmodified |
| License | LFM Open License v1.0, stored at `third_party/model-licenses/LFM2.5-1.2B-Instruct/LICENSE` with a `NOTICE.md` |
| Package path | `/usr/lib/local-first-ai-agent/models/` beside `LFM2.5-1.2B-Instruct.LICENSE` and `.NOTICE.md` |

The model license is separate from the application license, which is
still undecided. It permits redistribution with the license text. It
does not license commercial use by an entity with US$10 million or more
in annual revenue (section 5). The notice documents this.

Packaging is opt-in. The default `tauri build` and CI need no model.
`scripts/stage-bundled-model.sh <path>` verifies size and SHA-256 and links
the file into the ignored `apps/desktop/src-tauri/resources/models/`.
`tauri.bundled-model.conf.json` then builds a `.deb` that contains the
GGUF as a resource file, not inside the executable. No weights or
developer paths are in Git.

## Discovery and first launch

- New host module `apps/desktop/src-tauri/src/bundled_model.rs` pins the
  resource path, size and SHA-256. It checks size, then streams SHA-256
  on a blocking thread. A `bundled-model.verified` stamp beside the
  database keys path, size and modification time, so unchanged files are
  not rehashed. A missing resource returns `missing`. A wrong size,
  checksum or file type returns `corrupted`. Neither is registered, and
  neither blocks startup.
- Schema 5 (`005_bundled_model.sql`) adds a nullable `models.bundled_resource`
  with a partial unique index. `SqliteStore::register_bundled_model`
  inserts the row with `last_used_at = 0`, so it is listed after
  existing models. It reuses a manual import of the same path and
  follows a relocated resource. It never changes another row's
  settings or order. A new row gets the user's new-model defaults with
  4096 context and Thinking off.
- `auto_load` is true only when no other model exists. The UI then loads
  the bundled model. Existing users keep their selection and settings.

## Agent compatibility

The baseline gate in `crates/inference/src/llama.rs` required Thinking,
`enable_thinking` and ChatML markers. The local copy retained it. LFM2.5
renders ChatML but has no Thinking switch, so it was rejected.

- `ModelInfo.supports_agent` is now separate from `supports_thinking`.
  `classify_agent_format` recognizes the unchanged Qwen gate
  (`QwenChatMl`). It also recognizes `Lfm2ChatMl`: architecture `lfm2`,
  no Thinking, and an exact system/user/assistant probe rendering with
  no hidden system text, tool list or `<think>` prefix.
- Recognition is separate from release. `AgentFormat::released()` is
  false for LFM2, so `supports_agent` is false and `decide` returns
  `unsupported_template`. `LlamaProvider::for_agent_format_evaluation()`
  enables recognized unreleased formats for opt-in real-model tests only.
  The desktop app never calls it.
- When evaluated, LFM2 uses `DecisionStyle::Compact`. The prompt layout is
  shorter, with tool schemas under `parameters`, the real active scope in
  examples, and the task first. The grammar allows single spaces only.
  It restricts each call to a registered tool name and that tool's
  argument names, and requires `{}` for tools without arguments. Qwen
  keeps `DecisionStyle::Standard` and the byte-identical original
  grammar (unit tested).
- `parse_decision` now also rejects final answers that contain LFM
  native tool-call markers. This path never constructs a call. Thinking
  stays rejected for LFM2.5 Chat requests.

Registry validation, permission checks, approvals, cancellation,
execution budgets and prose rejection are unchanged for every model.

## IPC changes

| Change | Detail |
| --- | --- |
| New command `prepare_bundled_model` | No input. Returns `{ state: "ready" \| "missing" \| "corrupted", model: ModelReference \| null, autoLoad: boolean }`. Granted only to `main` (`allow-prepare-bundled-model`). Never loads a model. |
| `ModelInfo.supportsAgent` | New boolean on `load_model` and `inference_status` results. |

Frontend edits are compatibility-only. `inference.ts` and `chat.ts` add the types. `App.tsx`
calls the command after `bootstrap`, auto-loads when `autoLoad`, and
shows a banner for `corrupted`. Agent mode gating uses `supportsAgent`.
`Dialogs.tsx` shows Agent compatibility from `supportsAgent` and updates
the suggested models. `dev/fixtures.ts` mocks the new command.
Onboarding and settings redesign remain for Task 04-B.

## Files

| Area | Files |
| --- | --- |
| Inference | `crates/inference/src/llama.rs`, `crates/inference/tests/worker.rs` |
| Core | `crates/agent-core/src/inference.rs`, `crates/agent-core/src/agent/decision.rs`, `crates/agent-core/src/agent/mod.rs`, `crates/agent-core/tests/agent_decision.rs` |
| Storage | `crates/storage/migrations/005_bundled_model.sql`, `crates/storage/src/lib.rs`, `crates/storage/src/tests.rs` |
| Host | `apps/desktop/src-tauri/src/bundled_model.rs`, `commands.rs`, `main.rs`, `agent_conversation_tests.rs`, `desktop_acceptance.rs`, `build.rs`, `capabilities/main.json`, `Cargo.toml`, `tauri.bundled-model.conf.json` |
| Frontend | `apps/desktop/src/App.tsx`, `Dialogs.tsx`, `chat.ts`, `inference.ts`, `dev/fixtures.ts` |
| Packaging and notices | `scripts/stage-bundled-model.sh`, `third_party/model-licenses/LFM2.5-1.2B-Instruct/{LICENSE,NOTICE.md}`, `README.md` |
| Dependencies | `sha2 = "=0.10.9"`, already in `Cargo.lock`; the lock gains one dependency edge. Commit `Cargo.lock` with the change. |
