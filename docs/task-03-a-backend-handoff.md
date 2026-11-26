# Task 03-A backend handoff

The existing Chat and Agent views keep their conversation IDs and event
channels. Task 03-B owns continuation controls, document presentation,
and the revised settings experience.

## Chat recommendation and budgets

| Tauri command | Arguments | Result |
| --- | --- | --- |
| `recommended_chat_settings` | None | `{ settings: ModelSettings, trainedContext: number \| null }` |
| `send_message` | `{ id, content, options?, events }` | Existing `Message` |
| `regenerate` | `{ id, options?, events }` | Existing `Message` |

`ModelSettings` keeps its existing flat fields. The recommendation uses
8,192 context tokens, 4,096 output tokens, four CPU threads, and Thinking
off. When the active model advertises less context, the recommendation
uses its trained capacity and suggests half of that context for output,
up to 4,096 tokens. A null `trainedContext` means the loaded model could
not be used to check the recommendation. Review the settings and apply
them explicitly with the existing save/load commands. Context length
changes require a reload. The recommendation only reads state.

Stored limits including 256 and 768 remain valid. A normal request uses
its saved output limit as a ceiling. The backend measures the rendered
template with the actual model tokenizer, reserves a quarter of context
for ordinary output or a third for Thinking (each capped at the requested
limit), fits recent history, then uses remaining capacity up to the
ceiling. Very long mandatory input can reduce the reservation. The
explicit `options.maxTokens` on Send or Regenerate keeps its existing
semantics and must fit alongside the mandatory input. An input that
cannot fit with any generation space returns `context_limit`. The
existing Agent decision budget is separate.

```ts
type OutputBudget = {
  requested: number;
  effective: number;
  contextLength: number;
};

type ChatStats = ExistingGenerationStats & {
  outputBudget?: OutputBudget;
};
```

Expose the difference between requested and effective output when it
matters. `excludedTurns` continues to count earlier turns excluded from
model context. Thinking and the final answer share the same effective
output budget.

## Continuation

| Tauri command | Arguments | Result |
| --- | --- | --- |
| `chat_continuation_remaining` | `{ id: number, messageId: number }` | Remaining requests, 1–3, or a typed IPC error |
| `continue_chat` | `{ id: number, messageId: number, maxTokens?: number, events: Channel<ChatEvent> }` | Updated existing assistant `Message` |
| `stop_generation` | None | Existing boolean. Cancels a running Chat continuation |

Only the latest Chat assistant attempt can continue. Its state must be
`truncated` with `stats.reason === "token_limit"`, a persisted native
replay seed, and the original model selected. The backend verifies the
reconstructed model/prompt fingerprint before decoding. Responses saved
before the new replay schema can be regenerated if the user wants a
continuable attempt. Agent replies use their own task operations and
remain ineligible.

The optional continuation `maxTokens` is a ceiling of 1–4,096 **new**
tokens. By default the backend uses the current saved model output
ceiling. Sampling settings and Thinking mode come from the original
attempt. The original context and sampled assistant prefix occupy part
of the loaded window. No earlier turns are dropped in order to extend
that prefix. There are at most three continuation requests per attempt
and at most 8,192 sampled tokens across the full attempt. A preflight
failure after reservation consumes one request, restores the old
truncated message if it wrote no new text, and returns the typed error.

```ts
const remaining = await invoke<number>("chat_continuation_remaining", {
  id: 17, messageId: 125
});
const assistant = await invoke<Message>("continue_chat", {
  id: 17, messageId: 125, maxTokens: 1024, events
});
```

The channel uses the existing payloads. On `started`, `user` is null
and `assistant` already contains the saved prefix with state `streaming`.
Update the existing attempt by message ID. On `delta`, append only
`text` to that same message ID. The returned message and a refreshed
`get_unified_conversation` reconcile durable state.

```json
{"kind":"started","user":null,"assistant":{"id":125,"position":1,"role":"assistant","state":"streaming","content":"Here is the beginning…","createdAt":100,"stats":null}}
```

At completion, `generatedTokens` is cumulative. `outputBudget`,
`promptTokens`, timing, and throughput describe the latest segment.
A later token limit can remain continuable within the caps. A stop saves
the suffix already received and marks the response `cancelled`.
A restart during generation marks the attempt `interrupted` and retains
the last checkpoint. Legacy and ineligible attempts return
`invalid_config`. Wrong or unloaded models, insufficient remaining
context, backend errors, and cancellation keep their existing typed
errors or states. Native token IDs and any incomplete UTF-8 tail are
internal to Rust and SQLite.

## PDF search and scoped reading

`files.search` keeps its existing argument and result fields. A native
result now includes `extension: string | null`. The string is the
lowercased final suffix tested by the adapter. Null indicates no
extension filter. Render the native paths and coverage warning as the
authoritative result. A case-insensitive `pdf` extension match includes
`notes.PDF` and excludes `pdf-notes.zip` and `pdf-notes.txt`.

```json
{"kind":"tool_call","tool":"files.search","arguments":{"scope_id":"current_directory_scope","extension":"pdf"}}
```

`files.read` accepts optional `pdf_pages`. `first_page` and
`last_page` are one-based inclusive. At most four pages per request are
supported. Without a range, the reader selects pages one through four or
the end of the document. For a single-file grant omit `relative_path`.
For a folder grant use the validated relative document path. PDF returns
the page shape below under the existing `observation.result` envelope.
TXT returns the existing `content` shape.

```json
{"kind":"tool_call","tool":"files.read","arguments":{"scope_id":"current_directory_scope","relative_path":"notes.PDF","pdf_pages":{"first_page":2,"last_page":3}}}
```

```ts
type PdfResult = {
  page_count: number;
  first_page: number;
  last_page: number;
  pages: { page_number: number; text: string; truncated: boolean }[];
  file_bytes: number;
  truncated: boolean;
};

type FileSearchResult = {
  paths: string[];
  extension: string | null;
  truncated: boolean;
};
```

```json
{
  "tool": "files.read", "status": "success",
  "result": {
    "page_count": 5, "first_page": 2, "last_page": 3, "file_bytes": 918,
    "truncated": true,
    "pages": [
      {"page_number": 2, "text": "Synthetic page two.", "truncated": false},
      {"page_number": 3, "text": "Synthetic page three.", "truncated": false}
    ]
  }
}
```

Recognize a PDF result by the validated `pages` array and page numbers.
Render source coverage explicitly, for example “Pages 2–3 of 5”. A true
outer `truncated` means omitted pages or clipped returned text. A true
page flag applies to that page's text. Keep document text inert and treat
it as untrusted. The model may receive smaller previews. Preserve the
distinction between a native preview, a summary of available pages, and
a claim about the rest of the document. Native PDFs and extracted text
stay transient. Saved model answers can quote extracts, as with TXT.

| Code | User-facing meaning |
| --- | --- |
| `pdf_encrypted` | Choose an authorized unencrypted copy |
| `pdf_no_text` | The selected pages have no extractable text layer. OCR is outside this feature |
| `pdf_invalid` | The file is invalid or damaged |
| `pdf_unsupported` | The document uses a feature or encoding the extractor cannot handle |
| `pdf_limit` | A file, page count, extraction, or parser resource limit was exceeded |
| `pdf_unavailable` | Local parser dependencies or Linux isolation are unavailable |
| `invalid_arguments` | Page range is invalid or extends beyond the document |
| `permission_denied`, `scope_kind_mismatch`, `cancelled`, `timed_out` | Existing gate and lifecycle errors still apply |

## Unified Agent additions

Existing command names and event kinds remain stable. The Agent journal
summary adds nullable `conversationId: number`, `turnId: string`,
`excludedTurns: number`, and `errorMessage: string`. A null
`conversationId` identifies a standalone task. The existing `error`
field contains its error code.

`UnifiedTurn` adds nullable `taskState`, `taskError`,
`taskErrorMessage`, and `excludedTurns`. Read these fields for saved
outcome and context notes. Fetch `get_agent_task` when opening the
detailed journal or reviewing available native results. A null omission
count means there is no saved measurement. A zero count is an actual
saved count.

`get_active_agent_turn` adds `tool: ToolId | null`,
`startedAt: number | null` in Unix milliseconds, and
`taskTimeoutMs: number`. `startedAt` can be null while the worker is
starting, and the tool is null when there is no active tool. An approval
object now includes its authoritative `tool: ToolId` plus the existing
operation ID, expiry, and trusted target. Cancel and approve with the
exact live task and operation IDs. Existing decimal-string task and turn
identifiers are unchanged. Saved error messages come from fixed code
mappings.

The current frontend uses the new fields for the sidebar and existing task
copy. No new document or continuation view is included in this assignment.

## Markdown tables

The current renderer uses CommonMark without the GFM parser extension.
The valid test input below produced a paragraph in the installed parser.
A missing delimiter row also produced a paragraph. The original manual
answer text was unavailable, so its syntax cannot be classified.

```markdown
| File | Pages |
| --- | --- |
| alpha.pdf | 2 |
```

For Task 03-B, enable the maintained `remark-gfm` plugin on the existing
React Markdown component, then exercise a valid table and an invalid
delimiter fixture. Preserve the inert URL transformation, skipped HTML,
and blocked images. Validate table overflow at the small desktop
viewport without changing the thread layout. [GFM parser documentation](https://github.com/remarkjs/remark-gfm)
