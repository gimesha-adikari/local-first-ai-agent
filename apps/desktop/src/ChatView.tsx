import {
  outputBudgetNote, responseNotice, splitReasoning, suggestedOutputLimit, type ContinuationStatus, type Message,
} from "./chat";
import { useCallback, useMemo, useState } from "react";
import { ArrowDownIcon, BookIcon, ChevronRightIcon, RefreshIcon, Spinner, ThinkIcon } from "./icons";
import { contextUsageLabel, coverageLines, coverageSummary, isContextual, type KnowledgeCoverage } from "./knowledge";
import MarkdownMessage, { CopyButton } from "./MarkdownMessage";
import { SourceList, useSourceReferences } from "./Sources";
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

/** Library state that source references and coverage notes depend on. */
export type KnowledgeContext = {
  /** Changes when retained source text can change. */
  revision: number;
  documentName: (id: number) => string | null;
  /** Live coverage kept for this session when the attempt saved none (ordinary Chat routes). */
  liveCoverage: (messageId: number) => KnowledgeCoverage | null;
};

function AssistantMessage({
  message, latest, busy, contextLength, actions, continuation, modelName = null, knowledge,
}: {
  knowledge: KnowledgeContext;
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
  const contextual = isContextual(message);
  const legacyUsage = message.context?.memoryEnabled == null ? message.stats?.contextUsage : null;
  const usage = message.context?.fitted ?? legacyUsage ?? null;
  const coverage = message.context?.coverage ?? message.routeCoverage ?? knowledge.liveCoverage(message.id);
  const summary = coverageSummary(coverage);
  const citations = useSourceReferences(message.id, !streaming && contextual, knowledge.revision);
  const [focus, setFocus] = useState<{ id: string; at: number } | null>(null);
  const onSelect = useCallback((id: string) => setFocus({ id, at: Date.now() }), []);
  const citationProps = useMemo(() => {
    const ids = new Set(citations.filter(({ cited }) => cited).map(({ citationId }) => citationId));
    return ids.size ? { ids, onSelect } : undefined;
  }, [citations, onSelect]);
  const notes = coverageLines(coverage, streaming ? null : usage, (id) =>
    citations.find(({ documentId }) => documentId === id)?.documentName ?? knowledge.documentName(id),
  true);
  const usageLabel = !streaming ? contextUsageLabel(usage) : null;

  return (
    <div className="assistant-turn" aria-label="Assistant">
      {summary && (
        <p className={`context-note context-${summary.tone}`}>
          <BookIcon size={13} />
          <span className="context-title">{summary.title}</span>
          {summary.detail && <span className="context-detail">{summary.detail}</span>}
        </p>
      )}
      {reasoning !== null && <Reasoning text={reasoning} complete={reasoningComplete} streaming={streaming} />}
      {answer && <MarkdownMessage text={answer} citations={citationProps} />}
      {!streaming && citations.length > 0 && (
        <SourceList key={`${message.id}:${knowledge.revision}`} messageId={message.id} citations={citations} revision={knowledge.revision}
          focus={focus} onFocusHandled={() => setFocus(null)} />
      )}
      {!streaming && message.context?.coverage && !usage && (
        <p className="hint">Prompt preparation did not complete. Fitted counts are unavailable.</p>
      )}
      {notes.length > 0 && (
        <ul className="coverage-notes">{notes.map((note) => <li key={note}>{note}</li>)}</ul>
      )}
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
                title={contextual ? "Search again with your current knowledge settings" : undefined}
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
            <IconButton label={contextual ? "Regenerate with current knowledge settings" : "Regenerate response"} disabled={busy}
              onClick={() => onRegenerate({})}>
              <RefreshIcon size={14} />
            </IconButton>
          )}
          {modelName && <span className="message-meta" title={modelName}>{modelLabel(modelName)}</span>}
          {usageLabel && (
            <span className="message-meta" title="Passages and memories supplied in this attempt. Contextual answers are excluded from later prompts.">
              {usageLabel}
            </span>
          )}
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
  turn, latest, busy, contextLength, actions, continuation, knowledge,
}: {
  knowledge: KnowledgeContext;
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
            contextLength={contextLength} actions={actions} continuation={continuation} knowledge={knowledge} />
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
