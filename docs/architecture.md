# Local inference architecture

The desktop app is a modular monolith. The React view, Tauri host, and
inference worker run in one application. The release embeds its frontend assets.

| Layer | Responsibility |
| --- | --- |
| `crates/agent-core` | Provider trait, validated model and generation settings, cancellation token, errors, and result types |
| `crates/inference` | Worker, operation state, GGUF validation, and the optional llama.cpp provider |
| `apps/desktop/src-tauri` | Native file picker, selected path, Tauri request handlers, and streaming IPC |
| `apps/desktop/src` | Test screen, streaming text, control state, and statistics |

## Model selection and permissions

The main window can open a native single-file chooser. Rust stores the selected
path in memory and returns the base file name to the view. The load command
accepts model settings and obtains its path from that stored selection. The
llama.cpp provider checks that the selection is a regular GGUF file and rejects known split
file names before opening it in llama.cpp. The host serializes picker and load
requests. The capability grants only the six commands needed by this screen.
The release CSP permits embedded assets and Tauri IPC.

Load and generation can be resource intensive. Use trusted, reasonably sized
GGUF files and keep them in place while loaded because llama.cpp can memory-map
their contents. The native parser and inference engine run in the application
process.

## Work and cancellation

The worker reserves each operation under a mutex before submitting it to a
named OS thread. Load requires an empty worker and generate or unload requires a
ready model. Concurrent work is rejected. The worker owns the provider and Rust
drops each generation context and sampler before another request runs. Unload
releases the weights. Shutdown cancels active generation and joins the worker.
Cancellation is cooperative and checked between prompt batches and decoding
steps. Loading must finish before shutdown can complete.

The provider renders the model's embedded Jinja chat template for one user
message and an assistant generation prompt. It supplies special token strings
and requests thinking to be disabled when the template supports that option.
The template runs with a bounded evaluation budget and no file loader. Models
without a supported template report an error. The tokenizer reads the rendered
special tokens, and the vocab's end-of-generation token check stops sampling.
The worker sends complete UTF-8 text deltas over the request's Tauri channel.
The command returns the finish reason and statistics.

## Dependencies

- `llama-cpp-2` and `llama-cpp-sys-2` 0.1.161 wrap bundled llama.cpp revision
  `d81235049384534c167caea52b85a694f6103d14`. The Rust bindings are MIT or
  Apache-2.0 and llama.cpp is MIT.
  Default features are disabled and the app sets GPU layers to zero.
  [Bindings documentation](https://docs.rs/llama-cpp-2/0.1.161/llama_cpp_2/)
  [llama.cpp revision](https://github.com/ggml-org/llama.cpp/tree/d81235049384534c167caea52b85a694f6103d14)
- MiniJinja and its Python compatibility helpers 2.24.0 render embedded
  templates. Both are Apache-2.0. [MiniJinja](https://docs.rs/minijinja/2.24.0/minijinja/)
- RFD 0.15.4 uses the GTK3 libraries already used by Tauri on Linux. It is MIT.
  [RFD](https://docs.rs/rfd/0.15.4/rfd/)

The application remains independent of inference servers and cloud APIs.
Project license selection is deferred.
