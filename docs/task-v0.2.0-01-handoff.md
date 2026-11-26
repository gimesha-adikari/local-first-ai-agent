# v0.2.0 Task 01 handoff

The local knowledge library is available through the existing main-window Tauri capability. A frontend can call these commands without a loaded model. The host owns file selection and retention consent.

## IPC

| Command | Invoke arguments | Result |
| --- | --- | --- |
| `knowledge_start_import` | None | Numeric `jobId`. Opens a native picker. |
| `knowledge_reimport` | `{ documentId }` | `jobId`. Checks the existing document, then opens the picker and asks for explicit replacement consent. |
| `knowledge_import_status` | Optional `{ jobId }` | `{ jobId, state }` for the current or requested job, or `null` when unavailable. |
| `knowledge_cancel_import` | `{ jobId }` | Boolean indicating whether cancellation was accepted. During selection or confirmation, dismiss the native dialog. |
| `knowledge_list_documents` | `{ beforeId?, limit }` | Metadata array, newest ID first. Pass the last ID as the next page's exclusive cursor. Limit 1–100. |
| `knowledge_get_document` | `{ documentId }` | Current metadata. |
| `knowledge_search` | `{ request: { query, documentIds?, limit } }` | Ranked hits. Search all documents by omitting `documentIds` or passing `null`. An empty array yields no matches. Limit 1–20. |
| `knowledge_delete_document` | `{ documentId }` | Resolves with no payload after deleting metadata, chunks and indexed text. Ask for confirmation in the eventual library UI. |

Document IDs are positive JavaScript-safe integers. Missing IDs in details or deletion report `knowledge_not_found`. Reimport reports a missing target in its terminal job state. Search ignores absent IDs in a valid filter. Command errors have `{ code, message }` when reported by Rust, and Tauri rejects invalid argument types before dispatch. Common codes include `invalid_arguments`, `busy`, `knowledge_limit`, `permission_denied`, `storage_failed` and the existing `pdf_*` codes. Failed jobs deliver the same error shape in their status.

Poll `state.phase` through `selecting`, `confirming`, `reading`, `extracting`, `indexing`, `committing` and optionally `cancelling`. Terminal states are `completed` with `outcome`, `failed` with `error`, or `cancelled`. `outcome` is `{ kind, documentId }` with `imported`, `replaced` or `duplicate`. A duplicate returns the existing document even if the selected name differs. A duplicate on reimport leaves the target intact. A successful replacement retains the document ID and creation time. The job slot is process-local and retains one terminal status until the next job starts. Refresh the document list after an app restart.

Metadata fields are `id`, `name` (basename), `format` (`txt`, `markdown` or `pdf`), `fileBytes`, `textBytes`, `chunkCount`, `indexingStatus` (`ready` or `partial`), `pdfCoverage` and Unix millisecond `createdAt` and `updatedAt`. PDF coverage contains `pageCount`, `pagesExamined` and `pagesWithoutText`. Other formats return `null`. Pages past the cutoff and examined pages with no extractable text make a PDF partial. A selection with no extractable text fails.

A hit includes `documentId`, `documentName`, zero-based `ordinal`, `source`, `text` and `score`. References are `{ kind: "lines", startLine, endLine }` for TXT/Markdown and `{ kind: "page", page }` for PDF. Page and line numbers are one-based and ranges are inclusive. Lines follow LF or CRLF. Ordinals can change on reimport. Lower BM25 scores rank first. Query fragments with letters or numbers are matched as literal phrases joined by AND within a chunk. SQLite's Unicode tokenizer handles case and diacritics. Queries accept at most 256 UTF-8 bytes and 16 fragments, with up to 128 document IDs. Treat document names and text as untrusted when rendering them.

## Retention and limits

The native picker accepts `.txt`, `.md`, `.markdown` and `.pdf` case-insensitively. The user separately consents to retention. Imports and replacements each require a fresh selection. No path is accepted over IPC or retained in SQLite. There is no Agent grant created by import.

TXT and Markdown are capped at 1 MiB of UTF-8 input. PDF input is capped at 8 MiB and scans at most the first 100 pages for text, with a 1 MiB parser output ceiling. A text or parser resource ceiling can fail the whole import. Chunks are at most 2 KiB, with at most 1,024 per document. The library holds up to 128 documents and 32 MiB of retained text. SQLite index and database file overhead are additional. Only one import job runs at a time. The worker has a 30-second cooperative processing deadline before commit, excluding time in native dialogs.

Deletion prevents subsequent queries from retrieving the document. Historical conversation answers may retain previous quotations, and the original file remains on disk. The local database is unencrypted. The task adds no OCR, folder scans, monitoring or network services.

See the [implementation report](task-v0.2.0-01-report.md) for migration, recovery and verification evidence.
