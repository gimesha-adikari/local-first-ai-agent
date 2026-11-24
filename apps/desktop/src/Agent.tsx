import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { Dialog } from "./Dialogs";
import { errorMessage } from "./inference";
import MarkdownMessage from "./MarkdownMessage";

type Failure = { code: string; message: string };
type Target = { displayName: string; desktopFile: string; executable: string };
type Execution = { operationId: string; tool: string; status: string; error: string | null };
type AgentEvent =
  | { kind: "started"; taskId: string }
  | { kind: "state_changed"; taskId: string; state: string }
  | { kind: "tool_proposed"; taskId: string; tool: string }
  | { kind: "proposal_rejected"; taskId: string; code: string }
  | { kind: "dispatch_started"; taskId: string; operationId: string; tool: string }
  | { kind: "approval_required"; taskId: string; operationId: string; tool: string; expiresInMs: number; target: Target | null }
  | { kind: "execution"; taskId: string; record: Execution }
  | { kind: "observation"; taskId: string; record: Execution; observation: unknown }
  | { kind: "finished"; taskId: string; state: string; usage: unknown; answer: string | null; error: Failure | null; executions: Execution[]; journalIncomplete: boolean }
  | { kind: "failed"; taskId: string; error: Failure };
type Task = { id: string; state: string; createdAt: number; updatedAt: number; finalized: boolean; usage: unknown; error: string | null };
type TaskDetail = {
  task: Task;
  events: { sequence: number; createdAt: number; event: Record<string, unknown> }[];
  executions: (Execution & { createdAt: number; updatedAt: number })[];
  audit: { namespace: string; sequence: number; createdAt: number; record: Record<string, unknown> }[];
};
type Scope = { id: string; kind: "file" | "directory"; state: "active" | "expired" | "revoked"; remainingMs: number };
type Timeline = { key: number; at: number; event: AgentEvent };

const names: Record<string, string> = {
  "math.calculate": "Calculator", "device.info": "Device information",
  "files.search": "Search selected files", "files.read": "Read selected file",
  "apps.list": "Installed applications", "apps.launch": "Launch application",
};
const toolName = (value: string) => names[value] ?? value;
const label = (value: string) => value.replaceAll("_", " ").replace(/\b\w/g, (c) => c.toUpperCase());
const time = (value: number) => new Date(value).toLocaleString();
const remaining = (ms: number) => ms < 60_000
  ? String(Math.max(1, Math.ceil(ms / 1000))) + " sec"
  : String(Math.ceil(ms / 60_000)) + " min";
function json(value: unknown): string {
  try { return JSON.stringify(value, null, 2) ?? String(value); } catch { return String(value); }
}
function errorText(error: Failure | null | undefined): string {
  return error?.message ?? (error?.code ? label(error.code) : "The agent task failed.");
}
function eventTitle(event: AgentEvent): string {
  switch (event.kind) {
    case "started": return "Task started";
    case "state_changed": return "State · " + label(event.state);
    case "tool_proposed": return "Proposed · " + toolName(event.tool);
    case "proposal_rejected": return "Proposal rejected · " + label(event.code);
    case "dispatch_started": return "Dispatching · " + toolName(event.tool);
    case "approval_required": return "Approval requested · " + toolName(event.tool);
    case "execution": return toolName(event.record.tool) + " · " + label(event.record.status);
    case "observation": return "Result received · " + toolName(event.record.tool);
    case "finished": return "Task " + label(event.state);
    case "failed": return "Task failed";
  }
}
function savedEventTitle(event: Record<string, unknown>): string {
  const kind = String(event.kind ?? "event");
  if (kind === "transition" && event.transition && typeof event.transition === "object") {
    const item = event.transition as Record<string, unknown>;
    return label(String(item.from ?? "state")) + " → " + label(String(item.to ?? "state"));
  }
  if (kind === "dispatch_intent") return "Dispatch · " + toolName(String(event.tool ?? "tool"));
  if (kind === "execution" && event.record && typeof event.record === "object") {
    const item = event.record as Record<string, unknown>;
    return toolName(String(item.tool ?? "tool")) + " · " + label(String(item.status ?? "result"));
  }
  if (kind === "recovered_interrupted") return "Recovered after interrupted session";
  return label(kind);
}
function executionText(item: Execution): string {
  return toolName(item.tool) + " · " + label(item.status) + (item.error ? " · " + label(item.error) : "");
}

export default function AgentPanel({
  hidden, modelReady, supportsAgent, modelName, blocked, onOpenModel, onRunningChange,
}: {
  hidden: boolean; modelReady: boolean; supportsAgent: boolean; modelName: string | null;
  blocked: boolean; onOpenModel: () => void; onRunningChange: (running: boolean) => void;
}) {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [scopes, setScopes] = useState<Scope[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<TaskDetail | null>(null);
  const [events, setEvents] = useState<Timeline[]>([]);
  const [prompt, setPrompt] = useState("");
  const [running, setRunning] = useState(false);
  const [starting, setStarting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [scopeAction, setScopeAction] = useState<string | null>(null);
  const [approval, setApproval] = useState<Extract<AgentEvent, { kind: "approval_required" }> | null>(null);
  const [approvalBusy, setApprovalBusy] = useState(false);
  const [eventReady, setEventReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [scopeError, setScopeError] = useState<string | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);
  const activeId = useRef<string | null>(null);
  const selectedIdRef = useRef<string | null>(null);
  const startingRef = useRef(false);
  const queued = useRef<AgentEvent[]>([]);
  const listenerAvailable = useRef(false);
  const listenerReady = useRef<Promise<UnlistenFn>>(Promise.resolve(() => {}));
  const receiveRef = useRef<(event: AgentEvent) => void>(() => {});
  const eventKey = useRef(0);
  selectedIdRef.current = selectedId;

  function setTaskRunning(value: boolean) {
    setRunning(value);
    onRunningChange(value);
  }
  async function refreshTasks() {
    setHistoryBusy(true);
    try { setTasks(await invoke<Task[]>("list_agent_tasks", { before: null, limit: 30 })); }
    catch (failure) { setError(errorMessage(failure)); }
    finally { setHistoryBusy(false); }
  }
  async function refreshScopes() {
    try {
      setScopes(await invoke<Scope[]>("list_agent_scopes"));
      setScopeError(null);
    } catch (failure) { setScopeError(errorMessage(failure)); }
  }
  async function loadDetail(id: string) {
    try {
      const result = await invoke<TaskDetail>("get_agent_task", { taskId: id });
      if (selectedIdRef.current === id) setDetail(result);
    } catch (failure) { setError(errorMessage(failure)); }
  }
  function receive(event: AgentEvent) {
    setEvents((current) => [...current.slice(-79), { key: ++eventKey.current, at: Date.now(), event }]);
    if (event.kind === "approval_required") setApproval(event);
    if (event.kind === "finished" || event.kind === "failed") {
      setApproval(null);
      setCancelling(false);
      setTaskRunning(false);
      if (event.kind === "failed") setError(errorText(event.error));
      void refreshTasks();
      void loadDetail(event.taskId);
    }
  }
  receiveRef.current = (event) => {
    if (startingRef.current && activeId.current === null) { queued.current.push(event); return; }
    if (event.taskId === activeId.current) receive(event);
  };

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    listenerReady.current = listen<AgentEvent>("agent-event", ({ payload }) => receiveRef.current(payload))
      .then((stop) => {
        listenerAvailable.current = true;
        setEventReady(true);
        unlisten = stop;
        if (disposed) stop();
        return stop;
      })
      .catch((failure: unknown) => {
        listenerAvailable.current = false;
        setEventReady(false);
        setError(errorMessage(failure));
        return () => {};
      });
    void refreshTasks();
    void refreshScopes();
    return () => { disposed = true; unlisten?.(); };
  }, []);

  async function selectTask(id: string) {
    selectedIdRef.current = id;
    setSelectedId(id);
    setDetail(null);
    setError(null);
    await loadDetail(id);
  }
  async function startTask(event: FormEvent) {
    event.preventDefault();
    const text = prompt.trim();
    if (!text || starting || running || blocked || !modelReady || !supportsAgent || !eventReady) return;
    const previousSelection = selectedIdRef.current;
    setError(null);
    setStarting(true);
    onRunningChange(true);
    startingRef.current = true;
    queued.current = [];
    activeId.current = null;
    setEvents([]);
    setDetail(null);
    setApproval(null);
    try {
      await listenerReady.current;
      if (!listenerAvailable.current) throw new Error("Agent event delivery is unavailable. Reopen the Agent workspace and try again.");
      const id = await invoke<string>("start_agent_task", { prompt: text });
      activeId.current = id;
      selectedIdRef.current = id;
      setSelectedId(id);
      setPrompt("");
      setTaskRunning(true);
      void refreshTasks();
      const pending = queued.current.splice(0);
      for (const item of pending) if (item.taskId === id) receive(item);
    } catch (failure) {
      setError(errorMessage(failure));
      activeId.current = null;
      queued.current = [];
      onRunningChange(false);
      if (previousSelection) void loadDetail(previousSelection);
      else {
        selectedIdRef.current = null;
        setSelectedId(null);
      }
    } finally {
      startingRef.current = false;
      setStarting(false);
    }
  }
  async function cancelTask() {
    const id = activeId.current;
    if (!id || !running || cancelling) return;
    setCancelling(true);
    setApproval(null);
    try {
      const cancelled = await invoke<boolean>("cancel_agent_task", { taskId: id });
      if (!cancelled) { setCancelling(false); setTaskRunning(false); await refreshTasks(); }
    } catch (failure) { setError(errorMessage(failure)); setCancelling(false); }
  }
  async function answerApproval(approve: boolean) {
    if (!approval || approvalBusy) return;
    const request = approval;
    setApprovalBusy(true);
    setError(null);
    try {
      await invoke("respond_agent_approval", {
        taskId: request.taskId, operationId: request.operationId, approve,
      });
      setApproval(null);
    } catch (failure) { setError(errorMessage(failure)); }
    finally { setApprovalBusy(false); }
  }
  async function grantScope(kind: "file" | "directory") {
    if (scopeAction || running || blocked) return;
    setScopeAction(kind);
    setScopeError(null);
    try {
      await invoke<Scope | null>(kind === "file" ? "choose_agent_file" : "choose_agent_directory");
      await refreshScopes();
    } catch (failure) { setScopeError(errorMessage(failure)); }
    finally { setScopeAction(null); }
  }
  async function revokeScope(scope: Scope) {
    if (scope.state !== "active" || scopeAction) return;
    setScopeAction(scope.id);
    setScopeError(null);
    try {
      await invoke<boolean>("revoke_agent_scope", { scopeId: scope.id });
      await refreshScopes();
    } catch (failure) { setScopeError(errorMessage(failure)); }
    finally { setScopeAction(null); }
  }

  const liveSelected = selectedId !== null && selectedId === activeId.current;
  const finishedEntry = liveSelected ? [...events].reverse().find((item) => item.event.kind === "finished") : undefined;
  const finished = finishedEntry?.event.kind === "finished" ? finishedEntry.event : null;
  const failedEntry = liveSelected ? [...events].reverse().find((item) => item.event.kind === "failed") : undefined;
  const failed = failedEntry?.event.kind === "failed" ? failedEntry.event : null;
  const task = detail?.task.id === selectedId ? detail.task : tasks.find((item) => item.id === selectedId) ?? null;
  const canStart = !!prompt.trim() && eventReady && modelReady && supportsAgent && !running && !starting && !blocked;
  const status = running ? "Running task " + (activeId.current ? "#" + activeId.current : "")
    : !eventReady ? "Connecting to agent events…"
      : !modelReady ? "Load a local model to use Agent mode"
        : !supportsAgent ? "This model does not support the agent decision protocol"
          : blocked ? "Finish the current chat operation before starting an agent task"
            : "Ready · local model only";

  return (
    <>
      <main className={"agent-panel" + (hidden ? " view-hidden" : "")} aria-label="Agent workspace">
        <header className="agent-header">
          <div><h2>Agent</h2><p className="hint">A local assistant that can use approved tools on this device.</p></div>
          <div className="agent-header-actions">
            <span className="local-indicator"><span aria-hidden="true" />Local only</span>
            <button type="button" disabled={running || starting || blocked} onClick={onOpenModel}>Model controls</button>
          </div>
        </header>
        <div className="agent-statusline" role="status">
          <span>{status}</span>
          {modelName && <span>Model · {modelName}</span>}
          {running && <button type="button" className="danger" disabled={cancelling} onClick={() => void cancelTask()}>
            {cancelling ? "Cancelling…" : "Cancel task"}
          </button>}
        </div>

        <div className="agent-layout">
          <section className="agent-main">
            <div className="agent-thread">
              {!selectedId ? <div className="empty-state">
                <h3>What should the agent do?</h3>
                <p>Describe a task. The agent uses registered local tools and asks before launching an application.</p>
              </div> : <>
                <div className="agent-task-heading">
                  <div><h3>Task #{selectedId}</h3>
                    <span className="hint">{task ? label(task.state) + " · " + time(task.updatedAt) : "Loading task details…"}</span>
                  </div>
                  {task && <span className={"task-state task-state-" + task.state}>{label(task.state)}</span>}
                </div>

                {liveSelected && events.length > 0 ? <ol className="agent-timeline" aria-label="Live task timeline" aria-live="polite">
                  {events.map(({ key, at, event: item }) => <li key={key} className={"timeline-item timeline-" + item.kind}>
                    <span className="timeline-marker" aria-hidden="true" />
                    <div className="timeline-content">
                      <div className="timeline-heading"><strong>{eventTitle(item)}</strong><time>{new Date(at).toLocaleTimeString()}</time></div>
                      {item.kind === "execution" && <p className="hint">{executionText(item.record)}</p>}
                      {item.kind === "observation" && <details className="agent-observation">
                        <summary>View tool result (transient)</summary><pre>{json(item.observation)}</pre>
                      </details>}
                      {item.kind === "proposal_rejected" && <p className="hint">The proposal did not pass deterministic validation.</p>}
                      {item.kind === "failed" && <p className="error">{errorText(item.error)}</p>}
                    </div>
                  </li>)}
                </ol> : detail?.task.id === selectedId ? <div className="agent-history-detail">
                  <h4>Task events</h4>
                  {detail.events.length ? <ol className="agent-timeline">
                    {detail.events.map((item) => <li className="timeline-item" key={item.sequence}>
                      <span className="timeline-marker" aria-hidden="true" />
                      <div className="timeline-content"><div className="timeline-heading">
                        <strong>{savedEventTitle(item.event)}</strong><time>{time(item.createdAt)}</time>
                      </div></div>
                    </li>)}
                  </ol> : <p className="hint">No persisted events are available.</p>}
                  <h4>Execution history</h4>
                  {detail.executions.length ? <ul className="execution-list">
                    {detail.executions.map((item) => <li key={item.operationId}>
                      <strong>{executionText(item)}</strong><span className="hint">{time(item.updatedAt)}</span>
                    </li>)}
                  </ul> : <p className="hint">No tools were dispatched.</p>}
                  {detail.audit.length > 0 && <details className="audit-details">
                    <summary>Permission audit · {detail.audit.length} records</summary>
                    <ul>{detail.audit.map((item) => <li key={item.namespace + "-" + item.sequence}>
                      <strong>{label(String(item.record.action ?? "permission event"))}</strong>
                      <span className="hint">{String([item.record.tool, item.record.reason, item.record.scopeId].filter(Boolean).join(" · "))} · {time(item.createdAt)}</span>
                    </li>)}</ul>
                  </details>}
                </div> : <p className="hint">Loading saved task history…</p>}

                {finished && <section className="agent-result" aria-label="Task result">
                  <h4>Result · {label(finished.state)}</h4>
                  {finished.answer ? <MarkdownMessage text={finished.answer} /> : <p className="hint">The task ended without a final answer.</p>}
                  {finished.error && <p className="error">{errorText(finished.error)}</p>}
                  {finished.journalIncomplete && <p className="error">The task journal is incomplete. Do not treat this result as durable execution evidence.</p>}
                  <details><summary>Budget usage</summary><pre>{json(finished.usage)}</pre></details>
                </section>}
                {failed && <p className="error" role="alert">{errorText(failed.error)}</p>}
                {task?.error && !finished && <p className="error">{task.error}</p>}
                {task?.usage != null && !finished && <details className="agent-result">
                  <summary>Recorded budget usage</summary><pre>{json(task.usage)}</pre>
                </details>}
              </>}
            </div>

            <form className="agent-composer" onSubmit={(event) => void startTask(event)}>
              <label className="sr-only" htmlFor="agent-prompt">Task description</label>
              <textarea id="agent-prompt" rows={3} maxLength={8192} value={prompt}
                disabled={running || starting || blocked || !eventReady}
                placeholder="Describe a task for your local agent…"
                onChange={(event) => setPrompt(event.currentTarget.value)} />
              <div className="agent-composer-actions">
                <p className="hint">Only registered local tools are available. Application launches always require your approval.</p>
                <button className="primary" type="submit" disabled={!canStart}>
                  {starting ? "Starting…" : running ? "Agent running…" : "Start task"}
                </button>
              </div>
            </form>
            {error && <p className="error agent-error" role="alert">{error}</p>}
          </section>

          <aside className="agent-sidebar" aria-label="Agent history and permissions">
            <section className="agent-side-section">
              <div className="agent-section-heading"><h3>Task history</h3>
                <button type="button" className="quiet small" disabled={historyBusy} onClick={() => void refreshTasks()}>Refresh</button>
              </div>
              {tasks.length ? <nav className="agent-task-list" aria-label="Previous agent tasks">
                {tasks.map((item) => <button type="button" key={item.id} className="agent-task-link"
                  aria-current={selectedId === item.id ? "page" : undefined} onClick={() => void selectTask(item.id)}>
                  <span><strong>Task #{item.id}</strong><small>{label(item.state)}</small></span>
                  <time>{new Date(item.updatedAt).toLocaleDateString()}</time>
                </button>)}
              </nav> : <p className="hint">{historyBusy ? "Loading task history…" : "No agent tasks yet."}</p>}
            </section>

            <section className="agent-side-section">
              <div className="agent-section-heading"><h3>File permissions</h3>
                <button type="button" className="quiet small" onClick={() => void refreshScopes()}>Refresh</button>
              </div>
              <p className="hint">Grant read access only to files or folders you select. Grants expire after one hour and are not restored after restart.</p>
              <div className="scope-grant-actions">
                <button type="button" disabled={running || starting || blocked || scopeAction !== null} onClick={() => void grantScope("file")}>
                  {scopeAction === "file" ? "Choosing…" : "Select file"}
                </button>
                <button type="button" disabled={running || starting || blocked || scopeAction !== null} onClick={() => void grantScope("directory")}>
                  {scopeAction === "directory" ? "Choosing…" : "Select folder"}
                </button>
              </div>
              {scopeError && <p className="error" role="alert">{scopeError}</p>}
              {scopes.length ? <ul className="scope-list">
                {scopes.map((scope) => <li key={scope.id}>
                  <div><strong>{scope.kind === "file" ? "Selected file" : "Selected folder"}</strong>
                    <span className="hint">{label(scope.state)} · {scope.state === "active" ? "expires in " + remaining(scope.remainingMs) : "not available"}</span>
                    <code title={scope.id}>{scope.id}</code>
                  </div>
                  <button type="button" className="quiet danger small" disabled={scope.state !== "active" || scopeAction !== null}
                    onClick={() => void revokeScope(scope)}>{scopeAction === scope.id ? "Revoking…" : "Revoke"}</button>
                </li>)}
              </ul> : <p className="hint">No selected files or folders are currently authorized.</p>}
            </section>
          </aside>
        </div>
      </main>

      {approval && <Dialog title="Agent approval required" onClose={() => void answerApproval(false)} busy={approvalBusy}>
        <div className="dialog-form">
          <p>The agent wants to perform an action that requires your explicit approval.</p>
          <div className="approval-target">
            <strong>{toolName(approval.tool)}</strong>
            {approval.target ? <>
              <span>{approval.target.displayName}</span>
              <code>Desktop entry: {approval.target.desktopFile}</code>
              <code>Executable: {approval.target.executable}</code>
            </> : <span>Trusted target details are unavailable. Approval is disabled unless a target can be verified.</span>}
          </div>
          <p className="hint">This approval applies to this operation only and expires in about {remaining(approval.expiresInMs)}.</p>
          {error && <p className="error" role="alert">{error}</p>}
          <div className="dialog-actions">
            <button type="button" className="danger" disabled={approvalBusy} onClick={() => void answerApproval(false)}>Deny</button>
            <button type="button" className="primary" disabled={approvalBusy || !approval.target}
              onClick={() => void answerApproval(true)}>{approvalBusy ? "Responding…" : "Approve this launch once"}</button>
          </div>
        </div>
      </Dialog>}
    </>
  );
}
