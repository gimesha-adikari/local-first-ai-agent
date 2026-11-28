# Development rules

## Project direction

- The project is a multimodal Personal Intelligence Layer. The approved
  external blueprint (`local-first-ai-agent/blueprint/` in the
  project context repository) is authoritative for scope, architecture,
  invariants and decisions. If this file conflicts with it, follow the
  blueprint and raise the conflict.
- Local-first means the user owns their personal data and the local store is
  authoritative. It does not mean inference must run locally. Local, cloud,
  self-hosted, OS-native and hybrid intelligence are all supported
  architectural directions.
- Any external processing, including every application-controlled outbound
  request, must pass the approved deterministic Rust security boundary
  (permission, data-label and egress policy). Nothing may bypass it, and no
  production network request is enabled before its enforcement exists.
- Implement in the approved sequential milestone stages. Do only the assigned
  stage; do not start later stages or implement future capabilities early.
- Do not publish releases before v1.0.0. Do not change version numbers, create
  tags or publish artifacts unless a stage explicitly requires it.

## Architecture

- Keep the app a modular monolith. Put platform-independent domain logic in
  `crates/agent-core`, intelligence orchestration (Chat, knowledge, semantic
  retrieval, memory context and model lifecycle) in `crates/engine`, native
  inference in `crates/inference`, SQLite in `crates/storage`, and native tool
  adapters in `crates/tools`. Keep Tauri IPC, native dialogs and window
  concerns in `apps/desktop/src-tauri`, and UI state and presentation in
  `apps/desktop/src`.
- Keep `agent-core` and `engine` independent of Tauri, React and
  operating-system UI APIs. Platform services reach the engine only through
  its small host interfaces. Test shareable logic at its public Rust boundary,
  for example `cargo test -p agent-core --locked` and
  `cargo test -p engine --locked`.
- Keep the deterministic permission engine authoritative. No model, memory,
  observed content or extension can grant, widen or bypass a permission.
- Give Tauri commands explicit names and narrow input and output types. Validate
  command inputs at the host boundary. Grant each command only to the local
  window that needs it. Preserve a restrictive content security policy.
- Avoid mandatory cloud services and broad filesystem, shell, IPC or network
  grants. Until a stage explicitly enables egress behind the boundary, keep
  runtime operation local and make no network requests.

## Engineering

- Verify dependency engines, peer ranges, Rust version requirements, and native
  prerequisites before changing dependencies. Commit both `Cargo.lock` and
  `apps/desktop/package-lock.json` with dependency changes.
- Keep model weights, secrets, credentials, personal data, generated schemas,
  and build artifacts outside Git. Keep IDE settings local.
- Use default rustfmt and keep Clippy warning-free. Run the locked workspace
  tests and the frontend tests and type-check. For desktop changes, build and
  launch the Linux executable and exercise the changed UI path. Report
  observed results, including skipped or unavailable checks.
- Run targeted tests during development and the necessary regression after
  architectural changes. Avoid redundant suites and repeated acceptance.
- For structural code discovery, prefer the codebase-memory graph when
  available. Check index generation and coverage, then read source for any
  uncovered paths and material claims.
- Run native inference on its dedicated worker. Keep model paths in Rust and
  accept them from the native file picker. Keep ordinary tests model-free and
  run real-model tests only with an explicitly supplied local file. Never
  download models unless a stage requires it.
- Limit native build parallelism on constrained hardware. Reuse the release
  profile for checks when it avoids recompiling native dependencies.
- Persist each user turn and its pending assistant response in one transaction.
  Checkpoint streams in batches, keep interrupted responses explicit, and
  test schema and lifecycle changes against a temporary database.
- Keep saved history intact when trimming inference context. Model templates
  and the tokenizer determine the actual context cost.

## Workflow

- Work on the dedicated feature branch for the current milestone. Never modify
  `main` directly.
- Get approval before pushing to GitHub, opening or merging pull requests, or
  choosing a project license or brand.

## Superseded instructions

- The v0.0.3 restriction to offline CPU chat is superseded by the blueprint.
- The temporary v0.1.0 implementation-only workflow (static review without
  local verification) ended with v0.1.0. Verify changes locally as described
  above.
