// Frontend mirror of the Agent IPC contract in docs/task-01-a-backend-handoff.md.
// The Rust host is authoritative; these types only describe what it sends.

export type IpcError = { code: string; message: string };
export type TaskId = string;

export type TaskState =
  | "created" | "planning" | "validating" | "awaiting_authorization"
  | "authorized" | "executing" | "observing" | "completed" | "denied"
  | "cancelled" | "failed" | "timed_out" | "interrupted";

export type ToolId =
  | "math.calculate" | "device.info" | "files.search" | "files.read"
  | "apps.list" | "apps.launch";

export type Execution = { operationId: string; tool: ToolId; status: string; error: string | null };

export type ToolObservation =
  | { tool: ToolId; status: "success"; result: unknown }
  | { tool: ToolId; status: "failure"; error: IpcError };

export type NativeResult = { record: Execution; observation: ToolObservation };

export type BudgetUsage = {
  toolCalls: number;
  modelCalls: number;
  retries: number;
  generatedTokens: number;
};

export type ApprovalTarget = { displayName: string; desktopFile: string; executable: string };

export type AgentTaskSummary = {
  id: TaskId;
  state: TaskState;
  createdAt: number;
  updatedAt: number;
  finalized: boolean;
  usage: unknown;
  error: string | null;
  errorMessage: string | null;
  conversationId: number | null;
  turnId: string | null;
  excludedTurns: number | null;
};

export type AgentTaskDetail = {
  task: AgentTaskSummary;
  events: { sequence: number; createdAt: number; event: Record<string, unknown> }[];
  executions: (Execution & { createdAt: number; updatedAt: number })[];
  audit: { namespace: string; sequence: number; createdAt: number; record: Record<string, unknown> }[];
  nativeResults?: NativeResult[];
};

export type ApprovalEvent = {
  kind: "approval_required"; taskId: TaskId; operationId: string; tool: ToolId;
  expiresInMs: number; target: ApprovalTarget | null;
};

export type FinishedEvent = {
  kind: "finished"; taskId: TaskId; state: TaskState; usage: BudgetUsage; answer: string | null;
  error: IpcError | null; executions: Execution[]; nativeResults?: NativeResult[];
  journalIncomplete: boolean;
};

export type AgentEvent =
  | { kind: "started"; taskId: TaskId; taskTimeoutMs: number }
  | { kind: "state_changed"; taskId: TaskId; state: TaskState }
  | { kind: "tool_proposed"; taskId: TaskId; tool: ToolId }
  | { kind: "proposal_rejected"; taskId: TaskId; code: string }
  | { kind: "dispatch_started"; taskId: TaskId; operationId: string; tool: ToolId }
  | ApprovalEvent
  | { kind: "execution"; taskId: TaskId; record: Execution }
  | ({ kind: "observation"; taskId: TaskId } & NativeResult)
  | FinishedEvent
  | { kind: "failed"; taskId: TaskId; error: IpcError };

export type AgentScopeDisplay = {
  /** Dialog-session revocation reference. Keep it out of visible copy and prompts. */
  id: string;
  displayName: string;
  kind: "file" | "directory";
  access: "read_only";
  state: "active" | "expired" | "revoked";
  remainingMs: number;
};
