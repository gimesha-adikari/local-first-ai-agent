# Development rules

- Keep the app a modular monolith. Put platform-independent logic in
  `crates/agent-core`, native inference in `crates/inference`, SQLite in
  `crates/storage`, and native tool adapters in `crates/tools`. Keep Tauri IPC in `apps/desktop/src-tauri`, and UI state
  and presentation in `apps/desktop/src`.
- Keep the core independent of Tauri, React, and operating-system UI APIs. Test
  shareable logic at its public Rust boundary with `cargo test -p agent-core --locked`.
- Give Tauri commands explicit names and narrow input and output types. Validate
  command inputs at the host boundary. Grant each command only to the local
  window that needs it. Preserve a restrictive content security policy.
- Keep runtime operation local. Avoid mandatory cloud services and broad
  filesystem, shell, or network grants.
- Verify dependency engines, peer ranges, Rust version requirements, and native
  prerequisites before changing dependencies. Commit both `Cargo.lock` and
  `apps/desktop/package-lock.json` with dependency changes.
- Keep model weights, secrets, personal data, generated schemas, and build
  artifacts outside Git. Keep IDE settings local.
- Use default rustfmt and keep Clippy warning-free. Run the locked workspace
  tests and the frontend type-check. For desktop changes, build and launch the
  Linux executable and exercise the changed UI path. Report observed results.
- For structural code discovery, prefer the codebase-memory graph when
  available. Check index generation and coverage, then read source for any
  uncovered paths and material claims.
- Run native inference on its dedicated worker. Keep model paths in Rust and
  accept them from the native file picker. Keep ordinary tests model-free and
  run real-model tests only with an explicitly supplied local file.
- Limit native build parallelism on constrained hardware. Reuse the release
  profile for checks when it avoids recompiling native dependencies.
- Persist each user turn and its pending assistant response in one transaction.
  Checkpoint streams in batches, keep interrupted responses explicit, and
  test schema and lifecycle changes against a temporary database.
- Keep saved history intact when trimming inference context. Model templates
  and the tokenizer determine the actual context cost.
- Keep v0.0.3 scoped to offline CPU chat. Defer embeddings, tools, Android,
  device networking, and GPU offloading.
- Get approval before pushing to GitHub or choosing a project license or brand.

## v0.1.0 implementation-only branch

On the feature/agent-v0.1.0 branch, the approved temporary workflow is
source implementation and static review through GitHub. Write appropriate
test source, but defer local compilation, formatting/lint execution, test
execution, model evaluation and desktop acceptance to a separate Codex
verification phase. Do not disable existing CI; it may run automatically.
Treat changes as implemented, not runtime verified. Only update the dedicated
feature branch, never main, and do not merge without approval. The existing
v0.0.3 scope restriction describes the baseline; v0.1.0 adds only the bounded,
permission-aware agent MVP. Existing privacy and platform boundaries remain
in force.
