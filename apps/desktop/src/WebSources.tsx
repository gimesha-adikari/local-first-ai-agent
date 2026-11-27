import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { ChevronRightIcon, ExternalIcon, GlobeIcon } from "./icons";
import { errorMessage } from "./inference";
import { historyNote, researchSummary, supplyLabel, type ResearchView } from "./research";

/** Validated sources for a research answer, refetched when the turn finishes. */
export function useResearchView(messageId: number, enabled: boolean, revision: number) {
  const [snapshot, setSnapshot] = useState<{ messageId: number; view: ResearchView | null } | null>(null);
  useEffect(() => {
    if (!enabled) {
      setSnapshot(null);
      return;
    }
    let active = true;
    invoke<ResearchView | null>("chat_web_research", { messageId })
      .then((view) => { if (active) setSnapshot({ messageId, view }); })
      .catch(() => { if (active) setSnapshot({ messageId, view: null }); });
    return () => { active = false; };
  }, [messageId, enabled, revision]);
  return enabled && snapshot?.messageId === messageId ? snapshot.view : null;
}

async function open(messageId: number, target: { kind: "source" | "discovered"; index: number }) {
  await invoke("web_research_open", { messageId, target });
}

/**
 * Source cards for one research answer. Links open in the system browser
 * only on an explicit click; nothing from a page is rendered or previewed.
 */
export function WebSourceList({ messageId, view, modelName, focus, onFocusHandled }: {
  messageId: number;
  view: ResearchView;
  modelName: string | null;
  focus: { id: string; at: number } | null;
  onFocusHandled: () => void;
}) {
  const [openIds, setOpenIds] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const { record } = view;
  const note = historyNote(view);

  useEffect(() => {
    if (!focus) return;
    const source = view.sources.find(({ citationId }) => citationId === focus.id);
    if (source) {
      setOpenIds((current) => new Set(current).add(source.citationId));
      window.requestAnimationFrame(() => {
        root.current?.querySelector<HTMLElement>(`[data-source="${source.citationId}"]`)?.scrollIntoView({ block: "nearest" });
      });
    }
    onFocusHandled();
    // React to a new focus request only.
  }, [focus?.at]);

  async function launch(target: { kind: "source" | "discovered"; index: number }) {
    setError(null);
    try {
      await open(messageId, target);
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }

  return (
    <div className="sources web-sources" ref={root} aria-label="Web sources">
      <p className="sources-heading">
        <span className="sources-title"><GlobeIcon size={13} />Web sources</span>
        <span className="hint">{researchSummary(record)}{record.synthesis === "remote" && modelName ? ` (${modelName})` : ""}</span>
      </p>
      {record.synthesis !== "none" && view.sources.some((source) => source.supplied && !source.cited) && (
        <p className="hint">A citation shows which source the answer points to. It doesn't prove every statement is supported.</p>
      )}
      {view.sources.length > 0 && (
        <ol className="source-list">
          {view.sources.map((source, index) => {
            const isOpen = openIds.has(source.citationId);
            const title = source.title ?? source.host ?? "Source details not saved";
            return (
              <li key={source.citationId} data-source={source.citationId}>
                <div className="web-source-row">
                  <button type="button" className="source-toggle" aria-expanded={isOpen}
                    onClick={() => setOpenIds((current) => {
                      const next = new Set(current);
                      if (next.has(source.citationId)) next.delete(source.citationId); else next.add(source.citationId);
                      return next;
                    })}>
                    <span className="citation-chip" aria-hidden="true">{source.citationId}</span>
                    <span className="source-name" title={title}>{title}</span>
                    {source.host && <span className="hint">{source.host}</span>}
                    <span className={`badge ${source.evidence === "page" ? "badge-ready" : "badge-warning"}`}>
                      {source.evidence === "page" ? "Page read" : "Search snippet"}
                    </span>
                    <span className="badge badge-muted">{supplyLabel(source, record.synthesis)}</span>
                    <ChevronRightIcon size={13} className={isOpen ? "chevron-open" : undefined} />
                  </button>
                  {source.openable && (
                    <button type="button" className="icon-button" aria-label={`Open ${title} in your browser`}
                      title="Open in your browser" onClick={() => void launch({ kind: "source", index })}>
                      <ExternalIcon size={14} />
                    </button>
                  )}
                </div>
                {isOpen && (
                  <div className="source-excerpt">
                    {source.url && <p className="hint web-source-url">{source.url}</p>}
                    {source.published && <p className="hint">Published {source.published}</p>}
                    {source.excerpt
                      ? <pre tabIndex={0} className="source-text" aria-label="Evidence text">{source.excerpt}</pre>
                      : <p className="hint">The evidence text was shown only during the session that ran this research.</p>}
                    {source.evidence === "snippet" && (
                      <p className="hint">Only the search provider's snippet was available. The page itself wasn't read.</p>
                    )}
                  </div>
                )}
              </li>
            );
          })}
        </ol>
      )}
      {view.limitations.length > 0 && (
        <ul className="coverage-notes" aria-label="Pages that couldn't be read">
          {view.limitations.map((limitation) => <li key={limitation}>{limitation}</li>)}
        </ul>
      )}
      {view.discovered.length > 0 && (
        <details className="web-discovered">
          <summary><ChevronRightIcon size={13} />Other results ({view.discovered.length}), not supplied to the model</summary>
          <ul>
            {view.discovered.map((result, index) => (
              <li key={result.url}>
                <button type="button" className="link-button" title={result.url}
                  onClick={() => void launch({ kind: "discovered", index })}>{result.title}</button>
                <span className="hint"> {result.host}{result.attempted ? " · couldn't be read" : ""}</span>
                {result.snippet && <p className="hint">{result.snippet}</p>}
              </li>
            ))}
          </ul>
        </details>
      )}
      {note && <p className="hint">{note}</p>}
      {error && <p className="error-text" role="alert">{error}</p>}
    </div>
  );
}
