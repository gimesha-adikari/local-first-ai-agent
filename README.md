# local-first-ai-agent

A Linux desktop app for offline Chat, bounded Agent tasks, and local
knowledge. It runs GGUF models on the CPU with llama.cpp and saves
conversations, Agent activity, imported documents and optional memories in
local SQLite. v0.2.0 adds explicit document import, keyword and optional
semantic search, cited answers, and personal memory. Public releases are
deferred until v1.0.0. [v0.2.0 architecture](docs/architecture.md#local-knowledge) · [v0.1.0 release notes](docs/release-notes-v0.1.0.md)

## Models

| Model | Purpose | Download size |
| --- | --- | --- |
| LFM2.5-1.2B-Instruct Q4_K_M | Everyday Chat | 697 MiB |
| Qwen3.5-4B UD-Q4_K_XL | Advanced Chat, Thinking and supported Agent tasks | 2.7 GiB |
| BGE-small-en-v1.5 F16 | Semantic and hybrid Knowledge search in English | 64.2 MiB |

Open **Models → Discover** to see the exact file, license and estimated
settings. Downloads require approval and are blocked in Local-only mode.
Use **Downloads** to cancel, resume or discard a partial file. The app
checks the full SHA-256 before making the model available in **Installed**.
Load a Chat model or choose **Use for Knowledge** when you're ready to use it.

**Models → Manual import** opens a native picker for an existing Chat GGUF
or the supported BGE file. You can also configure a cloud or self-hosted
provider and use Chat without any local models.

## Install on Linux

### Bundled Debian package

The accepted `amd64` package was tested on Ubuntu 26.04.1 LTS. Its
executable references glibc 2.39 and its CPU build uses AVX2, FMA, F16C
and BMI2. Compatibility outside the tested environment remains unverified.
The checksum below identifies the v0.1.0 package. v0.2.0 has no packaged
installer in this milestone. Verify the v0.1.0 package before installing.

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
- **Knowledge** imports one TXT, Markdown or PDF file at a time after
  retention consent. Keyword search runs in SQLite. Semantic and hybrid
  search need the imported BGE model. Enable Knowledge in Chat, tick the
  documents to search, and choose Automatic or Documents-only grounding.
  Each answer shows its sources, coverage and fitted counts.
  Overviews read at most four passages and say when they are partial.
- **Memory** is off by default. Save notes explicitly under Settings →
  Knowledge & Memory. When enabled, up to three relevant notes are added to
  later answers as background, separate from document evidence.
- **Web research** is off until you turn on Search for one message. It uses
  your own Brave Search API key or a SearXNG instance you configure under
  Settings → Web search, or reads one public `https://` page you enter. Only
  your message, or the query you edit, goes to the search provider. Up to
  three public pages are read directly, without cookies, sign-in, scripts or
  images, and the answer cites the sources it was given. Searching and
  reading always use the internet, even with a local model, so Local-only
  mode blocks them. A cloud or self-hosted model receives page excerpts only
  when you allow it for that message. Raw results and excerpts stay in
  memory; cited links are saved only for direct pages or when you mark your
  search plan as permitting storage.

## Known limitations

- CPU speed, context capacity and memory affect latency and output length.
  Models can make mistakes or time out. A task defaults to 300 seconds
  and allows 1–600 seconds. Launch approval lasts up to 60 seconds within
  that budget. Cancellation is cooperative and an already dispatched
  app may open.
- PDF extraction requires Poppler and working Linux namespace isolation
  with Bubblewrap and seccomp. It supports PDFs with selectable text up to
  8 MiB and 256 pages. Scanned pages require OCR outside the app. Encrypted
  PDFs, image input, and GPU inference are unsupported.
- Knowledge limits: TXT and Markdown are capped at 1 MiB, the library holds
  128 documents and 32 MiB of retained text, and memory holds 512 notes.
  Scanned PDFs and encrypted files are not indexed. Only one import runs at
  a time.
- Search and routing are English-focused. Keyword search has no stemming, so
  "run" does not match "running". Routing cues are English heuristics. A
  refinement such as "make it shorter" must be short and add no new subject.
- Answers are grounded only in the passages supplied to the model. A
  refinement re-answers from current evidence, so wording can change beyond
  the requested revision. Evidence answers do not carry earlier turns.
- LFM2.5 often writes facts in brackets instead of linked source markers, so
  some grounded answers show sources as uncited.
- Semantic search returns `busy` while Chat or Agent is generating. The
  library refreshes by polling rather than by push events.
- The local personal database is encrypted with SQLCipher in Stage 04-A builds.
  Saved answers, documents, and memories can contain text from selected files.
  Raw tool previews are held in memory for the current session. Deleting a
  conversation removes its saved messages and linked task history.
  Content-free audit entries and external copies can remain. Earlier answers
  may keep quotations from deleted documents or memories.
- Web research reads HTTPS pages on port 443 only, at most 2 MiB each, and
  does not read PDFs, run JavaScript or pass sign-in or paywalls. Evidence
  ranking is lexical. Live compatibility with Brave Search and SearXNG has
  been checked only against recorded request and response shapes.
- Downgrading to v0.1.0 requires the database backup taken before the
  upgrade. The v0.1.0 executable accepts only schema version 5 and below.

## Encrypted personal storage

The Linux desktop uses your unlocked system keyring to open the database at
startup. Use **Settings → Privacy & Storage → Show recovery key** and confirm
in the native dialog to record the key in a password manager or another secure
location. The recovery key is specific to this database. Keep it separate from
the database file. Losing both the keyring entry and the recovery key can make
the database permanently inaccessible. If the keyring entry goes missing or
the profile moves to a new path, use **Enter recovery key** at startup. The app
checks the supplied key against the database before saving it to the keyring.

On first opening a legacy profile, the app copies and verifies the database
before switching to the encrypted file. **Privacy & Storage** shows the
original file as `conversations.sqlite3.plaintext-backup`. Verify your records
and record your recovery key, then quit the app and remove that backup through
your file manager when you no longer need it. File removal does not guarantee
erasure from SSDs, system snapshots, or earlier backups. Keep the backup if you
need to open the pre-migration state with an older build. Changes made after
migration live in the encrypted database.

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

Storage bundles SQLCipher Community Edition through rusqlite and links to the
system OpenSSL `libcrypto` on Linux. The desktop requires a persistent Secret
Service keyring. SQLite keeps temporary query data in memory, and SQLCipher
encrypts database pages written to journals.

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

### Build the Debian package

The base package contains no language or embedding model weights. Models are
optional, verified local imports, and Chat can also use a cloud or
self-hosted OpenAI-compatible provider.

```sh
npm --prefix apps/desktop run tauri -- build \
  --config src-tauri/tauri.deb.conf.json --bundles deb -- --locked
```

The package depends on Tesseract with English data and the PipeWire client
library for contextual screen-region text recognition. Other OCR languages
are optional `tesseract-ocr-*` packages; the recommended set is listed in
`/usr/share/doc/local-first-ai-agent/README.ocr-languages`.

### Providers

Settings → Providers configures OpenAI-compatible cloud services and servers
you run. Local-only mode, the default, makes no outbound connections. Every
remote request passes the Rust network boundary, carries only the categories
you allow, and is checked against the monthly limit first. API keys are
entered in a native dialog and kept in the system keyring (Secret Service);
without a keyring, providers that need a key stay off.
[Stage 03-C record](docs/stage-03-c-providers.md)

## Licenses and notices

The original application source is licensed under Apache-2.0.
The Debian package installs the application license, software copyright
and license notices, and the MPL-2.0 source copy for `option-ext` under
`/usr/share/doc/local-first-ai-agent/`.
[Application license](LICENSE) · [Software copyright and notices](third_party/software-notices/copyright)

LFM2.5, formerly bundled, is no longer part of the package. It remains
under the separate LFM Open License v1.0 when you import it; section 5
limits commercial use by legal entities with annual revenue of US$10
million or more.
[LFM2.5 license and notice](third_party/model-licenses/LFM2.5-1.2B-Instruct/NOTICE.md)

See the [roadmap](docs/roadmap.md) and
[distribution notice verification](docs/task-07-a-distribution-notices.md)
for milestone status and publication checks.
