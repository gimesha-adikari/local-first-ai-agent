import { useEffect, useId, useRef, useState } from "react";
import {
  clock, currentActivity, durationLabel, errorGuidance, eventTitle, executionLabel, outcomeSummary, pdfAnswerScope,
  progressSteps, stateLabel, isTerminal, type ProgressStep,
} from "./agent";
import type { AgentTaskDetail, AgentTaskSummary, IpcError } from "./agentTypes";
import {
  AgentIcon, AlertIcon, CheckIcon, ChevronRightIcon, ClockIcon, InfoIcon, LaunchIcon, Spinner,
} from "./icons";
import MarkdownMessage from "./MarkdownMessage";
import NativeResults from "./NativeResults";
import { Callout } from "./ui";
import {
  contextOmissionNote, permissionGuidance,
  type AgentTurnDisplay, type LiveTurn, type ScopeContext, type UnifiedTurn,
} from "./unified";
import type { AgentController, LiveTask } from "./useAgent";

function StepIcon({ status }: { status: ProgressStep["status"] }) {
  switch (status) {
    case "done": return <CheckIcon size={14} />;
    case "active": return <Spinner size={13} />;
    case "waiting": return <ClockIcon size={14} />;
    case "failed": return <AlertIcon size={14} />;
    default: return <InfoIcon size={14} />;
  }
}

function Steps({ steps }: { steps: ProgressStep[] }) {
  if (!steps.length) return null;
  return (
    <ul className="steps">
      {steps.map((step) => (
        <li key={step.key} className={`step step-${step.status}`}>
          <StepIcon status={step.status} /><span>{step.label}</span>
        </li>
      ))}
    </ul>
  );
}

const savedSteps = (detail: AgentTaskDetail | undefined): ProgressStep[] => (detail?.executions ?? []).map((item) => ({
  key: item.operationId,
  label: executionLabel(item),
  status: item.status === "succeeded" ? "done" : item.status === "failed" ? "failed" : "notice",
}));

/** What the turn is waiting on: the model, a native tool, or the person. */
function phaseLabel(task: LiveTask, steps: ProgressStep[]): string {
  if (task.approval) return "Waiting for your approval";
  const last = steps.at(-1);
  if (last?.status === "active") return last.label;
  return currentActivity(task.events.map((entry) => entry.event));
}

function Activity({
  task, running, now, detail,
}: { task: LiveTask; running: boolean; now: number; detail?: AgentTaskDetail }) {
  const [open, setOpen] = useState(false);
  const logId = useId();
  const events = task.events.map((entry) => entry.event);
  const liveSteps = progressSteps(events, running);
  const steps = liveSteps.length || running ? liveSteps : savedSteps(detail);
  const elapsedMs = running && task.startedPerf !== null
    ? now - task.startedPerf
    : (task.endedAt ?? Date.now()) - task.submittedAt;
  const remaining = running && task.timeoutMs ? (task.timeoutMs - elapsedMs) / 1000 : null;
  const phase = running ? phaseLabel(task, liveSteps) : null;
  const waitingOnPerson = !!task.approval;

  return (
    <div className="activity">
      <button type="button" className="activity-toggle" aria-expanded={open} aria-controls={logId}
        onClick={() => setOpen((value) => !value)}>
        {running ? (
          <span className={`activity-live${waitingOnPerson ? " activity-waiting" : ""}`} role="status" aria-live="polite">
            {waitingOnPerson ? <ClockIcon size={14} /> : <Spinner size={13} />}{phase}…
          </span>
        ) : (
          <span>{task.events.length ? `Worked for ${durationLabel(elapsedMs)}` : "Agent activity"}</span>
        )}
        {running && (
          <span className="activity-clock">
            {clock(elapsedMs / 1000)}{remaining !== null && <> · {clock(remaining)} left</>}
          </span>
        )}
        <ChevronRightIcon size={14} className={open ? "chevron-open" : undefined} />
      </button>
      <Steps steps={steps} />
      {open && (
        <ol id={logId} className="event-log" aria-label="Activity details">
          {task.events.length === 0 && <li><span>{running ? "Waiting for the first update." : "Live events for this turn aren't available in this window. Open details for the saved record."}</span></li>}
          {task.events.map(({ key, at, event }) => (
            <li key={key}>
              <time dateTime={new Date(at).toISOString()}>{new Date(at).toLocaleTimeString()}</time>
              <span>{eventTitle(event)}</span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

function SavedActivity({ detail, running, state, onLoad }: {
  detail: AgentTaskDetail | undefined;
  running: boolean;
  state: string | null;
  onLoad: (() => Promise<unknown>) | null;
}) {
  const steps = savedSteps(detail);
  const [loading, setLoading] = useState(false);
  return (
    <div className="activity">
      <p className="activity-static">
        {running ? <span className="activity-live" role="status"><Spinner size={13} />Working…</span>
          : detail ? `Agent activity · ${stateLabel(detail.task.state)}`
            : state ? `Agent activity · ${stateLabel(state)}` : "Agent activity"}
        {!running && !detail && onLoad && (
          <button type="button" className="link-button" disabled={loading}
            onClick={() => { setLoading(true); void onLoad().finally(() => setLoading(false)); }}>{loading ? "Loading steps…" : "Show steps"}</button>
        )}
      </p>
      {detail && (steps.length ? <Steps steps={steps} /> : !running && <p className="hint">No tools were run.</p>)}
    </div>
  );
}

function Approval({ task, agent }: { task: LiveTask; agent: AgentController }) {
  const approval = task.approval;
  const headingId = useId();
  const ref = useRef<HTMLElement>(null);
  const operationId = approval?.operationId;
  useEffect(() => {
    // Keep the decision visible, including in small windows.
    if (operationId) ref.current?.scrollIntoView({ block: "nearest" });
  }, [operationId]);
  if (!approval) return null;
  const secondsLeft = Math.max(0, Math.ceil((approval.expiresAt - agent.now) / 1000));
  const expired = secondsLeft <= 0;
  const target = approval.target;
  const disabled = agent.approvalBusy || agent.cancelling;

  return (
    <section ref={ref} className="approval" role="group" aria-labelledby={headingId}>
      <header>
        <span className="approval-icon"><LaunchIcon size={16} /></span>
        <div>
          <h3 id={headingId}>{target ? `Open ${target.displayName}?` : "Approval needed"}</h3>
          <p className="hint">The agent is asking to launch an application. Approval applies to this launch only.</p>
        </div>
      </header>
      {target ? (
        <dl className="approval-target">
          <div><dt>Application</dt><dd>{target.displayName}</dd></div>
          <div><dt>Desktop entry</dt><dd><code>{target.desktopFile}</code></dd></div>
          <div><dt>Executable</dt><dd><code>{target.executable}</code></dd></div>
        </dl>
      ) : (
        <Callout tone="warning">The trusted target could not be verified, so this launch can't be approved.</Callout>
      )}
      <p className={`approval-timer${secondsLeft <= 10 ? " approval-timer-urgent" : ""}`} aria-live="off">
        <ClockIcon size={14} />{expired ? "This request has expired. Nothing was launched." : `Expires in ${clock(secondsLeft)}`}
      </p>
      {agent.error && <p className="error-text" role="alert">{agent.error}</p>}
      <div className="approval-actions">
        <button type="button" className="ghost-button" disabled={agent.cancelling} onClick={() => void agent.cancel()}>
          {agent.cancelling ? "Cancelling…" : "Cancel task"}
        </button>
        <span className="spacer" />
        <button type="button" className="secondary-button" disabled={disabled || expired} onClick={() => void agent.respond(false)}>Deny</button>
        <button type="button" className="primary-button" disabled={disabled || !target || expired}
          onClick={() => void agent.respond(true)}>
          {agent.approvalBusy ? "Responding…" : "Approve once"}
        </button>
      </div>
    </section>
  );
}

function guidanceFor(error: IpcError, state: string | null, scopes: ScopeContext) {
  return permissionGuidance(error.code, scopes) ?? errorGuidance(error, state ?? undefined);
}

/** One linked Agent turn: its activity, native results, approval and saved answer. */
export function AgentTurnView({
  turn, live, detail, display, agent, scopes, isActive, onDetails,
}: {
  turn: UnifiedTurn;
  live: LiveTurn | undefined;
  detail: AgentTaskDetail | undefined;
  display: AgentTurnDisplay;
  agent: AgentController;
  scopes: ScopeContext;
  isActive: boolean;
  onDetails: (turnId: string) => void;
}) {
  const error = display.error;
  const guidance = error && !display.running ? guidanceFor(error, display.state, scopes) : null;
  const omission = contextOmissionNote(display.excludedTurns);
  const toolsRan = display.succeededSteps > 0 || (live?.finished?.executions.length ?? detail?.executions.length ?? 0) > 0;
  const discoveryOnly = !display.running && display.state === "completed"
    && (detail?.executions ?? live?.finished?.executions ?? []).some((item) => item.tool === "apps.list" && item.status === "succeeded")
    && !(detail?.executions ?? live?.finished?.executions ?? []).some((item) => item.tool === "apps.launch");
  const interrupted = display.state === "interrupted" || display.assistantState === "interrupted";
  const summaryState = display.state && display.state !== "running" ? display.state : null;
  const successful = display.nativeResults.filter((item) => item.observation.status === "success").length;
  const pdfScope = pdfAnswerScope(display.nativeResults);

  return (
    <div className="assistant-turn agent-turn" aria-label="Agent turn">
      <p className="turn-kind"><AgentIcon size={13} />Agent</p>
      {live && (live.events.length || display.running)
        ? <Activity task={live.timeoutMs ? live : { ...live, timeoutMs: turn.taskTimeoutMs }} running={display.running} now={agent.now} detail={detail} />
        : <SavedActivity detail={detail} running={display.running} state={turn.taskState}
          onLoad={turn.taskId ? () => agent.loadDetail(turn.taskId!) : null} />}
      {omission && <p className="context-note"><InfoIcon size={13} />{omission}</p>}
      <NativeResults results={display.nativeResults} targets={live?.targets ?? {}} />
      {display.nativeUnavailable && (
        <Callout tone="neutral" title="Native result details are no longer available">
          {display.succeededSteps === 1 ? "A native step ran in this turn" : `${display.succeededSteps} native steps ran in this turn`}, but
          raw tool results are kept only for recent turns in the current session and are never saved with the conversation.
          {display.answer ? " The saved answer below is the model's reply from that time." : ""}
        </Callout>
      )}
      {isActive && live && <Approval task={live} agent={agent} />}
      {display.answer && (
        <section className="agent-answer" aria-label="Model answer">
          <h3 className="section-label">
            Model answer
            <span>
              {toolsRan ? "Interpretation of the native results" : "No tools were run. This is the model's own reply, not a verified result."}
              {display.answerSource === "saved" ? " · Saved in this conversation" : ""}
            </span>
          </h3>
          <MarkdownMessage text={display.answer} />
          {pdfScope && <p className="context-note"><InfoIcon size={13} />{pdfScope}</p>}
          {discoveryOnly && <p className="hint">No application launch was dispatched.</p>}
        </section>
      )}
      {!display.running && !display.answer && !error && !interrupted && summaryState === "completed" && (
        <Callout tone="neutral" title="No written answer">
          {successful > 0
            ? "The model didn't write an answer. The native results above are shown exactly as returned. Ask a follow-up to continue; completed operations are never repeated automatically."
            : "The turn ended without a written answer."}
        </Callout>
      )}
      {interrupted && !error && (
        <Callout tone="warning" title="Interrupted">
          The app closed or stopped before this turn finished. A step whose outcome wasn't recorded is marked uncertain
          and is never repeated automatically.
        </Callout>
      )}
      {guidance && (
        <Callout tone={error?.code === "cancelled" || display.state === "denied" ? "neutral" : "error"} title={guidance.title} role="alert">
          {guidance.detail}
          {successful > 0 && error?.code !== "cancelled" && (
            <p>The native results above are verified and remain valid. They won't be run again automatically.</p>
          )}
        </Callout>
      )}
      {live?.finished?.journalIncomplete && (
        <Callout tone="warning" title="Task journal incomplete">
          Some records for this turn couldn't be saved. Don't treat its history as a complete record of what ran.
        </Callout>
      )}
      {!display.running && (
        <footer className="agent-footer">
          <span className={`state-dot state-${summaryState ?? "interrupted"}`} aria-hidden="true" />
          <span>
            {summaryState
              ? outcomeSummary({ state: summaryState, answer: display.answer, error }, successful)
              : turn.taskId ? "Status unavailable" : "Not linked to a task"}
          </span>
          {turn.taskId && <><span aria-hidden="true">·</span>
            <button type="button" className="link-button" onClick={() => onDetails(turn.id)}>View details</button></>}
        </footer>
      )}
    </div>
  );
}

/** A standalone journal record from before Agent turns lived in conversations. */
export function LegacyRecord({
  summary, detail, onDetails,
}: {
  summary: AgentTaskSummary | null;
  detail: AgentTaskDetail | null;
  onDetails: () => void;
}) {
  const task = detail?.task ?? summary;
  if (!task) return <p className="empty-line" role="status">Loading record…</p>;
  const steps = savedSteps(detail ?? undefined);
  const error = (task.error || task.errorMessage) && isTerminal(task.state)
    ? errorGuidance({ code: task.error ?? task.state, message: task.errorMessage ?? "" }, task.state) : null;

  return (
    <div className="assistant-turn">
      <Callout tone="neutral" title="Execution record">
        This is a saved execution record from the Agent journal, not a conversation. Older standalone records keep only
        what ran and how it ended. Their request, answer and native result contents were never saved.
      </Callout>
      <div className="activity">
        <p className="activity-static">Agent task started {new Date(task.createdAt).toLocaleString()}</p>
        {detail ? (steps.length ? <Steps steps={steps} /> : <p className="hint">No tools were run.</p>)
          : <p className="hint">Loading saved activity…</p>}
      </div>
      {detail?.nativeResults && detail.nativeResults.length > 0 && <NativeResults results={detail.nativeResults} targets={{}} />}
      {error && <Callout tone="error" title={error.title}>{error.detail}</Callout>}
      <footer className="agent-footer">
        <span className={`state-dot state-${task.state}`} aria-hidden="true" />
        <span>{stateLabel(task.state)}{!isTerminal(task.state) && " (not finalized)"}</span>
        <span aria-hidden="true">·</span>
        <button type="button" className="link-button" onClick={onDetails}>View details</button>
      </footer>
    </div>
  );
}
