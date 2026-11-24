# Offline chat architecture

React, the Tauri host, the inference worker, and SQLite run in one local
application. The Linux release embeds its frontend assets.

| Layer | Responsibility |
| --- | --- |
| crates/agent-core | Conversation and message entities, storage contract, inference provider contract, and context selection |
| crates/inference | Dedicated worker, GGUF validation, llama.cpp, template rendering, and tokenizer-backed budgeting |
| crates/storage | Versioned SQLite schema, transactions, message history, model references, and preferences |
| apps/desktop/src-tauri | App data directory, native picker, command validation, generation coordination, and streaming IPC |
| apps/desktop/src | Conversation UI, composer, scoped Markdown rendering, model controls, and settings |

## Persistence and recovery

SQLite stores timestamped conversations and ordered messages. The conversation
and its messages retain all saved turns when inference omits older context.
A transaction records each new user message and streaming assistant row before
generation starts. Regeneration appends an assistant attempt for the latest
user message and keeps earlier attempts. The host checkpoints output after
500 ms or 4 KiB when new output arrives and writes a terminal state and
available statistics on completion. A stopped, failed, truncated, or recovered
response remains visibly incomplete.

The adapter enforces foreign keys and uses transactional schema upgrades.
Startup marks interrupted streams while it holds the database lock, so another
instance cannot recover an active writer's messages. The database lives under
the OS local application data directory. Its exact path appears in Settings.
On Unix, the app creates the chat directory and database with owner-only
permissions. Ordinary SQLite persistence is **not encrypted at rest**.

## Inference context

Each request preserves the conversation's system instructions and latest user
message. Earlier completed user and assistant pairs enter the model context
in order. The latest assistant attempt determines completeness for an earlier
turn. The model's embedded Jinja template preserves roles and controls how
stored reasoning delimiters are interpreted. The native tokenizer counts
the rendered prompt, including delimiters and the generation prompt. The core
reserves the requested output budget and selects a fitting suffix of earlier
turns. It rejects a request when the mandatory messages alone cannot fit.
The UI reports the number of otherwise eligible turns omitted to fit context.

Only one generation runs at a time. Native inference stays on the named
worker thread. A fresh context and sampler are released after each generation.
Cancellation is cooperative between prompt batches and decode steps. Thinking
and final answer tokens share the same output limit. The view separates
reasoning when it recognizes the model's thinking delimiters.

## Permissions and trust

A native picker authorizes a local GGUF path, which Rust stores in the model
reference table. The view receives model reference IDs and file names. Load
requests resolve IDs in Rust, including references to drives that may be
disconnected. The main window capability grants only the named chat, model,
and preference commands. The release content security policy permits embedded
assets and Tauri IPC.

Generated Markdown is untrusted. The view disables raw HTML, external images,
and navigation links in model output. Copy controls copy text on user action.
Conversation content stays in the application and its database. Diagnostic
errors omit prompt and response content. Use trusted local GGUF files and
keep them stable while loaded because the native engine can memory-map weights.

## Dependency choices

- llama-cpp-2 and llama-cpp-sys-2 0.1.161 use llama.cpp revision
  d81235049384534c167caea52b85a694f6103d14. The bindings are MIT or Apache-2.0
  and llama.cpp is MIT. The app uses the CPU build with zero GPU layers.
  [Bindings](https://docs.rs/llama-cpp-2/0.1.161/llama_cpp_2/)
- MiniJinja and its Python compatibility helpers 2.24.0 render templates
  locally. Both are Apache-2.0. [MiniJinja](https://docs.rs/minijinja/2.24.0/minijinja/)
- Rusqlite 0.43.0 is MIT. Its bundled SQLite engine compiles with the C
  toolchain already used by CI. [Rusqlite](https://docs.rs/crate/rusqlite/0.43.0)
- React Markdown 10.1.0 is MIT and supports React 18 and newer.
  [React Markdown](https://github.com/remarkjs/react-markdown)
- RFD 0.15.4 is MIT and uses GTK3 on Linux.
  [RFD](https://docs.rs/rfd/0.15.4/rfd/)
