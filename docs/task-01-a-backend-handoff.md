# Task 01-A backend handoff

Use the existing Tauri `invoke` and `Channel` APIs. The registered command
names and capability list are unchanged. Option and result fields from
the desktop host use camel case. Native observation payloads preserve
their existing tool-specific keys. Run the frontend against the backend
in this checkout. The backend changes require two frontend behaviors:
send an explicit thinking choice and render successful native results
independently of the final model answer.

## Chat and model controls

```ts
type ChatOptions = { enableThinking?: boolean; maxTokens?: number };
type ChatState = "streaming" | "complete" | "cancelled" | "interrupted"
  | "failed" | "truncated";
type FinishReason = "end_of_generation" | "token_limit" | "cancelled";
type GenerationStats = {
  reason: FinishReason;
  promptTokens: number;
  promptTokensPerSecond: number | null;
  generatedTokens: number;
  timeToFirstTokenMs: number | null;
  elapsedMs: number;
  tokensPerSecond: number;
  excludedTurns: number;
};
type ChatMessage = {
  id: number;
  position: number;
  role: "system" | "user" | "assistant";
  state: ChatState;
  content: string;
  createdAt: number;
  stats: GenerationStats | null;
};
type ChatEvent =
  | { kind: "started"; user: ChatMessage | null; assistant: ChatMessage }
  | { kind: "delta"; id: number; text: string };
type ModelSettings = {
  contextLength: number;
  threads: number;
  temperature: number;
  maxTokens: number;
  topP: number;
  topK: number;
  enableThinking: boolean;
};
type ModelReference = { id: number; fileName: string; settings: ModelSettings };
type ModelInfo = {
  fileName: string;
  name: string;
  architecture: string;
  parameters: number;
  fileBytes: number;
  trainedContext: number;
  supportsThinking: boolean;
  config: { contextLength: number; threads: number };
  loadMs: number;
};
type InferenceStatus = {
  phase: "empty" | "loading" | "ready" | "generating" | "unloading";
  model: ModelInfo | null;
};
type Preferences = { appearance: "system" | "light" | "dark"; defaults: ModelSettings; enterToSend: boolean };
type Bootstrap = {
  preferences: Preferences;
  databasePath: string;
  models: ModelReference[];
  activeModelId: number | null;
  inference: InferenceStatus;
};
type IpcError = { code: string; message: string };
```

| Command | Invoke arguments | Result |
| --- | --- | --- |
| `bootstrap` | None | `Bootstrap` |
| `list_conversations` | `{ query }` | Saved conversation summaries matched against a search string |
| `choose_model` | None | `ModelReference` or `null` from the native GGUF picker |
| `inference_status` | None | `InferenceStatus` |
| `load_model` | `{ modelId, settings }` | Model info, including `supportsThinking`, `config`, `trainedContext` and `fileName` |
| `save_model_settings` | `{ modelId, settings }` | `void`. Unload first to change the loaded context length or CPU threads |
| `save_preferences` | `{ preferences }` | `void`. Includes default settings for new model references |
| `unload_model` | None | `void` |
| `send_message` | `{ id, content, options?, events: Channel<ChatEvent> }` | Terminal assistant `ChatMessage` |
| `regenerate` | `{ id, options?, events: Channel<ChatEvent> }` | New terminal assistant attempt for the latest user message |
| `stop_generation` | None | `boolean`. Wait for the pending send or regenerate result to show terminal state |
| `get_conversation` | `{ id }` | Saved conversation and messages for reconciliation after reload or IPC failure |

- Default the composer to direct mode. Pass `{ enableThinking: true }`
  only when the user opts into thinking for the send or regeneration and
  the ready model supports it. Missing `options` or `enableThinking`
  means direct mode, even if the saved model setting is true. Use the
  saved setting to inform the controls, with an explicit request choice.
- New model settings default to 768 output tokens and a 2048-token
  context. A turn uses the saved max unless `options.maxTokens` is
  supplied. Legacy 256-token values remain saved. Offer the user a
  larger budget through settings or a one-turn override. The integer
  budget is 1–4096, strictly below the loaded context length, and must
  leave room for the rendered system and latest user message. A
  `context_limit` error is actionable through shorter input, a smaller
  output reservation, or a larger loaded context.
- Append deltas by assistant message ID and parse the **accumulated**
  content. A leading `<think>` begins reasoning. `</think>` separates
  the answer, and either tag can cross delta boundaries. The Qwen3.5
  prompt-prefilled opener is normalized to a streamed marker. It is
  synthetic and `generatedTokens` still counts actual model tokens.
- `complete` requires normal model termination and a nonblank answer.
  `truncated` means the token limit was reached, including when the
  model produced only reasoning. `failed` with `end_of_generation`
  in stats means the model ended without an answer. Show a stopped or
  incomplete response as such, and offer Regenerate. An IPC rejection
  has `{ code, message }` and may leave a durable failed assistant row.
  Refresh the conversation after a rejection. Do not wait for a
  separate terminal channel event.

## Agent tasks and native output

Keep Agent task IDs as decimal strings. Execution `operationId` is an
opaque string used for matching events and answering an active approval.

```ts
type TaskId = string;
type TaskState = "created" | "planning" | "validating" | "awaiting_authorization"
  | "authorized" | "executing" | "observing" | "completed" | "denied"
  | "cancelled" | "failed" | "timed_out" | "interrupted";
type ToolId = "math.calculate" | "device.info" | "files.search" | "files.read"
  | "apps.list" | "apps.launch";
type Execution = { operationId: string; tool: ToolId; status: string; error: string | null };
type ToolObservation =
  | { tool: ToolId; status: "success"; result: unknown }
  | { tool: ToolId; status: "failure"; error: IpcError };
type NativeResult = { record: Execution; observation: ToolObservation };
type BudgetUsage = { toolCalls: number; modelCalls: number; retries: number; generatedTokens: number };
type ApprovalTarget = { displayName: string; desktopFile: string; executable: string };
type AgentTaskSummary = {
  id: TaskId;
  state: TaskState;
  createdAt: number;
  updatedAt: number;
  finalized: boolean;
  usage: BudgetUsage | null;
  error: string | null;
};
type AgentTaskDetail = {
  task: AgentTaskSummary;
  events: { sequence: number; createdAt: number; event: Record<string, unknown> }[];
  executions: (Execution & { createdAt: number; updatedAt: number })[];
  audit: { namespace: string; sequence: number; createdAt: number; record: Record<string, unknown> }[];
  nativeResults: NativeResult[];
};
type AgentEvent = { taskId: TaskId } & (
  | { kind: "started"; taskTimeoutMs: number }
  | { kind: "state_changed"; state: TaskState }
  | { kind: "tool_proposed"; tool: ToolId }
  | { kind: "proposal_rejected"; code: string }
  | { kind: "dispatch_started"; operationId: string; tool: ToolId }
  | { kind: "approval_required"; operationId: string; tool: ToolId; expiresInMs: number; target: ApprovalTarget | null }
  | { kind: "execution"; record: Execution }
  | ({ kind: "observation" } & NativeResult)
  | { kind: "finished"; state: TaskState; usage: BudgetUsage; answer: string | null;
      error: IpcError | null; executions: Execution[]; nativeResults: NativeResult[]; journalIncomplete: boolean }
  | { kind: "failed"; error: IpcError }
);
```

| Command | Invoke arguments | Result |
| --- | --- | --- |
| `start_agent_task` | `{ prompt, taskTimeoutSeconds? }` | Decimal `TaskId`. Timeout defaults to 300 seconds and accepts 1–600 |
| `cancel_agent_task` | `{ taskId }` | `boolean`. Keep listening until a terminal event |
| `respond_agent_approval` | `{ taskId, operationId, approve }` | `void` or `IpcError`. Use the exact live operation and trusted target |
| `list_agent_tasks` | `{ before?: TaskId, limit? }` | `AgentTaskSummary[]` |
| `get_agent_task` | `{ taskId }` | `AgentTaskDetail` |

- Subscribe to `agent-event` before starting. An event may arrive before
  the command returns its ID. Queue it briefly and match the exact task.
  Use both `finished` and `failed` as terminal signals. Permit
  cancellation during confirmation. Approve only with a present target,
  within `expiresInMs`, for the displayed executable and desktop entry.
- Show **Native results** as a separate section whenever there are
  successful observations, including when `answer` is `null` or
  `state` is `failed`. Merge live `observation` events and
  `finished.nativeResults` by `operationId`. Recover the latest task's
  native results from `get_agent_task` if an event was missed. A later
  finished task replaces the in-memory cache, and restart clears it.
  SQLite continues to store dispatch and outcome metadata.
- `files.search` returns `{ paths: string[], truncated: boolean }`. These
  are relative to the chosen directory. Render them as text and mark
  `truncated` as potentially incomplete, including a native bound or a
  skipped entry that could not be resolved safely. The model may receive a smaller
  preview while the UI still receives the complete bounded observation.
  `files.read` uses `{ content, file_bytes, truncated }`. `apps.list` uses
  `{ applications: { app_id, display_name }[], truncated }`. Calculator
  returns `{ value }`. Device values retain their existing snake-case
  keys. Launch returns `{ app_id, dispatched }`.
- `model_context_limit` suggests shorter input or a larger loaded
  context. `decision_output_limit` suggests a shorter answer. In both
  cases leave native results visible. Show the error message separately
  from `answer`, and never automatically retry a completed operation or
  render model text as a tool request. File names, native content and
  model answers are untrusted display data.

## File and folder grants

```ts
type AgentScopeDisplay = {
  id: string;
  displayName: string;
  kind: "file" | "directory";
  access: "read_only";
  state: "active" | "expired" | "revoked";
  remainingMs: number;
};
```

`choose_agent_file` and `choose_agent_directory` take no arguments and
return `AgentScopeDisplay | null`. `list_agent_scopes` takes no arguments
and returns an array. Show the basename, file or folder kind, read-only
access, and current state. `remainingMs` is relative to response time,
and inactive grants return zero. Refresh the list at expiration and after
picker or revoke actions. Call `revoke_agent_scope` with
`{ scopeId: grant.id }`; it returns a boolean. `grant.id` is a dialog
session reference for revocation. Keep it in UI state and omit it from
visible copy and task prompts. The native path, descriptor and policy
scope ID stay behind Rust. Task audit records delivered by IPC omit
`scopeId`. Selection is session-only and the model gets a bounded set of
active grants with display names and backend IDs through trusted context.
