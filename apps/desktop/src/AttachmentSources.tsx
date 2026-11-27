import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { ChevronRightIcon, PaperclipIcon, Spinner } from "./icons";
import { passageLabel, recordNotes, type AttachmentRecord } from "./attachments";

type Excerpt = { state: "loading" } | { state: "ready"; text: string } | { state: "unavailable" } | { state: "error" };

/**
 * Passages of attached files that were actually supplied to the model for
 * one answer, in citation order. Text is loaded on demand and only while
 * the attachment is still available in this conversation.
 */
export function AttachmentSources({ messageId, record, supplied, available, focus, onFocusHandled }: {
  messageId: number;
  record: AttachmentRecord;
  /** Passages fitted into the prompt; null when preparation didn't finish. */
  supplied: number | null;
  available: (id: string) => boolean;
  focus: { id: string; at: number } | null;
  onFocusHandled: () => void;
}) {
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [excerpts, setExcerpts] = useState<Record<string, Excerpt>>({});
  const root = useRef<HTMLDivElement>(null);
  const passages = record.passages.slice(0, supplied ?? 0);
  const name = (id: string) => record.attachments.find((item) => item.id === id)?.name ?? "Attachment";
  const notes = recordNotes(record, supplied, available);

  async function resolve(id: string, attachment: string) {
    if (!available(attachment)) {
      setExcerpts((current) => ({ ...current, [id]: { state: "unavailable" } }));
      return;
    }
    setExcerpts((current) => ({ ...current, [id]: { state: "loading" } }));
    try {
      const text = await invoke<string | null>("chat_attachment_evidence", { messageId, citationId: id });
      setExcerpts((current) => ({ ...current, [id]: typeof text === "string" ? { state: "ready", text } : { state: "unavailable" } }));
    } catch {
      setExcerpts((current) => ({ ...current, [id]: { state: "error" } }));
    }
  }

  function toggle(id: string, attachment: string, force?: boolean) {
    const willOpen = force ?? !open.has(id);
    setOpen((current) => {
      const next = new Set(current);
      if (willOpen) next.add(id); else next.delete(id);
      return next;
    });
    if (willOpen && (!excerpts[id] || excerpts[id].state === "error")) void resolve(id, attachment);
  }

  useEffect(() => {
    if (!focus) return;
    const index = Number(focus.id.slice(1)) - 1;
    const passage = passages[index];
    if (passage) {
      toggle(focus.id, passage.attachment, true);
      window.requestAnimationFrame(() => {
        root.current?.querySelector<HTMLElement>(`[data-source="${focus.id}"]`)?.scrollIntoView({ block: "nearest" });
      });
    }
    onFocusHandled();
    // React to a new focus request only.
  }, [focus?.at]);

  return (
    <div className="sources attachment-sources" ref={root} aria-label="Attached files used">
      <p className="sources-heading">
        <span className="sources-title"><PaperclipIcon size={13} />Attached files</span>
        <span className="hint">{record.attachments.map((item) => item.name).join(", ")}</span>
      </p>
      {passages.some(({ cited }) => !cited) && (
        <p className="hint">Uncited excerpts were supplied to the model. The answer did not cite them.</p>
      )}
      {passages.length > 0 && (
        <ol className="source-list">
          {passages.map((passage, index) => {
            const id = `S${index + 1}`;
            const isOpen = open.has(id);
            const excerpt = excerpts[id];
            const present = available(passage.attachment);
            return (
              <li key={id} data-source={id} className={present ? undefined : "source-unavailable"}>
                <button type="button" className="source-toggle" aria-expanded={isOpen} onClick={() => toggle(id, passage.attachment)}>
                  <span className="citation-chip" aria-hidden="true">{id}</span>
                  <span className="source-name" title={name(passage.attachment)}>{name(passage.attachment)}</span>
                  <span className="hint">{passageLabel(passage.source)}</span>
                  {!passage.cited && <span className="badge badge-muted">Uncited</span>}
                  {!present && <span className="badge badge-muted">Unavailable</span>}
                  <ChevronRightIcon size={13} className={isOpen ? "chevron-open" : undefined} />
                </button>
                {isOpen && (
                  <div className="source-excerpt">
                    {!excerpt || excerpt.state === "loading" ? <p className="hint inline-status"><Spinner size={11} />Loading excerpt…</p>
                      : excerpt.state === "ready" ? <pre tabIndex={0} className="source-text" aria-label="Attachment excerpt">{excerpt.text}</pre>
                        : excerpt.state === "unavailable"
                          ? <p className="hint">This attachment is no longer available, so its text can't be shown. The answer above may still quote it.</p>
                          : <p className="error-text">The excerpt couldn't be loaded. <button type="button" className="link-button" onClick={() => void resolve(id, passage.attachment)}>Try again</button></p>}
                  </div>
                )}
              </li>
            );
          })}
        </ol>
      )}
      {notes.length > 0 && <ul className="coverage-notes">{notes.map((note) => <li key={note}>{note}</li>)}</ul>}
    </div>
  );
}
