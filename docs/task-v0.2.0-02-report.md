# v0.2.0 Task 02 implementation report

Semantic and hybrid retrieval run locally on `feature/knowledge-v0.2.0`. Keyword retrieval continues to work independently. Task 01's import path, original keyword IPC contract, and local work are preserved.

## Model and schema

The supplied 67,308,128-byte GGUF matches SHA-256 `f0b2fef971e8366438bfd2d9aefea1b0115919389448806d290237f638bae999` and the converter's Git LFS object at revision `d32f8c040ea3b516330eeb75b72bcc2d3a780ab7`. Both upstream repositories declare MIT. [BAAI model card](https://huggingface.co/BAAI/bge-small-en-v1.5), [CompendiumLabs model card](https://huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf/tree/d32f8c040ea3b516330eeb75b72bcc2d3a780ab7), [Published file metadata](https://huggingface.co/api/models/CompendiumLabs/bge-small-en-v1.5-gguf/tree/d32f8c040ea3b516330eeb75b72bcc2d3a780ab7)

GGUF v3 metadata identifies a noncausal 12-layer BERT model with F16 weights, a 512-token context, 384 dimensions, and CLS pooling. Native evaluation passed with the existing llama-cpp-2 0.1.161 and llama.cpp revision `d81235049384534c167caea52b85a694f6103d14`. The worker loads verified bytes from a sealed Linux memory file. It uses CLS pooling, L2 normalization, and BAAI's search instruction on queries only. [BAAI pooling configuration](https://huggingface.co/BAAI/bge-small-en-v1.5/blob/main/1_Pooling/config.json), [Pinned llama.cpp](https://github.com/ggml-org/llama.cpp/tree/d81235049384534c167caea52b85a694f6103d14)

Schema 7 adds document revisions and two tables. `knowledge_embedding_indexes` records the revision, model fingerprint, preprocessing version, and window count. `knowledge_embedding_windows` stores token ranges and 1,536-byte little-endian float vectors tied to their parent chunks. Replacement invalidates the old generation in the same transaction as the new text. Deletion cascades to vectors. A cancelled or interrupted reindex preserves the last committed generation. Backup of the schema 6 database is the downgrade path.

Semantic mode uses exact cosine over compatible windows and ranks each chunk by its best window. Hybrid uses reciprocal rank fusion with k=60 and at most 60 candidates per ranking. Partial coverage is returned explicitly. See the [IPC handoff](task-v0.2.0-02-handoff.md) for the command contracts and resource limits.

## Synthetic retrieval measurements

One run on an Intel Core i5-8500 with two embedding threads used eight synthetic documents, eight chunks, and nine embedding windows. The three exact queries used `ZX-4179`, `quartzbridge`, and `propylene glycol`. The three paraphrases asked about a commuter rail service, honey-producing insects, and chilling a citrus dessert.

| Mode | Exact top-1 | Paraphrase top-1 | Median query time | Range |
| --- | ---: | ---: | ---: | ---: |
| Keyword | 3/3 | 0/3 | 0.18 ms | 0.12–0.21 ms |
| Semantic | 3/3 | 3/3 | 11.28 ms | 9.61–12.68 ms |
| Hybrid | 3/3 | 3/3 | 11.49 ms | 9.08–12.89 ms |

Verified model loading took 816 ms. Indexing took 545 ms including SQLite writes, or 16.5 windows/s. Vectors occupied 13,824 bytes of payload and grew the SQLite file by 45,056 bytes. Peak process RSS was 192,876 KiB (188.4 MiB), measured by Linux `VmHWM` across loading and retrieval. Dense timings include worker dispatch and query embedding. These are small-corpus figures with an already loaded model. Larger libraries, other languages, and concurrent application memory were outside this measurement.

## Verification

- Formatting, `git diff --check`, locked release Clippy with all workspace targets, and frontend type-check and asset build passed. Vite emitted its asset-size warning.
- The locked release workspace suite passed 142 tests. The default run skipped 33 opt-in tests. Focused coverage included migration with existing FTS rows, persisted ranking and source references, filtering, reindex rollback, revision changes, deletion, quota enforcement, and incompatible or corrupt vectors.
- The opt-in real-model tests passed for unit-vector output, paraphrase similarity, multi-window input, and cancellation. The packaged WebView passed through real native model selection, indexing progress and cancellation, partial coverage, search and model unload. The Task 01 native import and keyword fixture also passed on schema 7.
- The standalone Linux release executable opened under Xvfb. On reopening the isolated fixture profile, the one surviving vector was 1,536 bytes and foreign-key checks passed.

The current model targets English retrieval. Exact scan cost grows with the vector count, subject to the handoff caps. Model selection is session-local and vector indexing is explicit per document. The UI and RAG answer generation remain outside Task 02.
