# Development rules

- Keep the app a modular monolith. Put platform-independent logic in
  `crates/agent-core`. Keep Tauri IPC and window setup in
  `apps/desktop/src-tauri`, and UI state and presentation in `apps/desktop/src`.
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
- Keep Phase 1 limited to the documented foundation. Defer inference, chat,
  SQLite, Android, plugins, device networking, and autonomous behavior.
- Get approval before pushing to GitHub or choosing a project license or brand.
