# Task 02-A backend handoff

A conversation owns ordered messages. An Agent execution is one task linked
to one user turn and its assistant attempt. The backend accepts multiple Agent
turns in the same conversation and shares saved conversational text between
Chat and Agent. Task 02-B owns composing and displaying the unified thread.

## Commands and identifiers

Use the conversation ID returned by the existing Chat API. The existing
conversation and message fields still use JSON numbers. The new turn ID and
the existing Agent task ID are decimal strings. Keep them as strings,
including keys and Tauri arguments. An operation ID is an opaque string.

| Tauri command | Invoke argument | Result |
| --- | --- | --- |
| `create_conversation` | None | Existing `Conversation` with an `id` and empty `messages` |
| `list_conversations` | `{ query: string }` | Existing `{ id, title, updatedAt }[]`. Searches titles and saved Chat and Agent text |
| `get_unified_conversation` | `{ id: number }` | `UnifiedConversation` |
| `rename_conversation` | `{ id: number, title: string }` | `void` |
| `set_system_prompt` | `{ id: number, systemPrompt: string }` | `void`. Chat receives these user-configured instructions |
| `start_agent_turn` | `{ conversationId: number, prompt: string, taskTimeoutSeconds?: number }` | `AgentTurnIdentity` |
| `get_active_agent_turn` | None | `ActiveAgentTurn \| null` for the current linked execution |
| `get_agent_task` | `{ taskId: string }` | Existing `AgentTaskDetail` with task state, events, executions, audit, and available session results |
| `cancel_agent_task` | `{ taskId: string }` | `boolean` |
| `respond_agent_approval` | `{ taskId: string, operationId: string, approve: boolean }` | `void` |
| `delete_conversation` | `{ id: number }` | `void`. The operation gate rejects deletion while Chat, Agent, or another exclusive operation is active |

The prompt must be nonblank, at most 8 KiB, and contain no NUL. The overall
task time is 1–600 seconds, with 300 as the default. The separate approval
window is at most 60 seconds and fits within the remaining task time.
The backend keeps the existing model-call, decision-token, retry, and
tool-call bounds.

`send_message` accepts `{ id, content, options?, events }` and
`regenerate` accepts `{ id, options?, events }`. Pass the same
`id` when changing modes. `options` contains optional `enableThinking`
and `maxTokens`, and `events` is the existing `Channel<ChatEvent>`.
Regenerate applies to the latest Chat user turn. Starting a fresh Chat
message after an Agent turn is supported. Agent always uses the constrained
decision protocol and its own cancellation command.

The legacy `start_agent_task` keeps `{ prompt, taskTimeoutSeconds? }` and
returns a task ID string. It still creates a standalone journal entry.
The current workspace can keep using it until Task 02-B switches to
`start_agent_turn`. `get_conversation` keeps its flat message wire shape.
For the unified view, read `get_unified_conversation`.

## Read models and recovery

```ts
type ConversationId = number;
type TurnId = string;
type TaskId = string;

type AgentTurnIdentity = {
  conversationId: ConversationId;
  turnId: TurnId;
  taskId: TaskId;
};

type ActiveAgentTurn = AgentTurnIdentity & {
  approval: null | {
    operationId: string;
    expiresInMs: number;
    target: { displayName: string; desktopFile: string; executable: string } | null;
  };
};

type UnifiedConversation = {
  id: ConversationId;
  title: string;
  systemPrompt: string;
  createdAt: number;
  updatedAt: number;
  turns: UnifiedTurn[];
};

type UnifiedTurn = {
  id: TurnId;
  kind: "chat" | "agent";
  user: Message;
  assistants: { message: Message; modelName: string | null }[];
  taskId: TaskId | null;
  taskTimeoutMs: number | null;
  nativeResultsAvailable: boolean;
};
```

`Message` keeps the existing `id`, `position`, `role`, `state`,
`content`, `createdAt`, and `stats` fields. Timestamps are Unix
milliseconds. Render turns and assistant attempts in returned order.
A Chat regeneration appends an assistant attempt under the same user turn.
The model name is the file basename recorded for that attempt. The model
name is null for older messages without a snapshot. A linked Agent
assistant attempt can be empty in failed, denied, or cancelled tasks. Use its
`taskId` to retrieve the exact journal status, usage and failure code.

A process restart marks unfinished linked tasks and their pending assistant
rows interrupted. Any durable dispatch intent without a known outcome is
uncertain. Reopening a turn may show a saved answer and execution metadata
while `nativeResultsAvailable` is false. Explain that session tool details
have expired or were cleared. Request a new picker grant if a follow-up needs
access to a file again.

## Events and active ownership

Subscribe to `agent-turn-event` before invoking `start_agent_turn`. The
command writes the pending turn and may emit an event before returning.
Buffer events until their `event.taskId` matches the returned identity.

```ts
type AgentTurnEvent = {
  conversationId: ConversationId;
  turnId: TurnId;
  excludedTurns: number;
  event: AgentEvent;
};
```

The nested `AgentEvent` keeps the kinds and payloads in [the previous
handoff](task-01-a-backend-handoff.md). Linked runs also emit unchanged
`agent-event` payloads for compatibility. Use one event channel per unified
controller. Merge native observations by operation ID. Persisted execution
metadata is retrieved through `get_agent_task`.

`excludedTurns` counts earlier turns omitted from model input. It includes
the bounded storage window and the highest tokenizer-backed omission observed
across decisions in the task. It does not remove messages from history.
An initial event may only know the storage omission. Refresh the saved
conversation after either `finished` or `failed` and display the terminal
error alongside any verified native result.

A remounted controller can call `get_active_agent_turn`, then
`get_agent_task` for current state. A null active response can race with a
terminal event, so reconcile with the saved journal for a known task ID.
The active lookup returns null for legacy standalone tasks. Cancel and
approve using the exact active task (and approval operation) IDs, even when
another conversation is selected. Only show Approve for a live, unexpired
approval with a trusted target. A declining, expired, or already consumed
approval never authorizes dispatch.

Representative start and observation:

```ts
const owner = await invoke<AgentTurnIdentity>("start_agent_turn", {
  conversationId: 17,
  prompt: "Read the first matching file from the previous search.",
  taskTimeoutSeconds: 300
});
// { conversationId: 17, turnId: "124", taskId: "91" }
```

```json
{
  "conversationId": 17,
  "turnId": "124",
  "excludedTurns": 0,
  "event": {
    "kind": "observation",
    "taskId": "91",
    "record": {"operationId": "session_op_8", "tool": "files.read", "status": "succeeded", "error": null},
    "observation": {"tool": "files.read", "status": "success", "result": {"content": "Synthetic note", "file_bytes": 14, "truncated": false}}
  }
}
```

```json
{
  "id": 17,
  "title": "Find the ledger files",
  "systemPrompt": "",
  "createdAt": 100,
  "updatedAt": 250,
  "turns": [{
    "id": "124", "kind": "agent", "taskId": "91", "taskTimeoutMs": 300000,
    "nativeResultsAvailable": false,
    "user": {"id": 124, "position": 0, "role": "user", "state": "complete", "content": "Read the selected file.", "createdAt": 100, "stats": null},
    "assistants": [{
      "message": {"id": 125, "position": 1, "role": "assistant", "state": "complete", "content": "The file says that rainwater is collected.", "createdAt": 100, "stats": null},
      "modelName": "local-model.gguf"
    }]
  }]
}
```

## Permission and privacy copy

Continue using the existing native picker grant APIs. A previously selected
directory can be reused in a new turn while its grant is current. Each
operation passes the current policy and actual target checks. A historical
message or result never grants access. Exact-file and directory authority stay
separate. The new `scope_kind_mismatch` error means the model used arguments
incompatible with the selected resource. Suggest reading the existing selected
file directly or selecting a folder to search it. Keep other denials,
`approval_declined`, and `approval_expired` distinct.

Disclose that SQLite stores prompts and assistant answers locally in ordinary,
unencrypted storage. An answer may contain text or facts from a selected file.
Raw native observations are session-only. Backend history includes at most
eight result-bearing turns across the session; older items and restart can
clear the data. Show persisted answers and native tool results as different
kinds of information. Avoid suggesting a result was replayed after restart.

Deleting a conversation removes its message text, linked tasks, events,
execution metadata, and session cache. Detached metadata-only permission
audit entries remain for sequence integrity. They have no conversation/task
association, scope or operation reference, private path, answer, or prompt.
Existing unlinked Agent history remains accessible through its own journal.
