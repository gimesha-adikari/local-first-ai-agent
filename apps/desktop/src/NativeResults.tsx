import { useState } from "react";
import {
  formatBytes, formatNumber, humanize, pdfCoverage, pdfGuidance, pdfWarnings, resultView, searchFilterLabel, toolLabel,
  type PdfView, type ResultView,
} from "./agent";
import type { ApprovalTarget, NativeResult } from "./agentTypes";
import {
  AlertIcon, CalculatorIcon, CpuIcon, FileIcon, GridIcon, LaunchIcon, SearchIcon, ShieldIcon,
} from "./icons";
import { CopyButton } from "./MarkdownMessage";

const toolIcons: Record<string, typeof FileIcon> = {
  "math.calculate": CalculatorIcon,
  "device.info": CpuIcon,
  "files.search": SearchIcon,
  "files.read": FileIcon,
  "apps.list": GridIcon,
  "apps.launch": LaunchIcon,
};

const collapsedCount = 12;

function Truncated({ children }: { children: string }) {
  return <p className="result-truncated"><AlertIcon size={14} />{children}</p>;
}

function FileSearch({ view }: { view: Extract<ResultView, { kind: "file-search" }> }) {
  const [expanded, setExpanded] = useState(false);
  const shown = expanded ? view.paths : view.paths.slice(0, collapsedCount);
  return (
    <>
      <div className="result-meta result-meta-row">
        <span>
          {view.paths.length === 0 ? "No matching files were returned."
            : `${view.paths.length} ${view.paths.length === 1 ? "file" : "files"} returned`}
        </span>
        <span className="result-filter">{searchFilterLabel(view.extension)}</span>
      </div>
      {shown.length > 0 && (
        <ul className="result-files">
          {shown.map((path, index) => (
            <li key={`${index}-${path}`}><FileIcon size={14} /><span title={path}>{path}</span></li>
          ))}
        </ul>
      )}
      {view.paths.length > collapsedCount && (
        <button type="button" className="ghost-button small" aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}>
          {expanded ? "Show fewer" : `Show all ${view.paths.length}`}
        </button>
      )}
      {view.truncated && (
        <Truncated>
          The list may be incomplete. The native search reached a limit or skipped entries it could not resolve safely.
        </Truncated>
      )}
    </>
  );
}

function FileRead({ view }: { view: Extract<ResultView, { kind: "file-read" }> }) {
  return (
    <>
      <div className="result-meta result-meta-row">
        <span>{view.fileBytes === null ? "File size unknown" : `File size ${formatBytes(view.fileBytes)}`}</span>
        {view.content && <CopyButton text={view.content} label="Copy preview" showLabel />}
      </div>
      {view.content
        ? <pre className="result-preview" tabIndex={0}>{view.content}</pre>
        : <p className="result-meta">The file is empty.</p>}
      {view.truncated && <Truncated>Preview only. The native tool returned the beginning of the file, not its full content.</Truncated>}
    </>
  );
}

function PdfPages({ pdf }: { pdf: PdfView }) {
  const warnings = pdfWarnings(pdf);
  const all = pdf.pages.map((page) => `Page ${page.pageNumber}\n${page.text}`).join("\n\n");
  return (
    <>
      <div className="result-meta result-meta-row">
        <span>
          <strong className="pdf-coverage">{pdfCoverage(pdf)}</strong>
          {" · "}{pdf.pages.length} {pdf.pages.length === 1 ? "page" : "pages"} extracted
          {pdf.fileBytes !== null && <> · {formatBytes(pdf.fileBytes)}</>}
        </span>
        {all.trim() && <CopyButton text={all} label="Copy extracted text" showLabel />}
      </div>
      {pdf.pages.map((page) => (
        <section key={page.pageNumber} className="pdf-page" aria-label={`Page ${page.pageNumber}`}>
          <h4 className="pdf-page-number">Page {page.pageNumber}{page.truncated && <span> · clipped</span>}</h4>
          {/* Document text is untrusted and stays inert: plain text, never Markdown or HTML. */}
          {page.text.trim()
            ? <pre className="result-preview" tabIndex={0}>{page.text}</pre>
            : <p className="result-meta">No text on this page.</p>}
        </section>
      ))}
      {warnings.map((warning) => <Truncated key={warning.kind}>{warning.text}</Truncated>)}
      <p className="result-meta">Extracted locally from the PDF text layer. This is the document's own text, not a model summary.</p>
    </>
  );
}

function Apps({ view }: { view: Extract<ResultView, { kind: "apps" }> }) {
  const [expanded, setExpanded] = useState(false);
  const shown = expanded ? view.applications : view.applications.slice(0, 18);
  return (
    <>
      <p className="result-meta">
        {view.applications.length} {view.applications.length === 1 ? "application" : "applications"} found
      </p>
      <ul className="result-apps">
        {shown.map((app) => <li key={app.appId} title={app.displayName}>{app.displayName}</li>)}
      </ul>
      {view.applications.length > 18 && (
        <button type="button" className="ghost-button small" aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}>
          {expanded ? "Show fewer" : `Show all ${view.applications.length}`}
        </button>
      )}
      {view.truncated && <Truncated>The list may be incomplete. Discovery reached its native limit.</Truncated>}
    </>
  );
}

function ResultBody({ view }: { view: ResultView }) {
  switch (view.kind) {
    case "calculator":
      return <p className="result-value"><output>{formatNumber(view.value)}</output></p>;
    case "device":
      return (
        <dl className="result-grid">
          {view.rows.map((row) => <div key={row.label}><dt>{row.label}</dt><dd>{row.value}</dd></div>)}
        </dl>
      );
    case "file-search":
      return <FileSearch view={view} />;
    case "file-read":
      return <FileRead view={view} />;
    case "pdf":
      return <PdfPages pdf={view.pdf} />;
    case "apps":
      return <Apps view={view} />;
    case "launch":
      return (
        <div className="result-launch">
          <p>
            {view.dispatched ? "Launch request dispatched" : "Launch was not dispatched"}
            {view.target && <> for <strong>{view.target.displayName}</strong></>}.
          </p>
          <p className="result-meta">
            {view.dispatched
              ? "This confirms the system accepted the request, not that a window opened."
              : "The application was not started."}
          </p>
        </div>
      );
    case "failure": {
      const pdf = pdfGuidance[view.error.code];
      return (
        <>
          <p className="result-failure">{pdf?.title ?? (view.error.message || humanize(view.error.code))}</p>
          {pdf && <p className="result-meta">{pdf.detail}</p>}
        </>
      );
    }
    case "unrecognized":
      return (
        <>
          <p className="result-meta">This result has an unexpected shape and is shown as received.</p>
          <pre className="result-preview" tabIndex={0}>{view.json}</pre>
        </>
      );
  }
}

export function NativeResultCard({ result, targets }: { result: NativeResult; targets: ReadonlyMap<string, ApprovalTarget> }) {
  const view = resultView(result, targets);
  const Icon = toolIcons[result.observation.tool] ?? ShieldIcon;
  const failed = view.kind === "failure";
  return (
    <article className={`result-card${failed ? " result-card-failed" : ""}`}
      aria-label={`${toolLabel(result.observation.tool)} native result`}>
      <header className="result-heading">
        <span className="result-icon"><Icon size={15} /></span>
        <strong>{toolLabel(result.observation.tool)}</strong>
        <span className={`badge ${failed ? "badge-error" : "badge-verified"}`}>
          {failed ? "Native failure" : "Native result"}
        </span>
      </header>
      <div className="result-body"><ResultBody view={view} /></div>
    </article>
  );
}

export default function NativeResults({
  results, targets,
}: {
  results: NativeResult[];
  targets: Record<string, ApprovalTarget>;
}) {
  if (!results.length) return null;
  const map = new Map(Object.entries(targets));
  return (
    <section className="native-results" aria-label="Native results">
      <h3 className="section-label">
        <ShieldIcon size={14} />Native results
        <span>Returned by local tools, not written by the model</span>
      </h3>
      {results.map((result) => <NativeResultCard key={result.record.operationId} result={result} targets={map} />)}
    </section>
  );
}
