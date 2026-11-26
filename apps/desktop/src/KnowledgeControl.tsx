import { useState } from "react";
import { BookIcon, ChevronDownIcon, SearchIcon } from "./icons";
import {
  maxSelectedDocuments, modeHint, modeLabel, retrievalModes,
  type DocumentIndexState, type Grounding, type KnowledgeDocument, type KnowledgeSelection, type Readiness,
} from "./knowledge";
import { Popover, Segmented, Switch } from "./ui";

const groundingHint: Record<Grounding, string> = {
  auto: "Searches, overviews and questions use the documents. Unrelated requests get an ordinary answer marked as having no document evidence.",
  documents: "Every answer is grounded in the selected documents. Requests they can't answer say so.",
};

/**
 * Compact composer control. Knowledge is off until turned on, searches only
 * the documents ticked here, and never changes the mode by itself.
 */
export default function KnowledgeControl({
  selection, onChange, documents, loaded, indexStates, readiness, memoryEnabled, disabled,
  onOpenKnowledge, onOpenMemory,
}: {
  selection: KnowledgeSelection;
  onChange: (next: KnowledgeSelection) => void;
  documents: KnowledgeDocument[];
  loaded: boolean;
  indexStates: Record<number, DocumentIndexState | null>;
  readiness: Readiness;
  memoryEnabled: boolean | null;
  disabled: boolean;
  onOpenKnowledge: () => void;
  onOpenMemory: () => void;
}) {
  const [filter, setFilter] = useState("");
  const selected = new Set(selection.documentIds);
  const visible = filter.trim()
    ? documents.filter((document) => document.name.toLocaleLowerCase().includes(filter.trim().toLocaleLowerCase()))
    : documents;
  const dense = selection.mode !== "keyword";
  const count = selection.documentIds.length;
  const attention = selection.enabled && !!readiness.blocked;

  const toggleDocument = (id: number) => {
    const next = selected.has(id) ? selection.documentIds.filter((value) => value !== id)
      : [...selection.documentIds, id].slice(0, maxSelectedDocuments);
    onChange({ ...selection, documentIds: next });
  };

  const strict = selection.grounding === "documents";
  const label = selection.enabled
    ? `Knowledge: ${strict ? "documents only" : "automatic"}, ${modeLabel[selection.mode]}, ${count} ${count === 1 ? "document" : "documents"}`
    : "Knowledge: off";

  return (
    <Popover label={label} placement="above" fitViewport disabled={disabled} panelClassName="knowledge-popover"
      triggerClassName={`composer-pill knowledge-pill${selection.enabled ? " is-on" : ""}${attention ? " has-dot" : ""}`}
      trigger={<>
        <BookIcon size={14} />
        <span className="pill-text">{selection.enabled ? `${strict ? "Only docs" : modeLabel[selection.mode]} · ${count}` : "Knowledge"}</span>
        <ChevronDownIcon size={13} />
      </>}>
      <div className="knowledge-panel">
        <Switch label="Use my documents" checked={selection.enabled}
          description="Selected documents become available to Chat. You can inspect every passage supplied to the model."
          onChange={(enabled) => onChange({ ...selection, enabled })} />
        {selection.enabled && (
          <>
            <div className="knowledge-mode">
              <span className="knowledge-label" aria-hidden="true">Ranking</span>
              <Segmented label="Search mode" value={selection.mode} onChange={(mode) => onChange({ ...selection, mode })}
                options={retrievalModes.map((mode) => ({ value: mode, label: modeLabel[mode] }))} />
              <p className="hint">{modeHint[selection.mode]}.</p>
            </div>
            <div className="knowledge-mode">
              <span className="knowledge-label" aria-hidden="true">Document context</span>
              <Segmented<Grounding> label="Document context" value={selection.grounding === "documents" ? "documents" : "auto"}
                onChange={(grounding) => onChange({ ...selection, grounding })}
                options={[{ value: "auto", label: "Automatic" }, { value: "documents", label: "Documents only" }]} />
              <p className="hint">{groundingHint[selection.grounding === "documents" ? "documents" : "auto"]}</p>
            </div>
            <div className="knowledge-docs">
              <div className="knowledge-docs-heading">
                <strong>Documents</strong>
                <span className="hint">{count} selected</span>
                <span className="spacer" />
                <button type="button" className="link-button" disabled={!documents.length || count === Math.min(documents.length, maxSelectedDocuments)}
                  onClick={() => onChange({ ...selection, documentIds: documents.slice(0, maxSelectedDocuments).map(({ id }) => id) })}>All</button>
                <button type="button" className="link-button" disabled={!count}
                  onClick={() => onChange({ ...selection, documentIds: [] })}>None</button>
              </div>
              {documents.length > 6 && (
                <div className="search-field compact">
                  <SearchIcon size={13} />
                  <label className="sr-only" htmlFor="knowledge-filter">Filter documents</label>
                  <input id="knowledge-filter" type="search" value={filter} placeholder="Filter…"
                    onChange={(event) => setFilter(event.currentTarget.value)} />
                </div>
              )}
              {!loaded && <p className="hint">Loading documents…</p>}
              {loaded && documents.length === 0 && <p className="hint">Your library is empty. Import documents first.</p>}
              {visible.length > 0 && (
                <ul className="knowledge-doc-list">
                  {visible.map((document) => {
                    const state = indexStates[document.id] ?? null;
                    const id = `knowledge-doc-${document.id}`;
                    return (
                      <li key={document.id}>
                        <input id={id} type="checkbox" checked={selected.has(document.id)} onChange={() => toggleDocument(document.id)} />
                        <label htmlFor={id} title={document.name}>{document.name}</label>
                        {dense && state !== "indexed" && (
                          <span className="badge badge-warning">{state === "incompatible" ? "Outdated index" : "Not indexed"}</span>
                        )}
                        {document.indexingStatus === "partial" && <span className="badge badge-muted">Partial</span>}
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
            {readiness.blocked && (
              <p className="knowledge-blocked" role="status"><strong>{readiness.blocked.title}.</strong> {readiness.blocked.detail}</p>
            )}
            {readiness.notes.map((note) => <p key={note} className="hint">{note}</p>)}
          </>
        )}
        <div className="knowledge-footer">
          <button type="button" className="link-button" onClick={onOpenKnowledge}>{readiness.blocked?.fix === "index" ? "Manage semantic indexes…" : "Open library…"}</button>
          <span className="hint">
            Memory {memoryEnabled === null ? "…" : memoryEnabled ? "on" : "off"} ·{" "}
            <button type="button" className="link-button" onClick={onOpenMemory}>Manage</button>
          </span>
        </div>
      </div>
    </Popover>
  );
}
