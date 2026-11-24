# v0.0.3 offline chat verification

Verification ran on Ubuntu 26.04.1 LTS x86-64 with an Intel Core i5-8500,
14 GiB of RAM, Rust 1.100.0, and Node.js 24.22.3. Native builds used one
low-priority job pinned to one CPU. Inference used four CPU threads with zero
GPU layers. The release embedded the React frontend and ran with no Vite
server. Desktop checks used a new temporary application data directory.

## Model and runtime

The existing `Qwen3-0.6B-Q4_K_M.gguf` verified at 396,704,416 bytes with
SHA-256 `b0638f08417a2d3c8652760462eb5407c6e30173cf9608ad0820757a281eea0e`.
The official upload is Apache-2.0 at revision
`1eaf4d9657fe65ad10a51eab76a8db5b363bddaa`. Verification reused the file
on the BACKUP volume. [Official pinned Qwen GGUF](https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/blob/1eaf4d9657fe65ad10a51eab76a8db5b363bddaa/Qwen3-0.6B-Q4_K_M.gguf)

The new dependencies are rusqlite 0.43.0 with bundled SQLite 3.54.1
(`libsqlite3-sys` 0.40.0), and React Markdown 10.1.0. The inference engine
remains llama-cpp-2 0.1.161 with MiniJinja 2.24.0. The desktop uses Tauri
2.12.7, React 19.3.1, TypeScript 7.0.2, and Vite 8.3.8.

## Automated checks

| Check | Result |
| --- | --- |
| Rust formatting and `git diff --check` | Passed |
| Release-profile workspace Clippy with warnings denied | Passed |
| Locked release-profile Rust workspace tests | 17 passed, 4 real-model tests ignored by default |
| Frontend tests | 8 passed |
| TypeScript and Vite production build | Passed |
| GitHub Actions workflow lint | Passed locally with `actionlint` |
| Linux Tauri release build | Passed, produced `target/release/local-first-ai-agent` |
| Opt-in real Qwen tests in the network namespace | 3 passed |

Ordinary tests cover schema creation and migration rollback, CRUD and literal
search, ordered reply attempts, foreign keys, interrupted-generation recovery,
model configuration and references, GGUF path and file validation, mocked
generation cancellation, context selection, streaming UTF-8 and state updates,
split reasoning delimiters, and Markdown rendering with untrusted input.

The real-model tests returned `Pico.` after omitting one complete earlier
turn. The rendered prompt used 67 tokens with a 384-token context and a
64-token output budget. A latest user message that could not fit was rejected
before decoding. The existing inference lifecycle test verified multiple
streamed chunks, end-of-generation detection, exact UTF-8 output `你好世界`,
rejection of conflicting operations, cancellation, and inference after reload.
The measured cancellation latency in this run was 31.7 ms. The template test
verified both Qwen thinking modes. The thinking generation stopped at 256
tokens.

## Actual Linux desktop acceptance

All 15 checks used the Tauri release executable with its bundled frontend,
the native GTK file picker, and the pinned GGUF. AT-SPI and X11 input drove
the window in Xvfb. The process ran in a Linux network namespace with only
`lo` present and down, and no default route.

| # | Check | Observation |
| --- | --- | --- |
| 1 | Create a conversation | New conversation showed the empty chat state. |
| 2 | Send a fact | Enter sent `My pet cat is named Miso. Reply with OK.` and Qwen replied `OK.` |
| 3 | Ask a follow-up | Sent `What is the name of my pet cat? Reply with the name only.` in the same chat. |
| 4 | Get a relevant answer | Qwen replied `Miso`. |
| 5 | Verify conversation isolation | A separate chat asking for the same fact with an `UNKNOWN` fallback received `UNKNOWN`. |
| 6 | Rename | Saved `Miso notes` and `Separate chat` as conversation titles. |
| 7 | Search | Searching `Miso notes` narrowed the list to that chat. Clearing search restored the second entry. |
| 8 | Cancel | Stop remained usable during a long reply. The partial reply displayed `Stopped` and SQLite stored `cancelled`. |
| 9 | Recover | The next prompt received `READY`. |
| 10 | Regenerate | Regenerate returned `READY` in a new assistant attempt for the same user message. |
| 11 | Restart | Relaunching with the same profile showed the saved titles and contextual message. |
| 12 | Persist preferences | After relaunch Enter was still configured to insert a newline. SQLite retained the dark appearance and composer preference. |
| 13 | Reload model | Loaded from the saved model reference, unloaded, then loaded it again. |
| 14 | Enforce context budget | With a 256-token context and 64-token output budget, Qwen answered `BLUE`, the UI reported one omitted earlier turn, and saved history remained. An oversized latest message produced the context-limit error and a failed assistant row. |
| 15 | Delete and restart | Confirmed deletion of `Budget chat`. Subsequent launch and a database read showed only `Miso notes` and `Separate chat`. |

After the final close, SQLite `integrity_check` returned `ok` and
`foreign_key_check` returned no rows. The two retained conversations had ten
complete message rows and one cancelled assistant row. The chat directory was
mode `0700` and the database file was `0600`.

A separate release smoke test used a private Weston Wayland compositor with
`DISPLAY` unset and networking disabled. It mapped a 1172 × 869 window and
exposed the model controls. The user’s graphical session remained untouched.
The Wayland smoke covered launch and window rendering. Full inference and
conversation acceptance ran in Xvfb.

## Reproduction

With project dependencies installed, run the checks and build from the workspace root.

```sh
export CARGO_BUILD_JOBS=1 CMAKE_BUILD_PARALLEL_LEVEL=1
cargo fmt --all --check
cargo clippy --release --locked --workspace --all-targets -- -D warnings
cargo test --release --locked --workspace
npm --prefix apps/desktop test
npm --prefix apps/desktop run typecheck
actionlint .github/workflows/ci.yml
npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked
```

To repeat the Linux desktop check in an isolated profile, start an Xvfb session
with a separate network namespace. The only interface is loopback, which stays
down. Use the installed model file through the native picker.

```sh
profile=$(mktemp -d)
mkdir -m 700 "$profile"/data "$profile"/config "$profile"/cache "$profile"/runtime
unshare --user --map-current-user --net env -u WAYLAND_DISPLAY \
  XDG_DATA_HOME="$profile/data" XDG_CONFIG_HOME="$profile/config" \
  XDG_CACHE_HOME="$profile/cache" XDG_RUNTIME_DIR="$profile/runtime" \
  GDK_BACKEND=x11 xvfb-run -a dbus-run-session -- sh -eu -c '
    ip -br link
    test -z "$(ip route show default)"
    exec ./target/release/local-first-ai-agent
  '
```

To interact manually, use an isolated graphical session with a viewer or
exercise the Xvfb window with AT-SPI accessibility actions. Use the same
temporary profile when restarting the program. Real-model Rust tests can run
inside the same type of network namespace with the existing dependencies cached.

```sh
export AGENT_TEST_MODEL=/path/to/Qwen3-0.6B-Q4_K_M.gguf
unshare --user --map-current-user --net bash -eu -c '
  ip -br link
  test -z "$(ip route show default)"
  export CARGO_NET_OFFLINE=true
  cargo test --release --locked -p inference --features llama --test qwen \
    qwen_remembers_the_retained_turn_after_trimming -- --ignored --nocapture
  cargo test --release --locked -p inference --features llama --test qwen \
    real_qwen_cpu_inference -- --ignored --nocapture
  cargo test --release --locked -p inference --features llama --lib \
    renders_pinned_qwen_template_in_both_modes -- --ignored --nocapture
'
```

## Security and limits

Conversation data is plaintext SQLite in the OS local application data
directory. The Unix chat directory and database are created with owner-only
permissions. Delete removes the conversation and associated messages from the
database. Forensic erasure is outside the scope of ordinary SQLite deletion.

The native picker authorizes GGUF paths and the renderer uses Rust-issued
reference IDs. The app starts without loading a model and reports an error if
a saved file is unavailable. Generated Markdown disables raw HTML, navigation
links, and remote images. Code fences copy text on request. The main-window
capability grants named IPC commands. Chat and diagnostics run locally.

Search uses SQLite's ASCII case folding. The model sees only the complete
turns that fit, the system instructions, and the latest user message. Earlier
saved attempts remain in the transcript. Reasoning shares the configured
output budget. One generation runs at a time. The CPU benchmark in the
v0.0.2 verification remains the performance reference for this milestone.
