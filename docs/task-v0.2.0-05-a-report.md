# v0.2.0 Task 05-A — Final runtime verification

**PASS.** Both remaining cancellation paths passed with deterministic checkpoints and real desktop button clicks. The ten-cycle resource session completed without a crash or swap use. The application released roughly 1.2 GiB after each unload. Its post-unload footprint rose during the session, so this run leaves longer-term memory behavior unverified.

## Cancellation

| Desktop action | Observed outcome |
| --- | --- |
| **Cancel import** while the standalone release displayed **Extracting text…** | The real Poppler `pdftotext` child was paused externally with `SIGSTOP` after native picker and consent, using a synthetic 3.9 MB PDF. An X11 click on the release UI reaped the child in **8.3 ms**. OCR confirmed **Import cancelled. Nothing was added.** within **1,340 ms** of the click. This is an upper bound that includes polling and OCR. A subsequent TXT import succeeded. The final database contained one document, one chunk and one search row, all from that TXT file. SQLite integrity and foreign-key checks passed. |
| **Stop generating** while Chat displayed **Searching 1 selected document…** | The release-profile desktop fixture ran the real BGE embedding of the Chat query and briefly held its worker at a cancellable test-only checkpoint before returning the vector. An X11 click produced a trusted event on the real composer button. The worker observed Stop, the UI reported **Stopped before the answer started** in **13 ms**, Send became available with the original draft in **25 ms**, and a subsequent semantic search returned a hit within **47 ms** of the click. The database contained no conversation, message, attempt or citation from the stopped first send. SQLite integrity and foreign-key checks passed. |

The normal windows were too brief for reliable manual timing. PDF extraction used the existing unmodified executable. Dense Chat used the existing Tauri test harness with the application UI and production IPC in a release-profile test build. Its checkpoint and the temporary fixture scripts were restored after verification. No production delay was added.

## Resource session

Intel Core i5-8500, six CPU cores and 15,148 MiB RAM. The session ran offline in an isolated profile and network namespace under Xvfb. It used bundled LFM2.5 1.2B at 1,024 context tokens and four threads, and the supported local BGE small English model. Each cycle loaded both models, indexed a synthetic document, retrieved via keyword, semantic and hybrid search, ran a short LFM Chat turn and unloaded both models. The first cycle indexed a 159-passage archive. The next nine indexed single-passage Markdown or PDF fixtures. Each cycle also saved a separate Chat turn.

| Measurement | Observed |
| --- | --- |
| Fresh standalone launch to ready | **186 ms** to window mapped and **4.20 s** to the visible LFM ready indicator, one launch with a fresh profile and bundled-resource verification. The standalone default used an 8,192-token context. Warm fixture LFM loads took **394–410 ms** at 1,024 tokens. |
| CPU and system RAM | **58.2 s** sampled at about **143 ms** intervals. App and WebKit CPU averaged about **103%** across the run and **205%** during the large index build, with a sampled peak of **406%**. One fully occupied core is 100%. System available RAM stayed above **6.89 GiB** and free swap was unchanged. |
| Loaded RSS | Application **1,448–1,491 MiB** and WebKit children **240–241 MiB** at the post-turn samples with both models loaded. Separate sampled peaks were **1,493 MiB** and **252 MiB**, respectively. Summed WebKit RSS can count shared pages more than once. |
| Unload and drift | Application RSS dropped approximately **1,231–1,234 MiB** per cycle by the sample about **850 ms** after both unloads. Post-unload application RSS rose from **214 to 259 MiB** across ten cycles, including **15 MiB** from cycle four onward. WebKit stayed near **240–241 MiB** after each unload. RSS includes the whole application and cannot isolate allocator retention, Chat history growth or a leak. No plateau was established in ten cycles. |
| Indexing and navigation | The 159-passage index took **18.0 s** including polling. A focused navigation sample confirmed that the Memory and Knowledge headings rendered in **195 ms** and **139 ms**, respectively, while indexing remained active. These timings cover the interaction through the destination and an animation-frame callback. They do not measure frame rate. |

The supplemental navigation fixture recorded both timings, then required forced cleanup after requesting harness exit during the active index. The ten-cycle fixture exited normally after unloading both models. Standalone shutdown during an active index was outside this focused rerun.

## UI and checks

- Both memory deletion dialogs now describe removal from the app’s local database. Verified both dialogs in the rebuilt Linux app.
- The answer footer reports passages and memories **supplied to the model**. Retrieved passages without valid model citations still display **Uncited**. Verified the historical unavailable-evidence state at **1,120, 440 and 320 px**, where the heading and availability status wrap separately.
- Changes were limited to `MemoryView.tsx`, `ChatView.tsx`, `Sources.tsx`, `styles.css`, `knowledge.ts` and its existing frontend tests. **57 frontend tests passed**. TypeScript, the Vite production build, the cached Linux release build and `git diff --check` passed. Vite retained its chunk-size warning. The previously completed broad Rust and acceptance suites were outside this targeted rerun.

The existing worktree changes were preserved. Task 06 and release publication remain outside this verification.
