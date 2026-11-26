# v0.2.0 Task 02 handoff

Semantic retrieval is available to the main Tauri window on Linux. The model is loaded separately from chat and remains available until unload or process exit. Its path stays in Rust. Stored vectors survive a restart and can be queried again after selecting the compatible model.

## IPC

| Command | Invoke arguments | Result |
| --- | --- | --- |
| `knowledge_embedding_status` | None | `{ supported, available, name, sha256, preprocessing, dimensions, contextTokens, cpuThreads }`. |
| `knowledge_choose_embedding_model` | None | Opens a native file picker. Returns model status after verification and loading, or `null` on dismissal. |
| `knowledge_unload_embedding_model` | None | Releases the model with persisted vectors retained. |
| `knowledge_embedding_coverage` | Optional `{ documentIds }` | `{ indexedDocuments, missingDocuments, incompatibleDocuments }` for current revisions and the supported model and preprocessing version. |
| `knowledge_reindex` | `{ documentId }` | Numeric `jobId`. Builds or replaces the vector generation for one document. |
| `knowledge_index_progress` | Optional `{ jobId }` | `{ jobId, documentId, phase, processedChunks, totalChunks, windowCount, error }` for the latest matching job, or `null`. |
| `knowledge_cancel_index` | `{ jobId }` | Boolean indicating whether cancellation was accepted before commit. |
| `knowledge_retrieve` | `{ mode, request: { query, documentIds?, limit } }` | `{ mode, coverage, hits }`. Modes are `keyword`, `semantic`, and `hybrid`. |

Use the original `knowledge_search` for the Task 01 array response. It keeps its existing BM25 ranking and argument limits. The new keyword mode wraps the same hits with `coverage: null`. An omitted filter or `null` covers the library. An empty filter covers no documents. Queries use at most 256 UTF-8 bytes and 16 fragments. Filters allow up to 128 positive JavaScript-safe IDs. Result limits are 1–20.

Semantic and hybrid requests require the loaded model. When selected documents exist but none has a compatible vector generation, requests fail with `embedding_index_unavailable`. With some indexed documents, semantic ranks those documents and hybrid also considers lexical matches from unindexed documents. Read `coverage` to surface incomplete indexing. Keyword search is an explicit choice and remains available during indexing. Each hit uses the Task 01 fields. Lower BM25 values rank first in keyword mode. Higher cosine and reciprocal rank fusion scores rank first in the other modes. Scores are specific to their mode.

Poll progress through `indexing`, `cancelling`, `committing` and one terminal phase of `completed`, `cancelled` or `failed`. `windowCount` is set on completion and `error` carries `{ code, message }` on failure. Common errors include `embedding_unavailable`, `embedding_incompatible`, `knowledge_revision_changed`, `knowledge_limit`, `busy` and `timed_out`. The job slot holds the latest job for this process. After replacement, deletion or restart, refresh coverage. To resume interrupted work, reindex documents whose current generation is missing or incompatible.

## Processing and storage limits

Each chunk uses up to eight 510-token WordPiece windows with 32-token overlap. CLS and SEP occupy the other two positions in the 512-token context. A result returns the parent chunk and its original page or line reference. Chunks exceeding the limits fail the index build. The maximum is 4,096 windows per document and 32,768 per library, or 48 MiB of raw vector payload across the library. SQLite overhead is additional.

The two-thread CPU worker runs one embedding operation at a time. Model picking, indexing and dense retrieval share the desktop operation lease with chat and Agent generation. Indexing has a five-minute cooperative deadline and dense queries have a 15-second deadline. A native evaluation finishes its current window before cancellation takes effect. The index swaps generations atomically. Cancellation before commit leaves any prior generation available.

See the [implementation report](task-v0.2.0-02-report.md) for provenance, schema and measured behavior.
