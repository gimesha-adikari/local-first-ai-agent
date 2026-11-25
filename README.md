# local-first-ai-agent

A Linux desktop app for offline Chat and bounded Agent tasks. v0.1.0
runs GGUF models on the CPU with llama.cpp and saves conversations and
Agent activity in local SQLite. The accepted build is a release candidate.
Publication is pending. [v0.1.0 release notes](docs/release-notes-v0.1.0.md)

## Models

| Model | How to get it | Chat | Thinking | Agent |
| --- | --- | --- | --- | --- |
| LFM2.5-1.2B-Instruct Q4_K_M | Included in the bundled Debian package | Yes | Disabled | Disabled |
| Qwen3.5-4B | Optional local GGUF imported through **Models** | Yes | Yes | Yes |

LFM2.5 is the lightweight built-in assistant. The app verifies the
bundled file by size and SHA-256 and loads it automatically in a fresh
profile. Qwen3.5-4B takes more memory and CPU time. The verified build used
the Unsloth `UD-Q4_K_XL` quantization. Use **Models → Import model…**,
select an existing single-file GGUF, then load it. The app has no model
download or automatic model switching. [Verified Qwen GGUF](https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/blob/6727efff692673b7a2ebb4c6ba830ce4f5fb1309/Qwen3.5-4B-UD-Q4_K_XL.gguf)

## Install on Linux

### Bundled Debian package

The accepted `amd64` package was tested on Ubuntu 26.04.1 LTS. Its
executable references glibc 2.39 and its CPU build uses AVX2, FMA, F16C
and BMI2. Compatibility outside the tested environment remains unverified.
With the Task 07-A package in the current directory, verify it before
installing.

```sh
printf '%s  %s\n' \
  ff0daea7989611feba6770b8e0dfc0c914ed0d2dcc42ebb6afb0a29a3b011c1a \
  local-first-ai-agent_0.1.0_amd64.deb | sha256sum -c -
sudo apt install ./local-first-ai-agent_0.1.0_amd64.deb
```

Open the app from the desktop menu or run `local-first-ai-agent`.
APT resolves GTK 3, WebKitGTK 4.1, Poppler, Bubblewrap and util-linux.
The checksum above identifies the package with the distribution notices.
A later build has its own checksum.

### Model-free executable

From a source checkout, install the native build packages under
[Development on Linux](#development-on-linux), then build and install
the standalone executable for your account. Import a local model before
starting Chat or Agent.

```sh
npm --prefix apps/desktop ci --engine-strict
npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked
install -Dm755 target/release/local-first-ai-agent "$HOME/.local/bin/local-first-ai-agent"
"$HOME/.local/bin/local-first-ai-agent"
```

The executable needs the Linux GTK 3 and WebKitGTK 4.1 runtime. Install
`poppler-utils`, `bubblewrap` and `util-linux` to read PDFs. The packaged
build declares these dependencies automatically.

## Use Chat and Agent

- **Chat** streams answers, saves multi-turn history, and supports Stop,
  Regenerate and continuation when a reply reaches its output limit.
  Thinking is opt-in on compatible models and shares the answer budget.
  New profiles target 8,192 context tokens, 4,096 output tokens and four
  CPU threads. Model capacity and available memory can lower these
  recommendations. Instructions, input, history and output share context.
  Existing model settings and preferences are preserved.
- **Agent** uses a calculator, reads device information, searches selected
  files, reads UTF-8 text and PDFs, and discovers trusted desktop apps.
  File and folder grants are read-only, expire after an hour, and end when
  revoked or when the app closes. Select a file for an exact read or select
  a folder to search its contents and read files within it. Each launch
  requires one-time approval of the displayed application, desktop entry
  and executable.
- **PDFs** support suffix-filtered search and text-layer extraction. A read
  covers up to four contiguous pages, with a default of the first four.
  The app shows extracted text and coverage warnings separately from the
  model's answer. [Permission and data architecture](docs/architecture.md)

## Known limitations

- CPU speed, context capacity and memory affect latency and output length.
  Models can make mistakes or time out. A task defaults to 300 seconds
  and allows 1–600 seconds. Launch approval lasts up to 60 seconds within
  that budget. Cancellation is cooperative and an already dispatched
  app may open.
- PDF extraction requires Poppler and working Linux namespace isolation
  with Bubblewrap and seccomp. It supports PDFs with selectable text up to
  8 MiB and 256 pages. Scanned pages require OCR outside the app. Encrypted
  PDFs, image input, and GPU inference are unsupported in v0.1.0.
- Local SQLite is unencrypted. Saved answers can contain text from
  selected files. Raw tool previews are held in memory for the current
  session. Deleting a conversation removes its saved messages and linked
  task history. Content-free audit entries and external copies can remain.

## Development on Linux

Use Rust **1.100.0** and Node.js **24.22.3** as pinned in
`rust-toolchain.toml` and `.nvmrc`. With nvm, run `nvm use` from the
repository root. Install the native dependencies on Ubuntu if needed.
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

```sh
sudo apt-get update
sudo apt-get install -y build-essential libwebkit2gtk-4.1-dev libxdo-dev \
  libssl-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev \
  libgtk-3-dev cmake clang libclang-dev
npm --prefix apps/desktop ci --engine-strict
npm --prefix apps/desktop run tauri -- dev
```

Use the release profile to reuse native build output. On constrained
machines, set `CARGO_BUILD_JOBS=1` and `CMAKE_BUILD_PARALLEL_LEVEL=1`.
If bindgen cannot find Clang resource headers, the installed GCC headers
can be added with
`export BINDGEN_EXTRA_CLANG_ARGS="-idirafter $(gcc -print-file-name=include)"`.

```sh
cargo fmt --all --check
cargo clippy --release --workspace --all-targets --locked --features tauri/custom-protocol -- -D warnings
cargo test --release --workspace --locked --features tauri/custom-protocol
npm --prefix apps/desktop test
npm --prefix apps/desktop run typecheck
```

Ordinary tests are model-free. Real-model tests are opt-in and receive an
existing local file through `AGENT_TEST_MODEL`. Keep weights, generated
files and local profiles outside Git. [CPU inference benchmarks](docs/verification-v0.0.2.md)

### Build the bundled package

The Debian build uses a local copy of the pinned LFM2.5 GGUF. The staging
script verifies it and links it into the ignored packaging resources.

```sh
scripts/stage-bundled-model.sh /path/to/LFM2.5-1.2B-Instruct-Q4_K_M.gguf
npm --prefix apps/desktop run tauri -- build \
  --config src-tauri/tauri.bundled-model.conf.json --bundles deb -- --locked
```

## Licenses and notices

The original application source is licensed under Apache-2.0.
The Debian package installs the application license, software copyright
and license notices, and the MPL-2.0 source copy for `option-ext` under
`/usr/share/doc/local-first-ai-agent/`.
[Application license](LICENSE) · [Software copyright and notices](third_party/software-notices/copyright)

Bundled LFM2.5 remains under the separate LFM Open License v1.0.
Section 5 limits commercial use by legal entities with annual revenue
of US$10 million or more. Redistribution requires the license text,
which the package installs with the model notice beside the GGUF.
[Bundled model license and notice](third_party/model-licenses/LFM2.5-1.2B-Instruct/NOTICE.md)

See the [roadmap](docs/roadmap.md) and
[distribution notice verification](docs/task-07-a-distribution-notices.md)
for milestone status and publication checks.
