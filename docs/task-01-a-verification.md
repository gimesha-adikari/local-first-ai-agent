# Task 01-A backend verification

This report covers the functional foundation repair on the Linux
worktree based on `18d9bb8ac302c297acf0aaf671ea93631d8b4914`. The starting
dirty diff and file hashes were recorded before editing. Both GGUFs were
already present on the machine. Builds reused the release cache with one
low-priority Cargo and CMake job pinned to one CPU. Model probes used four
CPU threads and an isolated network namespace with loopback down. Desktop
probes used private Xvfb, D-Bus and temporary XDG profiles.

## Chat diagnosis

The old request default was 256 output tokens. SQLite stores sampling and
thinking settings per model reference, while a conversation stores its
system prompt and message history. The chat command previously inherited
the model's saved thinking flag for every send and regeneration. Thinking
and the visible answer shared the same 256-token budget. Reaching that
limit could therefore leave only reasoning. The earlier pinned Qwen3
test recorded exactly this output limit. [Previous real-model evidence](verification-v0.0.2.md#known-limits)

The model tokenizer checks the whole rendered prompt plus the reserved
output against the loaded context. Qwen3.5's enabled template adds an
opening `<think>` to the prompt. That token is outside the generated
stream. Qwen3 can emit its opener in generated output. Normalizing the
Qwen3.5 stream makes reasoning available to the existing parser while
keeping token statistics native. Normal end-of-generation now needs a
nonblank answer for a `complete` assistant row. Token-limit output stays
`truncated`, and cancellation stays `cancelled`. The core excludes known
reasoning-only attempts from completed chat history. Existing rows and
explicit saved output limits remain intact.

The old screenshot did not supply the model reference or settings behind
the request. The investigation used synthetic prompts and did not read
the user's conversation database.

Both local models passed real offline greeting, poem, Rust function,
ownership explanation, and multi-turn factual recall probes. The direct
answers ended normally without a leading reasoning block. Qwen3-0.6B
produced a three-line poem where the prompt asked for four. Qwen3.5
produced four lines. Both returned `Pico` from the supplied history.
Explicit thinking with a deliberately tiny budget produced `truncated`
on both models. The Qwen3.5 stream began `<think>\nThinking`, with only
one actual generated token. Stop after live deltas returned `cancelled`
on both. The existing Qwen3 lifecycle probe also reproduced a 256-token
thinking limit and passed UTF-8 streaming, unload and reload checks.

## Agent diagnosis

The previous Linux binary reproduced the manual failure at context 2048.
It used the native picker to grant access to a directory containing 102
synthetic ledger files with nested paths and UTF-8 names. SQLite recorded
one successful `files.search` dispatch, two model calls, and zero retries.
The second call ended as `model_failed` with no synthesized answer. The old
runtime mapped inference errors to that one generic code. It charged the
failed decision's full 512-token reservation because actual usage was
unavailable.

In a separate Qwen3.5 tokenizer probe, eighty valid paths serialized
within the 8 KiB native result limit, yet rendered to 5,324 prompt tokens.
The 2048-token model context also needed to reserve 512 output tokens.
The full input failed `context_limit` before native decoding. The first
preview attempt fit in 1,394 prompt tokens, then the model misreported the
six shown examples as the total. The model projection now gives the native
returned count and sample paths distinct keys, with an explicit statement
about the relationship. The original observation remains intact for IPC.
The corrected tokenizer run fit the 5,320-token full input into a
1,418-token model preview and produced a valid, normally terminated
31-token decision. It correctly reported 80 returned paths. It still
inferred broader group coverage from the examples. The native tool result
is authoritative for names and coverage.

The inference worker keeps a full prompt when it fits. After a context
rejection, it can try a 512-byte data preview per observation and then a
metadata-only view for variable-size data. These are tokenizer-backed
fitting attempts before decode, with the same authorization and token
limits. A clipped answer gets a bounded shorter-output retry. Decision
recovery suppresses already-successful calls, while authorization and
native dispatch rules still govern every new proposal. Native results
stay available with a failed synthesis and remain separate from model
answers. The IPC details and retention are in the [handoff](task-01-a-backend-handoff.md).

## Linux desktop evidence

| Case | Observed result |
| --- | --- |
| Baseline at context 2048 | The native ledger search succeeded. The following decision failed with generic `model_failed` after 68.58 seconds. |
| Patched release at context 2048 | The same natural-language search completed in 170.00 seconds with one dispatch, two model calls and zero retries. The answer reported 83 returned paths and identified the separate native result. |
| Count-only desktop search | The native observation contained 83 paths with `truncated: true`. The model independently reported the same count and truncation in 99.10 seconds. |
| Mixed-extension desktop search | The native observation and synthesized answer matched the three invoice `.txt` files, including names with spaces and accents. The `.json` file and symlink were absent. The task completed in 93.48 seconds with one dispatch, two model calls and no retries. |
| Picker grant and revocation | The rebuilt desktop selected a synthetic folder, displayed `Revoked` after the user's action, and wrote one grant audit record and one revoke record. |
| Patched release at context 1600 | The native search succeeded and emitted its observation. Synthesis reached `model_context_limit` in 56.80 seconds with one dispatch, two model calls and zero retries. The UI showed the actionable error. |
| Patched chat release | A saved thinking flag set to true still yielded a direct rain poem through the older client, which omits the per-turn option. A second prompt returned a `fn main` Rust program containing `HELLO_FROM_RUST`. The program was displayed as text. |
| Chat persistence and Stop | Stop after visible streaming reached its terminal UI in 410 ms. Regeneration and the next prompt worked. Conversation history, preferences, context trimming and deletion behaved as expected across restart. SQLite integrity and foreign-key checks passed. |

The old UI already exposes live observations in its timeline. The desktop
test verified that path on the undersized-context failure. The new
terminal `nativeResults` and scope display DTO are covered by Rust JSON
contract tests. The updated screens and the explicit per-turn UI control
are part of the frontend handoff.

The mixed-extension harness originally expected `truncated: false` and
stopped before testing revoke. The native adapter intentionally sets
`truncated: true` when a candidate cannot be resolved safely, including a
rejected symlink. The captured native result confirmed all three expected
paths. The result and flag were rechecked from the captured payload.

## Engineering checks

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` and `git diff --check` | Passed |
| Release Clippy for all workspace targets, warnings denied | Passed |
| Locked release workspace tests with Tauri custom protocol | 89 passed, 13 ignored in the ordinary run |
| Tauri Linux release build and embedded production assets | Passed |

The ordinary suite exercised completion classification, lossless IPC,
unchanged execution budgets, bounded recovery and non-repetition,
intent and outcome persistence, and sanitized grant display names.
The release build also type-checked and built the existing frontend.

Reproduce the opt-in model probes by building the locked release tests and
running the printed test binaries in a network namespace with `AGENT_TEST_MODEL`
pointing to the corresponding existing GGUF. Run `foundation_chat_prompts`
from the `qwen` test binary for either model, `real_qwen_cpu_inference` from
the same binary for Qwen3-0.6B, and
`llama::tests::foundation_agent_fits_large_native_observation` from the
inference unit test binary for Qwen3.5. Pass `--ignored --exact` for each
probe. Desktop verification used the embedded release executable with
temporary XDG profiles. It did not require a Vite server.

## Remaining limitations

- Model synthesis still depends on available context and the model's
  interpretation of previews. A completed synthesis can omit details or
  overgeneralize. Use the native observation for exact file names, counts
  and truncation.
- Saved 256-token limits retain their meaning. The user can increase
  that limit through model settings or the one-turn override. Explicit
  thinking can still use the entire output budget.
- Native result payloads are available in the terminal event and the
  most recent task's in-memory cache. Task history retains durable
  execution metadata across restart. A previous session's file contents
  are unavailable from its task detail.
- The current frontend continues to show its existing transient tool
  details and saved thinking control. The new separate result view,
  grant labels and explicit per-turn thinking choice await frontend
  integration and end-to-end review.

## File inventory

| Area | Files changed for this task |
| --- | --- |
| Desktop host and IPC tests | `apps/desktop/src-tauri/src/commands.rs`, `agent.rs`, `agent_contract.rs` |
| Chat and inference domain | `crates/agent-core/src/chat.rs`, `crates/agent-core/src/inference.rs`, `crates/agent-core/tests/chat.rs` |
| Agent domain and recovery | `crates/agent-core/src/agent/decision.rs`, `error.rs`, `policy.rs`, `runtime.rs`, `crates/agent-core/tests/agent_runtime.rs` |
| Model adapter and real probes | `crates/inference/src/llama.rs`, `crates/inference/tests/qwen.rs` |
| Native filesystem adapter and test | `crates/tools/src/filesystem.rs`, `crates/tools/tests/filesystem_scopes.rs` |
| Documentation | `README.md`, `docs/task-01-a-backend-handoff.md`, `docs/task-01-a-verification.md` |

All 46 starting dirty paths remained present. Thirty-four matched their
starting hashes and 12 had Task 01-A edits. The frontend source, CSS,
command allowlist and dependency lockfiles had no Task 01-A edits.
