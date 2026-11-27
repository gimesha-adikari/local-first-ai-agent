// Pure logic for unified Chat + Agent conversations (Task 02-A handoff).
// No React or Tauri imports, so the Node test runner can exercise it directly.
// Turn and task identifiers are decimal strings and must never become numbers.
import {
  applyAgentEvent, createLiveTask, isTerminal, mergeNativeResults, type LiveTask,
} from "./agent.ts";
import type {
  AgentEvent, AgentScopeDisplay, AgentTaskDetail, ApprovalTarget, IpcError, NativeResult, TaskState, ToolId,
} from "./agentTypes";
import type { ChatEvent, Message } from "./chat";

export type ConversationId = number;
export type TurnId = string;

export type AgentTurnIdentity = { conversationId: ConversationId; turnId: TurnId; taskId: string };

export type ActiveAgentTurn = AgentTurnIdentity & {
  approval: null | { operationId: string; expiresInMs: number; target: ApprovalTarget | null; tool: ToolId };
  tool: ToolId | null;
  startedAt: number | null;
  taskTimeoutMs: number;
};

export type AssistantAttempt = { message: Message; modelName: string | null };

export type UnifiedTurn = {
  id: TurnId;
  kind: "chat" | "agent";
  user: Message;
  assistants: AssistantAttempt[];
  taskId: string | null;
  taskTimeoutMs: number | null;
  taskState: TaskState | null;
  taskError: string | null;
  taskErrorMessage: string | null;
  excludedTurns: number | null;
  nativeResultsAvailable: boolean;
};

export type UnifiedConversation = {
  id: ConversationId;
  title: string;
  systemPrompt: string;
  createdAt: number;
  updatedAt: number;
  turns: UnifiedTurn[];
};

export type AgentTurnEvent = {
  conversationId: ConversationId;
  turnId: TurnId;
  excludedTurns: number;
  event: AgentEvent;
};

/** Live, session-only state for one linked Agent turn. */
export type LiveTurn = LiveTask & {
  conversationId: ConversationId;
  turnId: TurnId;
  excludedTurns: number;
};

export function sameIdentity(a: AgentTurnIdentity | null | undefined, b: AgentTurnIdentity | null | undefined): boolean {
  return !!a && !!b && a.conversationId === b.conversationId && a.turnId === b.turnId && a.taskId === b.taskId;
}

/** An event belongs to an owner only when conversation, turn and task all match. */
export function ownsEvent(owner: AgentTurnIdentity | null | undefined, payload: AgentTurnEvent): boolean {
  return !!owner && payload.conversationId === owner.conversationId && payload.turnId === owner.turnId
    && payload.event?.taskId === owner.taskId;
}

export const isTerminalEvent = (event: AgentEvent) => event.kind === "finished" || event.kind === "failed";

export function createLiveTurn(identity: AgentTurnIdentity, prompt: string, timeoutMs: number | null, now: number, perf: number): LiveTurn {
  return {
    ...createLiveTask(identity.taskId, prompt, timeoutMs ?? 0, now, perf),
    timeoutMs,
    conversationId: identity.conversationId,
    turnId: identity.turnId,
    excludedTurns: 0,
  };
}

/** Apply a scoped event to its own turn. Events for any other identity are ignored. */
export function applyTurnEvent(turn: LiveTurn, payload: AgentTurnEvent, entry: { key: number; at: number; perf: number }): LiveTurn {
  if (!ownsEvent({ conversationId: turn.conversationId, turnId: turn.turnId, taskId: turn.id }, payload)) return turn;
  const next = applyAgentEvent(turn, payload.event, entry) as LiveTurn;
  const excluded = Number.isFinite(payload.excludedTurns) ? Math.max(0, payload.excludedTurns) : 0;
  return { ...next, conversationId: turn.conversationId, turnId: turn.turnId, excludedTurns: Math.max(turn.excludedTurns, excluded) };
}

function upsertAttempt(attempts: AssistantAttempt[], message: Message, modelName: string | null): AssistantAttempt[] {
  const index = attempts.findIndex((attempt) => attempt.message.id === message.id);
  if (index === -1) return [...attempts, { message, modelName }];
  const next = [...attempts];
  next[index] = { ...next[index], message };
  return next;
}

/**
 * Merge a streamed Chat event into unified turns. A send creates or updates its
 * own Chat turn; a regeneration appends an attempt to the latest Chat turn.
 */
export function applyChatEventToTurns(turns: UnifiedTurn[], event: ChatEvent, modelName: string | null): UnifiedTurn[] {
  // Coverage arrives before `started` and is tracked by the caller, not in turns.
  if (event.kind === "knowledge") return turns;
  if (event.kind === "started") {
    if (event.user) {
      const id = String(event.user.id);
      const index = turns.findIndex((turn) => turn.id === id);
      const base: UnifiedTurn = index === -1
        ? { id, kind: "chat", user: event.user, assistants: [], taskId: null, taskTimeoutMs: null,
          taskState: null, taskError: null, taskErrorMessage: null, excludedTurns: null, nativeResultsAvailable: false }
        : { ...turns[index], user: event.user };
      const turn = { ...base, assistants: upsertAttempt(base.assistants, event.assistant, modelName) };
      return index === -1 ? [...turns, turn] : turns.map((item, position) => (position === index ? turn : item));
    }
    // A continuation restarts an existing attempt: update it in place by message ID.
    const owner = turns.findIndex((turn) => turn.assistants.some((attempt) => attempt.message.id === event.assistant.id));
    if (owner !== -1) {
      const turn = { ...turns[owner], assistants: upsertAttempt(turns[owner].assistants, event.assistant, modelName) };
      return turns.map((item, position) => (position === owner ? turn : item));
    }
    for (let index = turns.length - 1; index >= 0; index--) {
      if (turns[index].kind !== "chat") continue;
      const turn = { ...turns[index], assistants: upsertAttempt(turns[index].assistants, event.assistant, modelName) };
      return turns.map((item, position) => (position === index ? turn : item));
    }
    return turns;
  }
  if (event.kind === "research") return turns;
  if (typeof event.text !== "string" || !event.text) return turns;
  const text = event.text;
  let changed = false;
  const next = turns.map((turn) => {
    const index = turn.assistants.findIndex((attempt) =>
      attempt.message.id === event.id && attempt.message.role === "assistant" && attempt.message.state === "streaming");
    if (index === -1) return turn;
    changed = true;
    const assistants = [...turn.assistants];
    const current = assistants[index];
    assistants[index] = { ...current, message: { ...current.message, content: current.message.content + text } };
    return { ...turn, assistants };
  });
  return changed ? next : turns;
}

/** Regeneration is offered only when the newest turn is a Chat turn. */
export function canRegenerateLatest(conversation: Pick<UnifiedConversation, "turns"> | null): boolean {
  const last = conversation?.turns.at(-1);
  return !!last && last.kind === "chat";
}

export function agentTurnTaskIds(conversation: UnifiedConversation | null, options: { withNativeResults?: boolean } = {}): string[] {
  return (conversation?.turns ?? []).flatMap((turn) => (
    turn.kind === "agent" && turn.taskId && (!options.withNativeResults || turn.nativeResultsAvailable) ? [turn.taskId] : []));
}

export type AgentTurnDisplay = {
  running: boolean;
  state: TaskState | "running" | null;
  answer: string | null;
  answerSource: "saved" | "live" | null;
  assistantState: Message["state"] | null;
  nativeResults: NativeResult[];
  /** Native steps succeeded, but their payloads are no longer held by the session. */
  nativeUnavailable: boolean;
  succeededSteps: number;
  error: IpcError | null;
  excludedTurns: number;
};

const succeededCount = (detail: AgentTaskDetail | undefined, live: LiveTurn | undefined) => {
  const fromDetail = detail?.executions.filter((item) => item.status === "succeeded").length ?? 0;
  const fromLive = live?.finished?.executions.filter((item) => item.status === "succeeded").length
    ?? live?.nativeResults.filter((item) => item.observation.status === "success").length ?? 0;
  return Math.max(fromDetail, fromLive);
};

/**
 * Combine the saved turn, its live session state and its journal detail.
 * `loadedAt` is when the conversation snapshot was read: the backend's
 * `nativeResultsAvailable` flag is authoritative for anything that ended before it.
 */
export function agentTurnDisplay(
  turn: UnifiedTurn,
  live: LiveTurn | undefined,
  detail: AgentTaskDetail | undefined,
  options: { running: boolean; loadedAt: number },
): AgentTurnDisplay {
  const attempt = turn.assistants.at(-1);
  const saved = attempt && attempt.message.state !== "streaming" && attempt.message.content.trim()
    ? attempt.message.content : null;
  const liveAnswer = live?.finished?.answer?.trim() ? live.finished.answer : null;
  const liveFresh = !!live && (options.running || live.endedAt === null || live.endedAt > options.loadedAt);

  let results: NativeResult[] = [];
  if (liveFresh) results = mergeNativeResults(live.nativeResults, undefined);
  else if (turn.nativeResultsAvailable) {
    results = mergeNativeResults(live?.nativeResults ?? [], detail?.nativeResults);
  }
  const succeeded = succeededCount(detail, live);
  const savedState = detail?.task.state ?? turn.taskState;
  const state: AgentTurnDisplay["state"] = options.running ? "running"
    : live?.finished?.state ?? (live?.failure ? "failed" : undefined)
      ?? savedState ?? (attempt?.message.state === "interrupted" ? "interrupted" : null);
  const savedCode = detail?.task.error ?? turn.taskError;
  const savedMessage = detail?.task.errorMessage ?? turn.taskErrorMessage;
  const error = live?.finished?.error ?? live?.failure
    ?? (savedState && isTerminal(savedState) && (savedCode || savedMessage)
      ? { code: savedCode ?? savedState, message: savedMessage ?? "" } : null);

  return {
    running: options.running,
    state,
    answer: saved ?? liveAnswer,
    answerSource: saved ? "saved" : liveAnswer ? "live" : null,
    assistantState: attempt?.message.state ?? null,
    nativeResults: results,
    nativeUnavailable: !options.running && results.length === 0 && succeeded > 0,
    succeededSteps: succeeded,
    error,
    excludedTurns: Math.max(live?.excludedTurns ?? 0, turn.excludedTurns ?? 0),
  };
}

export function contextOmissionNote(excludedTurns: number): string | null {
  if (!Number.isFinite(excludedTurns) || excludedTurns <= 0) return null;
  return `${excludedTurns} earlier ${excludedTurns === 1 ? "turn was" : "turns were"} left out of the model's input to fit its context. `
    + `${excludedTurns === 1 ? "It remains" : "They remain"} saved in this conversation.`;
}

export type ScopeContext = { files: string[]; folders: string[] };

export function scopeContext(scopes: readonly AgentScopeDisplay[]): ScopeContext {
  const active = scopes.filter((scope) => scope.state === "active");
  return {
    files: active.filter((scope) => scope.kind === "file").map((scope) => scope.displayName),
    folders: active.filter((scope) => scope.kind === "directory").map((scope) => scope.displayName),
  };
}

const quoteList = (names: string[]) => names.map((name) => `“${name}”`).join(", ");

/**
 * Grant-aware guidance for permission errors. A kind mismatch means the model
 * used arguments that don't fit the selected resource, so never ask the user
 * to select a resource they already granted.
 */
export function permissionGuidance(code: string, context: ScopeContext): { title: string; detail: string } | null {
  if (code === "scope_kind_mismatch") {
    if (context.files.length) {
      return {
        title: "The model used folder-style arguments for a selected file",
        detail: `${quoteList(context.files)} is still selected. Ask the agent to read the selected file directly; it doesn't need a folder path. `
          + "To search for files instead, select a folder.",
      };
    }
    if (context.folders.length) {
      return {
        title: "The model treated the selected folder like a single file",
        detail: `${quoteList(context.folders)} is still selected. Ask the agent to search that folder, or to read a named file inside it.`,
      };
    }
    return {
      title: "The model used arguments that don't match the selected resource",
      detail: "The selection for this step is no longer active. Select the file or folder again if the agent still needs it.",
    };
  }
  if (code === "permission_denied" && (context.files.length || context.folders.length)) {
    return {
      title: "The step needed access that isn't granted",
      detail: `Current selections: ${quoteList([...context.files, ...context.folders])}. If the agent needs a different file or folder, select it with the + button. `
        + "Earlier messages and results never grant access by themselves.",
    };
  }
  return null;
}
