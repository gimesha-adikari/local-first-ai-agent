import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { ChevronRightIcon, QuoteIcon, Spinner } from "./icons";
import { citationState, sourceLabel, type SourceCitation } from "./knowledge";

type Excerpt = { state: "loading" } | { state: "ready"; text: string } | { state: "unavailable" } | { state: "error" };

/**
 * Validated references for one assistant message. Refetched whenever the
 * library changes so deleted or replaced documents show as unavailable.
 */
export function useSourceReferences(messageId: number, enabled: boolean, revision: number) {
  const [snapshot, setSnapshot] = useState<{ messageId: number; revision: number; citations: SourceCitation[] } | null>(null);
  useEffect(() => {
    if (!enabled) {
      setSnapshot(null);
      return;
    }
    let active = true;
    invoke<SourceCitation[]>("chat_supplied_evidence", { messageId })
      .then((next) => { if (active) setSnapshot({ messageId, revision, citations: next }); })
      .catch(() => { if (active) setSnapshot(null); });
    return () => { active = false; };
  }, [messageId, enabled, revision]);
  return enabled && snapshot?.messageId === messageId && snapshot.revision === revision ? snapshot.citations : [];
}

export function SourceList({
  messageId, citations, revision, focus, onFocusHandled,
}: {
  messageId: number;
  citations: SourceCitation[];
  revision: number;
  /** A citation ID chosen from the answer text. */
  focus: { id: string; at: number } | null;
  onFocusHandled: () => void;
}) {
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [excerpts, setExcerpts] = useState<Record<string, Excerpt>>({});
  const root = useRef<HTMLDivElement>(null);
  const epoch = useRef(0);

  // Never keep an excerpt across a library change: re-resolve on demand.
  useEffect(() => {
    epoch.current++;
    setExcerpts({});
  }, [revision, messageId]);

  async function resolve(citation: SourceCitation) {
    if (citationState(citation) === "unavailable") {
      setExcerpts((current) => ({ ...current, [citation.citationId]: { state: "unavailable" } }));
      return;
    }
    const started = epoch.current;
    setExcerpts((current) => ({ ...current, [citation.citationId]: { state: "loading" } }));
    try {
      const text = await invoke<string | null>("chat_resolve_evidence", { messageId, citationId: citation.citationId });
      if (epoch.current !== started) return;
      setExcerpts((current) => ({
        ...current,
        [citation.citationId]: citationState(citation, typeof text === "string" ? text : null) === "ready"
          ? { state: "ready", text: text as string } : { state: "unavailable" },
      }));
    } catch {
      if (epoch.current === started) setExcerpts((current) => ({ ...current, [citation.citationId]: { state: "error" } }));
    }
  }

  function toggle(citation: SourceCitation, force?: boolean) {
    const id = citation.citationId;
    const willOpen = force ?? !open.has(id);
    setOpen((current) => {
      const next = new Set(current);
      if (willOpen) next.add(id); else next.delete(id);
      return next;
    });
    if (willOpen && (!excerpts[id] || excerpts[id].state === "error")) void resolve(citation);
  }

  useEffect(() => {
    if (!focus) return;
    const citation = citations.find(({ citationId }) => citationId === focus.id);
    if (citation) {
      toggle(citation, true);
      window.requestAnimationFrame(() => {
        root.current?.querySelector<HTMLElement>(`[data-source="${citation.citationId}"]`)?.scrollIntoView({ block: "nearest" });
      });
    }
    onFocusHandled();
    // React to a new focus request only.
  }, [focus?.at]);

  if (!citations.length) return null;
  const unavailable = citations.filter(({ available }) => !available).length;

  return (
    <div className="sources" ref={root} aria-label="Evidence supplied to the model">
      <p className="sources-heading">
        <span className="sources-title"><QuoteIcon size={13} />Evidence supplied to the model</span>
        {unavailable > 0 && <span className="hint sources-availability">{unavailable} no longer available</span>}
      </p>
      {citations.some(({ cited }) => !cited) && <p className="hint">Uncited passages were supplied to the model. The answer did not cite them.</p>}
      <ol className="source-list">
        {citations.map((citation) => {
          const id = citation.citationId;
          const isOpen = open.has(id);
          const excerpt = excerpts[id];
          return (
            <li key={id} data-source={id} className={citation.available ? undefined : "source-unavailable"}>
              <button type="button" className="source-toggle" aria-expanded={isOpen} onClick={() => toggle(citation)}>
                <span className="citation-chip" aria-hidden="true">{id}</span>
                <span className="source-name" title={citation.documentName}>{citation.documentName}</span>
                <span className="hint">{sourceLabel(citation.source)}</span>
                {!citation.cited && <span className="badge badge-muted">Uncited</span>}
                {!citation.available && <span className="badge badge-muted">Unavailable</span>}
                <ChevronRightIcon size={13} className={isOpen ? "chevron-open" : undefined} />
              </button>
              {isOpen && (
                <div className="source-excerpt">
                  {!excerpt || excerpt.state === "loading" ? <p className="hint inline-status"><Spinner size={11} />Loading excerpt…</p>
                    : excerpt.state === "ready" ? <pre tabIndex={0} className="source-text" aria-label="Source excerpt">{excerpt.text}</pre>
                      : excerpt.state === "unavailable"
                        ? <p className="hint">This source is no longer available because its document was deleted or replaced. The answer above may still quote it.</p>
                        : <p className="error-text">The excerpt couldn't be loaded. <button type="button" className="link-button" onClick={() => void resolve(citation)}>Try again</button></p>}
                </div>
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}
