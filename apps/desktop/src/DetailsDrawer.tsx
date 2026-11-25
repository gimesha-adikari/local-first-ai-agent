import { useEffect, useRef, type ReactNode } from "react";
import { modelLabel } from "./models";
import {
  durationLabel, eventTitle, executionLabel, humanize, remainingLabel, savedEventTitle, stateLabel, toolLabel,
  usageFrom,
} from "./agent";
import type { AgentScopeDisplay, AgentTaskDetail, AgentTaskSummary } from "./agentTypes";
import type { UnifiedConversation } from "./unified";
import { FileIcon, FolderIcon, CloseIcon } from "./icons";
import type { ModelInfo } from "./inference";
import { IconButton } from "./ui";
import type { LiveTask } from "./useAgent";

function Facts({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="facts">
      {rows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
    </dl>
  );
}

function ChatDetails({ conversation, model, onEdit }: {
  conversation: UnifiedConversation | null;
  model: ModelInfo | null;
  onEdit: (kind: "rename" | "instructions" | "delete") => void;
}) {
  const last = conversation ? [...conversation.turns].reverse()
    .filter((turn) => turn.kind === "chat")
    .flatMap((turn) => [...turn.assistants].reverse())
    .find((attempt) => attempt.message.stats) : undefined;
  const stats = last?.message.stats;
  const agentTurns = conversation?.turns.filter((turn) => turn.kind === "agent").length ?? 0;
  return (
    <>
      {conversation && (
        <section className="drawer-section">
          <h3>Conversation</h3>
          <Facts rows={[
            ["Title", conversation.title],
            ["Turns", `${conversation.turns.length} (${agentTurns} Agent)`],
            ["Created", new Date(conversation.createdAt).toLocaleString()],
          ]} />
          <div className="drawer-instructions">
            <span>Instructions</span>
            <p>{conversation.systemPrompt.trim() ? conversation.systemPrompt : "None"}</p>
          </div>
          <div className="drawer-actions">
            <button type="button" className="secondary-button small" onClick={() => onEdit("instructions")}>Edit instructions</button>
            <button type="button" className="secondary-button small" onClick={() => onEdit("rename")}>Rename</button>
            <button type="button" className="ghost-button small danger-text" onClick={() => onEdit("delete")}>Delete</button>
          </div>
        </section>
      )}
      {stats && (
        <section className="drawer-section">
          <h3>Last chat response</h3>
          <Facts rows={[
            ["Finish", humanize(stats.reason)],
            ["Generated", `${stats.generatedTokens} tokens`],
            ["Speed", `${stats.tokensPerSecond.toFixed(1)} tokens/s`],
            ["Prompt", `${stats.promptTokens} tokens${stats.promptTokensPerSecond !== null ? ` · ${stats.promptTokensPerSecond.toFixed(0)}/s` : ""}`],
            ["First token", stats.timeToFirstTokenMs !== null ? `${stats.timeToFirstTokenMs} ms` : "—"],
            ["Total time", durationLabel(stats.elapsedMs)],
            ["Omitted turns", String(stats.excludedTurns)],
          ]} />
        </section>
      )}
      <section className="drawer-section">
        <h3>Model</h3>
        {model ? (
          <Facts rows={[
            ["Name", modelLabel(model.fileName)],
            ["Architecture", model.architecture],
            ["Context", `${model.config.contextLength.toLocaleString()} tokens`],
            ["CPU threads", String(model.config.threads)],
            ["Thinking", model.supportsThinking ? "Supported" : "Not supported"],
            ["Agent", model.supportsAgent ? "Supported" : "Not available"],
          ]} />
        ) : <p className="hint">No model loaded.</p>}
      </section>
    </>
  );
}

function AgentDetails({ live, summary, detail }: {
  live: LiveTask | null;
  summary: AgentTaskSummary | null;
  detail: AgentTaskDetail | null;
}) {
  const task = detail?.task ?? summary;
  const usage = live?.finished?.usage ?? usageFrom(task?.usage);
  return (
    <>
      <section className="drawer-section">
        <h3>Task</h3>
        <Facts rows={[
          ["Task", task ? `#${task.id}` : live ? `#${live.id}` : "—"],
          ["State", stateLabel(live?.finished?.state ?? task?.state ?? "created")],
          ["Started", task ? new Date(task.createdAt).toLocaleString() : live ? new Date(live.submittedAt).toLocaleString() : "—"],
          ["Updated", task ? new Date(task.updatedAt).toLocaleString() : "—"],
          ...(live?.timeoutMs ? [["Time limit", durationLabel(live.timeoutMs)] as [string, string]] : []),
        ]} />
      </section>
      {usage && (
        <section className="drawer-section">
          <h3>Usage</h3>
          <Facts rows={[
            ["Model calls", String(usage.modelCalls)],
            ["Tool calls", String(usage.toolCalls)],
            ["Retries", String(usage.retries)],
            ["Generated", `${usage.generatedTokens} tokens`],
          ]} />
        </section>
      )}
      <section className="drawer-section">
        <h3>Executions</h3>
        {detail?.executions.length ? (
          <ul className="drawer-list">
            {detail.executions.map((item) => (
              <li key={item.operationId}>
                <span>{executionLabel(item)}</span>
                <time>{new Date(item.updatedAt).toLocaleTimeString()}</time>
              </li>
            ))}
          </ul>
        ) : <p className="hint">{detail ? "No tools were run." : "Loading…"}</p>}
      </section>
      {detail && detail.audit.length > 0 && (
        <section className="drawer-section">
          <h3>Permission record</h3>
          <ul className="drawer-list">
            {detail.audit.map((item) => (
              <li key={`${item.namespace}-${item.sequence}`}>
                <span>
                  {humanize(String(item.record.action ?? "permission event"))}
                  {typeof item.record.tool === "string" && ` · ${toolLabel(item.record.tool)}`}
                  {typeof item.record.reason === "string" && ` · ${humanize(item.record.reason)}`}
                </span>
                <time>{new Date(item.createdAt).toLocaleTimeString()}</time>
              </li>
            ))}
          </ul>
        </section>
      )}
      <section className="drawer-section">
        <h3>Event log</h3>
        {live && live.events.length ? (
          <ol className="drawer-list">
            {live.events.map(({ key, at, event }) => (
              <li key={key}><span>{eventTitle(event)}</span><time>{new Date(at).toLocaleTimeString()}</time></li>
            ))}
          </ol>
        ) : detail?.events.length ? (
          <ol className="drawer-list">
            {detail.events.map((item) => (
              <li key={item.sequence}><span>{savedEventTitle(item.event)}</span><time>{new Date(item.createdAt).toLocaleTimeString()}</time></li>
            ))}
          </ol>
        ) : <p className="hint">No events recorded.</p>}
      </section>
    </>
  );
}

function AccessSummary({ scopes, remaining, onManage }: {
  scopes: AgentScopeDisplay[];
  remaining: (scope: AgentScopeDisplay) => number;
  onManage: () => void;
}) {
  const active = scopes.filter((scope) => scope.state === "active");
  return (
    <section className="drawer-section">
      <h3>File access</h3>
      {active.length ? (
        <ul className="drawer-list">
          {active.map((scope) => (
            <li key={scope.id}>
              <span className="drawer-grant">
                {scope.kind === "directory" ? <FolderIcon size={13} /> : <FileIcon size={13} />}{scope.displayName}
              </span>
              <time>{remainingLabel(remaining(scope))}</time>
            </li>
          ))}
        </ul>
      ) : <p className="hint">No files or folders selected.</p>}
      <button type="button" className="link-button" onClick={onManage}>Manage access…</button>
    </section>
  );
}

export default function DetailsDrawer({
  open, onClose, view, conversation, model, onEditConversation, agentTask, scopes, scopeRemaining,
  onManageAccess,
}: {
  open: boolean;
  onClose: () => void;
  view: "conversation" | "record" | "empty";
  conversation: UnifiedConversation | null;
  model: ModelInfo | null;
  onEditConversation: (kind: "rename" | "instructions" | "delete") => void;
  agentTask: { live: LiveTask | null; summary: AgentTaskSummary | null; detail: AgentTaskDetail | null; label: string } | null;
  scopes: AgentScopeDisplay[];
  scopeRemaining: (scope: AgentScopeDisplay) => number;
  onManageAccess: () => void;
}) {
  const panel = useRef<HTMLElement>(null);
  useEffect(() => {
    if (open) panel.current?.focus();
  }, [open]);
  if (!open) return null;

  return (
    <aside ref={panel} className="drawer" aria-label="Details" tabIndex={-1}>
      <header className="drawer-heading">
        <h2>{view === "record" ? "Execution record" : "Details"}</h2>
        <IconButton label="Close details" onClick={onClose}><CloseIcon /></IconButton>
      </header>
      <div className="drawer-body">
        {view !== "record" && <ChatDetails conversation={conversation} model={model} onEdit={onEditConversation} />}
        {agentTask && (
          <>
            {view !== "record" && <p className="drawer-subheading">{agentTask.label}</p>}
            <AgentDetails live={agentTask.live} summary={agentTask.summary} detail={agentTask.detail} />
          </>
        )}
        <AccessSummary scopes={scopes} remaining={scopeRemaining} onManage={onManageAccess} />
      </div>
    </aside>
  );
}
