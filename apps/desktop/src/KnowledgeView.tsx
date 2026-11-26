import { EmbeddingModelCard } from "./Settings";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { commandError } from "./chat";
import {
  CloseIcon, CpuIcon, FileIcon, MoreIcon, NoteIcon, RefreshIcon, SearchIcon, Spinner, TrashIcon, UploadIcon,
} from "./icons";
import {
  createLatest, documentSummary, formatBytes, formatLabel, importCancellable, importInDialog, importPhaseLabel,
  indexPercent, indexStateLabel, knowledgeErrorCopy, knowledgeQueryValid, modeHint, modeLabel, retrievalModes,
  sourceLabel, textCoverageNote,
  type KnowledgeDocument, type RetrievalMode, type RetrievalResults,
} from "./knowledge";
import { Callout, Dialog, Menu, MenuItem, MenuSeparator, Segmented } from "./ui";
import type { KnowledgeController } from "./useKnowledge";

type Scope = "names" | RetrievalMode;

const dateLabel = (at: number) => new Date(at).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });

function DocumentRow({
  document, knowledge, lockReason, expanded, onToggle, onDelete, menuAbove = false,
}: {
  /** Open the actions menu upward so it isn't clipped near the bottom of the list. */
  menuAbove?: boolean;
  document: KnowledgeDocument;
  knowledge: KnowledgeController;
  lockReason: string | null;
  expanded: boolean;
  onToggle: () => void;
  onDelete: () => void;
}) {
  const state = knowledge.indexStates[document.id] ?? null;
  const job = knowledge.indexJob;
  const indexingThis = knowledge.indexing && job?.documentId === document.id;
  const percent = indexingThis && job ? indexPercent(job) : null;
  const coverageNote = textCoverageNote(document);
  const modelReady = !!knowledge.embedding?.available;
  const canIndex = modelReady && !knowledge.indexing && !lockReason && knowledge.modelAction === null;
  const indexAction = state === "indexed" ? "Rebuild semantic index" : "Build semantic index";
  const indexHint = !knowledge.embedding?.supported ? "Not available on this system"
    : !modelReady ? "Choose the semantic search model first"
      : lockReason ?? (knowledge.indexing ? "Another document is being indexed" : undefined);
  const detailsId = `document-${document.id}-details`;

  return (
    <li className={`document-row${expanded ? " document-row-open" : ""}`}>
      <div className="document-main">
        <button type="button" className="document-toggle" aria-expanded={expanded} aria-controls={detailsId} onClick={onToggle}>
          <span className={`document-icon document-icon-${document.format}`} aria-hidden="true">
            {document.format === "pdf" ? <FileIcon size={17} /> : <NoteIcon size={17} />}
          </span>
          <span className="document-text">
            <span className="document-name" title={document.name}>{document.name}</span>
            <small>{documentSummary(document)}</small>
          </span>
        </button>
        <span className="document-badges">
          {coverageNote && <span className="badge badge-warning" title={coverageNote}>Partly readable</span>}
          {indexingThis ? (
            <span className="badge badge-progress"><Spinner size={11} />{percent === null ? "Indexing…" : `Indexing ${percent}%`}</span>
          ) : state === "indexed" ? <span className="badge badge-ready">Semantic ready</span>
            : state === "incompatible" ? <span className="badge badge-warning" title={indexStateLabel.incompatible}>Index outdated</span>
              : state === "missing" ? <span className="badge badge-muted">Keyword only</span> : null}
        </span>
        <Menu label={`Actions for ${document.name}`} align="end" placement={menuAbove ? "above" : "below"}
          trigger={<MoreIcon size={17} />} className="topbar-menu">
          {(close) => (
            <>
              <MenuItem icon={<CpuIcon size={15} />} disabled={!canIndex} hint={indexHint}
                onSelect={() => { close(); void knowledge.reindex(document.id); }}>{indexAction}</MenuItem>
              <MenuItem icon={<RefreshIcon size={15} />} disabled={knowledge.importing} hint="Choose an updated copy of the file"
                onSelect={() => { close(); void knowledge.startImport(document.id); }}>Replace file…</MenuItem>
              <MenuSeparator />
              <MenuItem danger icon={<TrashIcon size={15} />} disabled={indexingThis}
                onSelect={() => { close(); onDelete(); }}>Delete…</MenuItem>
            </>
          )}
        </Menu>
      </div>
      {indexingThis && job && (
        <div className="document-progress">
          <div className="progress" role="progressbar" aria-label={`Indexing ${document.name}`}
            aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent ?? undefined}>
            <span style={{ width: `${percent ?? 4}%` }} />
          </div>
          <span className="hint">
            {job.phase === "cancelling" ? "Cancelling…" : job.phase === "committing" ? "Saving…"
              : job.totalChunks ? `${job.processedChunks.toLocaleString()} of ${job.totalChunks.toLocaleString()} passages` : "Starting…"}
          </span>
          <button type="button" className="link-button" disabled={job.phase !== "indexing"}
            onClick={() => void knowledge.cancelIndex()}>Cancel</button>
        </div>
      )}
      {expanded && (
        <div id={detailsId} className="document-details">
          <dl className="facts">
            <div><dt>Type</dt><dd>{formatLabel[document.format]}</dd></div>
            <div><dt>File size</dt><dd>{formatBytes(document.fileBytes)}</dd></div>
            <div>
              <dt>Saved text</dt>
              <dd>{formatBytes(document.textBytes)} in {document.chunkCount.toLocaleString()} {document.chunkCount === 1 ? "passage" : "passages"}</dd>
            </div>
            {document.pdfCoverage && (
              <div>
                <dt>Pages</dt>
                <dd>
                  {document.pdfCoverage.pageCount.toLocaleString()} total · {document.pdfCoverage.pagesExamined.toLocaleString()} read
                  {document.pdfCoverage.pagesWithoutText ? ` · ${document.pdfCoverage.pagesWithoutText.toLocaleString()} without text` : ""}
                </dd>
              </div>
            )}
            <div><dt>Keyword search</dt><dd>{coverageNote ? `Partial. ${coverageNote}` : "All text is searchable"}</dd></div>
            <div>
              <dt>Semantic search</dt>
              <dd>
                {state === "indexed" ? "Index saved for the current version"
                  : state === "incompatible" ? "The saved index was built differently. Rebuild it to use semantic search."
                    : state === "missing" ? "Not indexed yet" : "Unknown"}
              </dd>
            </div>
            <div><dt>Added</dt><dd>{dateLabel(document.createdAt)}</dd></div>
            {document.updatedAt !== document.createdAt && <div><dt>Updated</dt><dd>{dateLabel(document.updatedAt)}</dd></div>}
          </dl>
          <div className="drawer-actions">
            <button type="button" className="secondary-button small" disabled={!canIndex} title={indexHint}
              onClick={() => void knowledge.reindex(document.id)}>{indexAction}</button>
            <button type="button" className="secondary-button small" disabled={knowledge.importing}
              onClick={() => void knowledge.startImport(document.id)}>Replace file…</button>
            <button type="button" className="ghost-button small danger-text" disabled={indexingThis} onClick={onDelete}>Delete…</button>
          </div>
        </div>
      )}
    </li>
  );
}

function EmbeddingPanel({ knowledge, lockReason }: { knowledge: KnowledgeController; lockReason: string | null }) {
  return (
    <section className="embedding-panel" aria-label="Semantic search model">
      <EmbeddingModelCard knowledge={knowledge} lockReason={lockReason} />
    </section>
  );
}

export default function KnowledgeView({
  knowledge, lockReason,
}: {
  knowledge: KnowledgeController;
  /** Why CPU-bound actions (indexing, model selection, semantic search) are unavailable right now. */
  lockReason: string | null;
}) {
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState<Scope>("names");
  const [snapshot, setResults] = useState<{ key: string; results: RetrievalResults } | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<KnowledgeDocument | null>(null);
  const [deleting, setDeleting] = useState(false);
  const gate = useRef(createLatest()).current;
  const { documents, importJob, importing } = knowledge;

  const trimmed = query.trim();
  const filtered = scope === "names" && trimmed
    ? documents.filter((document) => document.name.toLocaleLowerCase().includes(trimmed.toLocaleLowerCase()))
    : documents;

  const searchKey = JSON.stringify([scope, trimmed, knowledge.revision]);
  const results = snapshot?.key === searchKey ? snapshot.results : null;
  const denseBlocked = scope === "semantic" || scope === "hybrid"
    ? lockReason ?? (!knowledge.embedding?.available ? "Choose the semantic model to search by meaning." : null) : null;

  // Contents search: keyword runs as you type; semantic and hybrid wait for a pause.
  useEffect(() => {
    if (scope === "names" || !trimmed) {
      gate.invalidate();
      setResults(null);
      setSearchError(null);
      setSearching(false);
      return;
    }
    if (!knowledgeQueryValid(trimmed)) {
      gate.invalidate();
      setResults(null);
      setSearching(false);
      setSearchError("Use up to 256 bytes with at least one letter or number.");
      return;
    }
    if (denseBlocked) {
      gate.invalidate();
      setResults(null);
      setSearching(false);
      setSearchError(denseBlocked);
      return;
    }
    const token = gate.next();
    const timer = window.setTimeout(() => {
      setSearching(true);
      setSearchError(null);
      invoke<RetrievalResults>("knowledge_retrieve", { mode: scope, request: { query: trimmed, documentIds: null, limit: 10 } })
        .then((next) => { if (gate.isCurrent(token)) setResults({ key: searchKey, results: next }); })
        .catch((failure: unknown) => {
          if (!gate.isCurrent(token)) return;
          setResults(null);
          const copy = knowledgeErrorCopy(commandError(failure));
          setSearchError(`${copy.title}. ${copy.detail}`);
        })
        .finally(() => { if (gate.isCurrent(token)) setSearching(false); });
    }, scope === "keyword" ? 200 : 450);
    return () => { gate.invalidate(); window.clearTimeout(timer); };
  }, [scope, trimmed, searchKey, denseBlocked, gate]);

  async function confirmDeletion() {
    if (!confirmDelete) return;
    setDeleting(true);
    const ok = await knowledge.deleteDocument(confirmDelete.id);
    setDeleting(false);
    if (ok) {
      if (expanded === confirmDelete.id) setExpanded(null);
      setConfirmDelete(null);
    }
  }

  const job = importJob;
  const coverage = results?.coverage;
  const unindexed = coverage ? coverage.missingDocuments + coverage.incompatibleDocuments : 0;

  return (
    <div className="library-view">
      <header className="library-header">
        <div>
          <h2>Knowledge library</h2>
          <p>Your documents, stored on this device. Chat searches only the ones you select.</p>
        </div>
        <button type="button" className="primary-button" disabled={importing} onClick={() => void knowledge.startImport()}>
          {importing ? <Spinner size={14} /> : <UploadIcon size={15} />}Import document
        </button>
      </header>

      {job && importing && (
        <Callout tone="accent" role="status" title={importPhaseLabel(job.state)}
          actions={importCancellable(job.state) ? (
            <button type="button" className="link-button" onClick={() => void knowledge.cancelImport()}>Cancel import</button>
          ) : undefined}>
          {importInDialog(job.state)
            ? "Close the dialog to cancel. Text, Markdown and PDF files are supported."
            : "The app keeps a searchable copy of the text. The original file isn't changed or watched."}
        </Callout>
      )}
      {knowledge.notice && (
        <Callout tone={knowledge.notice.tone} role={knowledge.notice.tone === "error" ? "alert" : "status"} title={knowledge.notice.title}
          actions={<button type="button" className="link-button" onClick={() => knowledge.setNotice(null)}>Dismiss</button>}>
          {knowledge.notice.detail}
        </Callout>
      )}

      <EmbeddingPanel knowledge={knowledge} lockReason={lockReason} />

      <section className="library-search" aria-label="Search">
        <div className="search-field">
          <SearchIcon size={15} />
          <label className="sr-only" htmlFor="library-search">Search documents</label>
          <input id="library-search" type="search" maxLength={256} value={query}
            placeholder={scope === "names" ? "Search document names…" : "Search inside your documents…"}
            onChange={(event) => setQuery(event.currentTarget.value)} />
          {query && (
            <button type="button" className="chip-remove" aria-label="Clear search" onClick={() => setQuery("")}><CloseIcon size={12} /></button>
          )}
        </div>
        <div className="search-scope">
          <Segmented label="Search in" value={scope} onChange={setScope}
            options={[{ value: "names", label: "Names" }, ...retrievalModes.map((mode) => ({ value: mode, label: modeLabel[mode] }))]} />
          <span className="hint">{scope === "names" ? "Filter the list by file name" : `Contents · ${modeHint[scope]}`}</span>
        </div>
      </section>

      {scope !== "names" && trimmed && (
        <section className="search-results" aria-label="Search results" aria-busy={searching}>
          {searching && <p className="hint inline-status" role="status"><Spinner size={12} />Searching…</p>}
          {searchError && <p className="error-text" role="alert">{searchError}</p>}
          {results && !searching && (
            <>
              {unindexed > 0 && (
                <p className="hint">
                  {unindexed} {unindexed === 1 ? "document has" : "documents have"} no usable semantic index
                  {results.mode === "hybrid" ? " and matched by words only." : " and weren't searched."}
                </p>
              )}
              {results.hits.length === 0 ? <p className="empty-line">No passages match.</p> : (
                <ol className="hit-list">
                  {results.hits.map((hit) => (
                    <li key={`${hit.documentId}-${hit.ordinal}`} className="hit">
                      <div className="hit-heading">
                        <strong title={hit.documentName}>{hit.documentName}</strong>
                        <span className="hint">{sourceLabel(hit.source)}</span>
                      </div>
                      {/* Untrusted retained text, shown verbatim. */}
                      <p className="hit-text">{hit.text}</p>
                    </li>
                  ))}
                </ol>
              )}
            </>
          )}
        </section>
      )}

      <section aria-label="Documents" className="library-documents">
        {knowledge.listError && (
          <Callout tone="error" role="alert" title="The library couldn't be loaded"
            actions={<button type="button" className="link-button" onClick={() => void knowledge.refresh()}>Try again</button>}>
            {knowledge.listError}
          </Callout>
        )}
        {!knowledge.loaded && !knowledge.listError && <p className="hint inline-status" role="status"><Spinner size={12} />Loading…</p>}
        {knowledge.loaded && documents.length === 0 && (
          <div className="library-empty">
            <UploadIcon size={22} />
            <p>No documents yet.</p>
            <p className="hint">Import Text, Markdown or PDF files to ask questions about them in Chat. Up to 128 documents.</p>
          </div>
        )}
        {knowledge.loaded && documents.length > 0 && filtered.length === 0 && (
          <p className="empty-line">No document names match.</p>
        )}
        {filtered.length > 0 && (
          <ul className="document-list">
            {filtered.map((document, index) => (
              <DocumentRow key={document.id} document={document} knowledge={knowledge} lockReason={lockReason}
                menuAbove={filtered.length > 2 && index >= filtered.length - 2}
                expanded={expanded === document.id}
                onToggle={() => setExpanded((current) => (current === document.id ? null : document.id))}
                onDelete={() => setConfirmDelete(document)} />
            ))}
          </ul>
        )}
        {knowledge.loaded && documents.length > 0 && (
          <p className="hint library-footnote">
            Deleting or replacing a document stops it from being searched or cited. Earlier answers may still quote it,
            and the original file stays on your computer. The library is stored unencrypted on this device.
          </p>
        )}
      </section>

      {confirmDelete && (
        <Dialog size="sm" title={`Delete “${confirmDelete.name}”?`} busy={deleting} onClose={() => setConfirmDelete(null)}
          description="Its saved text and semantic index are removed from this device.">
          <div className="dialog-form">
            <ul className="privacy-list">
              <li>Chat will no longer search or cite it, and its source links become unavailable.</li>
              <li>Earlier answers may still contain quotations from it.</li>
              <li>The original file on your computer isn't touched.</li>
            </ul>
            <div className="dialog-actions">
              <button type="button" className="secondary-button" disabled={deleting} onClick={() => setConfirmDelete(null)}>Cancel</button>
              <button type="button" className="danger-button" disabled={deleting} onClick={() => void confirmDeletion()}>
                {deleting ? <Spinner size={13} /> : <TrashIcon size={14} />}Delete
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </div>
  );
}
