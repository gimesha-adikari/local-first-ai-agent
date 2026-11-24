# local-first-ai-agent

A local-first personal AI agent project. Milestone **v0.0.1** establishes the
Linux desktop foundation. The app lets you check the connection between React
and the shared Rust core. The release executable loads its UI from embedded
assets and the status check runs locally.

## Development on Linux

Use Rust **1.100.0** with rustup and Node.js **24.22.3 LTS** with npm. The versions
are recorded in `rust-toolchain.toml` and `.nvmrc`. With nvm, run `nvm use` from
the repository root.

Install the native build dependencies on Ubuntu if needed. See the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
sudo apt-get update
sudo apt-get install -y build-essential libwebkit2gtk-4.1-dev libxdo-dev \
  libssl-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev
```

From the repository root:

```sh
npm --prefix apps/desktop ci --engine-strict
npm --prefix apps/desktop run tauri -- dev
```

In RustRover, open the repository root and attach the root `Cargo.toml` if the
workspace is not detected automatically. Use the Tauri development command
above to start both Vite and the Rust host.

## Checks and release build

Run these commands from the repository root after installing dependencies:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked
```

The last command also type-checks and bundles the frontend. Run the release
binary with `./target/release/local-first-ai-agent` in a Linux graphical
session. Select **Check core connection** and confirm the status reads
**Agent core is ready.**

Use `cargo fmt --all` to apply the default rustfmt style. Run
`cargo test -p agent-core --locked` for core-only changes.

## Architecture

| Path | Responsibility |
| --- | --- |
| `crates/agent-core` | Tauri-independent Rust logic and its contract test |
| `apps/desktop/src-tauri` | Native desktop host and window-scoped IPC permissions |
| `apps/desktop/src` | React UI with strict TypeScript and Tailwind CSS |
| `.github/workflows/ci.yml` | Linux formatting, Clippy, tests, and release build on pushes and pull requests |

This is a modular monolith. The UI invokes the parameterless `core_status`
command on the main window. The command calls `agent_core::status()` and returns
its string. The window capability grants only that command. The release content
security policy permits local assets and Tauri IPC. Vite's development server
binds to loopback.

Linux is the first supported development target. Windows and Android are
long-term targets. Local inference, chat, SQLite, plugins, device networking,
autonomous behavior, and installer packaging are outside this milestone.
License selection is deferred.
