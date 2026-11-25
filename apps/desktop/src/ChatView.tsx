import {
  outputBudgetNote, responseNotice, splitReasoning, suggestedOutputLimit, type ContinuationStatus, type Message,
} from "./chat";
import { ArrowDownIcon, ChevronRightIcon, RefreshIcon, Spinner, ThinkIcon } from "./icons";
import MarkdownMessage, { CopyButton } from "./MarkdownMessage";
import { modelLabel } from "./models";
import { Callout, IconButton } from "./ui";
import type { UnifiedTurn } from "./unified";

export type RegenerateRequest = { maxTokens?: number };

function Reasoning({ text, complete, streaming }: { text: string; complete: boolean; streaming: boolean }) {
  const label = complete ? "Reasoning" : streaming ? "Thinking" : "Incomplete reasoning";
  return (
    <details className="reasoning">
      <summary>
        {streaming && !complete ? <Spinner size={12} /> : <ThinkIcon size={14} />}
        <span>{label}{streaming && !complete ? "…" : ""}</span>
        <ChevronRightIcon size={13} className="reasoning-chevron" />
      </summary>
      <div className="reasoning-body">
        {/* Raw model reasoning is shown verbatim, not interpreted as Markdown. */}
        {text ? <p className="reasoning-text">{text}</p> : <p className="hint">Waiting for reasoning text.</p>}
      </div>
    </details>
  );
}

export type ChatActions = {
  onRegenerate: (request: RegenerateRequest) => void;
  onContinue: (messageId: number) => void;
  onOpenLimits: () => void;
};

function AssistantMessage({
  message, latest, busy, contextLength, actions, continuation, modelName = null,
}: {
  modelName?: string | null;
  message: Message;
  latest: boolean;
  busy: boolean;
  contextLength: number | null;
  actions: ChatActions;
  continuation: ContinuationStatus | null;
}) {
  const { onRegenerate, onOpenLimits, onContinue } = actions;
  const streaming = message.state === "streaming";
  const { answer, reasoning, reasoningComplete } = splitReasoning(message.content, streaming);
  const limited = message.stats?.reason === "token_limit";
  // The latest segment's effective budget; generatedTokens is cumulative across continuations.
  const reached = limited ? message.stats?.outputBudget?.effective ?? message.stats?.generatedTokens : undefined;
  const own = latest && continuation?.messageId === message.id ? continuation : null;
  const canContinue = !!own && own.remaining !== null && own.remaining > 0 && !own.blocked;
  const notice = responseNotice(message, reached, canContinue);
  const larger = notice?.suggestLimit && contextLength && reached ? suggestedOutputLimit(reached, contextLength) : null;
  const excluded = message.stats?.excludedTurns ?? 0;
  const budgetNote = !streaming ? outputBudgetNote(message.stats) : null;

  return (
    <div className="assistant-turn" aria-label="Assistant">
      {reasoning !== null && <Reasoning text={reasoning} complete={reasoningComplete} streaming={streaming} />}
      {answer && <MarkdownMessage text={answer} />}
      {streaming && !answer && reasoning === null && (
        <p className="activity-live" role="status"><Spinner size={13} />Generating…</p>
      )}
      {notice && !streaming && (
        <Callout tone={notice.tone} title={notice.title}
          actions={latest && (
            <>
              {canContinue && (
                <button type="button" className="primary-button small" disabled={busy}
                  onClick={() => onContinue(message.id)}
                  title={`Append to this reply. ${own!.remaining} continuation ${own!.remaining === 1 ? "request" : "requests"} left.`}>
                  <ArrowDownIcon size={14} />Continue generating
                  <span className="button-count">{own!.remaining} left</span>
                </button>
              )}
              {own?.checking && <span className="hint inline-status"><Spinner size={12} />Checking continuation…</span>}
              <button type="button" className="secondary-button small" disabled={busy}
                onClick={() => onRegenerate({})}><RefreshIcon size={14} />Regenerate</button>
              {larger && (
                <button type="button" className="secondary-button small" disabled={busy}
                  onClick={() => onRegenerate({ maxTokens: larger })}>Regenerate with {larger} tokens</button>
              )}
              {notice.suggestLimit && (
                <button type="button" className="link-button" onClick={onOpenLimits}>Output limit settings</button>
              )}
            </>
          )}>
          {notice.detail}
          {own?.blocked && (
            <span className="continuation-blocked"><strong>{own.blocked.title}.</strong> {own.blocked.detail}</span>
          )}
        </Callout>
      )}
      {!streaming && (
        <div className="message-actions">
          {message.content && <CopyButton text={answer || message.content} label="Copy response" />}
          {latest && (
            <IconButton label="Regenerate response" disabled={busy} onClick={() => onRegenerate({})}>
              <RefreshIcon size={14} />
            </IconButton>
          )}
          {modelName && <span className="message-meta" title={modelName}>{modelLabel(modelName)}</span>}
          {budgetNote && <span className="message-meta" title="Thinking and the answer share this budget.">{budgetNote}</span>}
          {excluded > 0 && (
            <span className="message-meta">
              {excluded} earlier {excluded === 1 ? "turn" : "turns"} didn't fit in the model's context
            </span>
          )}
        </div>
      )}
    </div>
  );
}

const attemptLabel: Record<Message["state"], string> = {
  streaming: "In progress",
  complete: "Completed",
  cancelled: "Stopped",
  interrupted: "Interrupted",
  failed: "Failed",
  truncated: "Cut off",
};

/** One saved Chat turn: the user message and its assistant attempts. */
export function ChatTurnView({
  turn, latest, busy, contextLength, actions, continuation,
}: {
  turn: UnifiedTurn;
  latest: boolean;
  busy: boolean;
  contextLength: number | null;
  actions: ChatActions;
  continuation: ContinuationStatus | null;
}) {
  const attempts = turn.assistants;
  return (
    <>
      <div className="user-turn" aria-label="You"><p>{turn.user.content}</p></div>
      {attempts.map((attempt, index) => {
        const last = index === attempts.length - 1;
        const body = (
          <AssistantMessage message={attempt.message} modelName={attempt.modelName} latest={latest && last} busy={busy}
            contextLength={contextLength} actions={actions} continuation={continuation} />
        );
        if (last) return <div key={attempt.message.id}>{body}</div>;
        return (
          <details key={attempt.message.id} className="earlier-attempt">
            <summary><ChevronRightIcon size={13} />Previous attempt · {attemptLabel[attempt.message.state]}</summary>
            {body}
          </details>
        );
      })}
    </>
  );
}
