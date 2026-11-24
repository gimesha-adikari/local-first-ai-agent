# local-first-ai-agent

A local-first personal AI agent project. Milestone **v0.0.2** adds a Linux
desktop screen for testing local GGUF inference. The release embeds its UI and
runs llama.cpp on the CPU.

## Development on Linux

Use Rust **1.100.0** with rustup and Node.js **24.22.3 LTS** with npm. The versions
are recorded in `rust-toolchain.toml` and `.nvmrc`. With nvm, run `nvm use` from
the repository root.

Install the native build dependencies on Ubuntu if needed. See the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
sudo apt-get update
sudo apt-get install -y build-essential libwebkit2gtk-4.1-dev libxdo-dev \
  libssl-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev \
  libgtk-3-dev cmake clang libclang-dev
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
cargo clippy --release --workspace --all-targets --locked -- -D warnings
cargo test --release --workspace --locked
npm --prefix apps/desktop test
npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked
```

The last command also type-checks and bundles the frontend. Use the release
profile for Rust checks and the desktop build to reuse native build output.
On a resource-constrained Linux machine, set `CARGO_BUILD_JOBS=1` and
`CMAKE_BUILD_PARALLEL_LEVEL=1` before building. With libclang present and no
Clang resource headers installed, bindgen can use the installed GCC headers
via `export BINDGEN_EXTRA_CLANG_ARGS="-idirafter $(gcc -print-file-name=include)"`.

## Try local inference

Run `./target/release/local-first-ai-agent` in a Linux graphical session.
Choose a trusted local GGUF model with an embedded chat template, then select
**Load model**. Enter a prompt and select **Generate**. Select **Stop** to cancel
the run or **Unload model** to free the model. The default settings are 4 CPU
threads, a 2048-token context, 256 output tokens, temperature 0.7, top-p 0.8, and
top-k 20. The context must fit both the formatted prompt and requested output.

The reference fixture is Qwen's Apache-2.0
`Qwen3-0.6B-Q4_K_M.gguf` (396,704,416 bytes). Its official upload is pinned to
revision `1eaf4d9657fe65ad10a51eab76a8db5b363bddaa`. The published SHA-256 is
`b0638f08417a2d3c8652760462eb5407c6e30173cf9608ad0820757a281eea0e`.
Keep this file outside the repository.
[Qwen's pinned GGUF](https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/blob/1eaf4d9657fe65ad10a51eab76a8db5b363bddaa/Qwen3-0.6B-Q4_K_M.gguf)

The real-model test is opt-in and receives a local file path. It prints load
time, time to first token, decode throughput, cancellation latency, and Linux
peak RSS. The normal test suite never fetches model files.

Load time covers native initialization, mapping, and template validation.
Memory-mapped weights can page in during generation. Time to first token
includes prompt evaluation. Decode throughput excludes the first token, and
peak RSS covers the test process. Cancellation latency runs from the stop
request to completion of the worker operation.

```sh
AGENT_TEST_MODEL=/path/to/Qwen3-0.6B-Q4_K_M.gguf \
  cargo test --release --locked -p inference --features llama \
  --test qwen real_qwen_cpu_inference -- --ignored --nocapture
```

For a network-isolated Linux runtime check, first compile the test with
`--no-run`, then run the resulting test executable under
`unshare --user --map-current-user --net`. Keep the model path in
`AGENT_TEST_MODEL`.

See [the architecture notes](docs/architecture.md) for the provider contract
and permission boundaries. The milestone covers single-prompt CPU inference.
Linux is the verified target. License selection is deferred.
