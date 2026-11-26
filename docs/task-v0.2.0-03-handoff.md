# v0.2.0 Task 03 frontend handoff

## Chat

Use the existing `send_message({ id, content, options?, events })` and `regenerate({ id, options?, events })` commands. Both return the existing assistant `Message` and use the existing Chat event channel. The main window supplies the current selection on every request, including regeneration.

```ts
type RetrievalMode = "keyword" | "semantic" | "hybrid";
type ChatOptions = {
  enableThinking?: boolean;
  maxTokens?: number;
  knowledge?: { mode: RetrievalMode; documentIds: number[] } | null;
};

type KnowledgeEvent = {
  kind: "knowledge";
  coverage: {
    mode: RetrievalMode;
    embeddings: {
      indexedDocuments: number;
      missingDocuments: number;
      incompatibleDocuments: number;
    } | null;
    partialDocumentIds: number[];
    unavailableDocumentIds: number[];
    retrieved: number;
  };
};
```

Omit `knowledge` or send `null` for ordinary Chat. Use 1–128 selected positive JavaScript-safe document IDs when enabling it. A successful retrieval sends a `knowledge` event before `started`. Show partial coverage when the document arrays are nonempty or dense coverage has missing or incompatible documents. `retrieved: 0` means no evidence matched the selection. The final assistant `stats.contextUsage`, when present, has `sources` and `memories` counts after model context fitting. If `sources < retrieved`, some hits did not fit the output reservation. Completed messages retain the fitted counts across reload. Coverage events are live.

Dense modes require the supported embedding model and can reject preparation with `embedding_unavailable`, `embedding_index_unavailable` or `embedding_incompatible`. A rejected preparation saves no turn. Offer indexing or an explicit mode change. Hybrid with partial indexes still includes the keyword rank by design. Keyword Chat matches a bounded set of significant question terms with BM25. Its library search counterpart retains the Task 01 literal AND behavior. Semantic and hybrid embed a bounded form of the question. Search and indexing contracts from the previous tasks still apply.

`stop_generation` also cancels contextual Chat preparation. Before a turn is saved, cancellation rejects with `cancelled`. During generation, it uses the existing persisted cancelled response. `started` and `delta` retain their existing shapes. Preparation and generation own the same operation lease.

Native Continue stays available for eligible ordinary Chat attempts. For a contextual attempt, both `chat_continuation_remaining` and `continue_chat` reject with `invalid_config`. Offer Regenerate with the current settings. This creates a new assistant message and preserves the old one. Answers from contextual attempts remain visible in conversation history but are omitted from subsequent Chat and Agent prompts. The user's original text remains saved.

## Source references

| Command | Invoke arguments | Result |
| --- | --- | --- |
| `chat_source_references` | `{ messageId }` | Array ordered by citation slot containing `citationId`, `documentId`, `revision`, `chunkOrdinal`, `documentName`, `source` and `available`. |
| `chat_resolve_source` | `{ messageId, citationId }` | Current text for the cited chunk of the original revision, or `null` when the citation cannot be resolved. |

Use an assistant message ID. An absent assistant message returns `not_found`. Source metadata is committed with the final response and can be fetched after the generation call settles. Link only markers matching a returned `citationId`, such as `S1` for `[S1]`. An unknown marker stays plain text. Stored references prove the model was supplied the indicated chunk and emitted that identifier. The model can still misattribute a claim. Render names and excerpts as untrusted text.

`chunkOrdinal` is zero-based and only identifies a chunk together with its document ID and revision. `source` is `{ kind: "page", page }` or `{ kind: "lines", startLine, endLine }`. Pages and inclusive line ranges are one-based. Deletion or replacement makes earlier citations unavailable even if the same ordinal is used again. Show that earlier answers may still contain quotations from a deleted or replaced document. The resolver reads only retained index text and accepts no filesystem paths.

## Personal memory

| Command | Invoke arguments | Result |
| --- | --- | --- |
| `memory_retrieval_enabled` | None | Persisted boolean, initially `false`. |
| `memory_set_retrieval_enabled` | `{ enabled }` | No payload. |
| `memory_create` | `{ content }` | Saved memory. |
| `memory_list` | `{ query?, beforeId?, limit }` | Page of saved memories in descending ID order. |
| `memory_edit` | `{ id, content }` | Updated memory. |
| `memory_delete` | `{ id }` | No payload. |
| `memory_clear` | None | Number of memories deleted. |

A memory has `id`, `content`, `createdAt` and `updatedAt`. IDs are positive JavaScript-safe integers and timestamps are Unix milliseconds. Explicitly save 1–1,024 UTF-8 bytes of trimmed text per memory. The database holds up to 512 memories. Listing accepts a case-insensitive substring query of at most 256 UTF-8 bytes, an exclusive ID cursor, and a limit of 1–100. Missing edit and delete IDs return `not_found`. Exceeding the count limit returns `memory_limit`.

Keep save, edit and delete actions explicit in the UI and confirm Clear All. Disabling retrieval leaves records available for management. Clearing removes the records and preserves the retrieval setting. Enabled retrieval ranks prompt term overlap across conversations and supplies up to three relevant memories as background, separate from document evidence. Earlier answers may still contain remembered details. A running generation uses the snapshot it started with. Edits, deletion and toggle changes apply to subsequent attempts.

The new commands are scoped to the main window. Memory writes occur only through their explicit management commands. Chat supplies text to inference and does not use Agent execution or permission APIs.
