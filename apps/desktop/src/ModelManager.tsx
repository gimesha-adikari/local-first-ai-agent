import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { formatBytes } from "./agent";
import type { ModelSettings } from "./chat";
import { BookIcon, ChatIcon, CubeIcon, Spinner, ThinkIcon, UploadIcon } from "./icons";
import { errorMessage } from "./inference";
import { Callout, Segmented } from "./ui";

type Purpose = "everyday" | "advanced" | "knowledge";
type Status = "available" | "downloading" | "verifying" | "interrupted" | "failed" | "ready_to_finish"
  | "installed" | "external" | "missing" | "incompatible" | "removal_pending";
type Artifact = {
  key: string;
  name: string;
  fileName: string;
  purpose: Purpose;
  description: string;
  runtime: string;
  format: string;
  bytes: number;
  sha256: string;
  source: string;
  revision: string;
  license: string;
  licenseUrl: string;
  capabilities: string[];
  thinking: boolean;
  languages: string[];
  languageNote: string | null;
  trainedContext: number;
};
type Entry = {
  artifact: Artifact;
  status: Status;
  received: number;
  resumable: boolean;
  partial: boolean;
  managedFile: boolean;
  error: string | null;
  modelId: number | null;
  loaded: boolean;
  recommended: ModelSettings | null;
  supported: boolean;
};
type Catalog = { entries: Entry[]; localOnly: boolean; availableMemory: number | null; transferActive: boolean };
type View = "discover" | "installed" | "downloads";
const views: { value: View; label: string }[] = [
  { value: "discover", label: "Discover" }, { value: "installed", label: "Installed" }, { value: "downloads", label: "Downloads" },
];

const purposes = {
  everyday: { label: "Everyday intelligence", Icon: ChatIcon },
  advanced: { label: "Advanced intelligence", Icon: ThinkIcon },
  knowledge: { label: "Knowledge understanding", Icon: BookIcon },
};
const labels: Record<Status, string> = {
  available: "Available", downloading: "Downloading", verifying: "Verifying and installing", interrupted: "Interrupted",
  failed: "Download failed", ready_to_finish: "Ready to finish", installed: "Verified download", external: "Verified local copy",
  missing: "File missing or changed", incompatible: "Verification or compatibility failed", removal_pending: "Removal incomplete",
};
const installed = ({ status }: Entry) => status === "installed" || status === "external";
const active = ({ status }: Entry) => status === "downloading" || status === "verifying";
const inDownloads = (entry: Entry) => active(entry) || entry.partial
  || ["failed", "interrupted", "ready_to_finish"].includes(entry.status);
const languages = new Intl.DisplayNames(["en"], { type: "language" });

export function ModelManager({
  children, initialInstalled, localOnly, operationsBusy, onSelectModel, onImportChat, onImportEmbedding, onUnloadEmbedding,
  onEmbeddingChanged, embeddingNotice,
}: {
  children: ReactNode;
  initialInstalled: boolean;
  localOnly: boolean;
  operationsBusy: boolean;
  onSelectModel: (id: number) => void;
  onImportChat: () => void;
  onImportEmbedding: () => Promise<unknown>;
  onUnloadEmbedding: () => Promise<unknown>;
  onEmbeddingChanged: () => Promise<unknown>;
  embeddingNotice: { tone: "neutral" | "accent" | "error" | "warning"; title: string; detail?: string } | null;
}) {
  const [view, setView] = useState<View>(initialInstalled ? "installed" : "discover");
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [action, setAction] = useState<string | null>(null);
  const [remove, setRemove] = useState<{ entry: Entry; installed: boolean } | null>(null);

  const refresh = useCallback(async () => {
    setCatalog(await invoke<Catalog>("model_catalog"));
  }, []);

  useEffect(() => {
    let current = true;
    const read = () => { if (current) void refresh().catch((failure) => setError(errorMessage(failure))); };
    read();
    const listener = listen("model-library-changed", read);
    return () => {
      current = false;
      void listener.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [refresh]);

  useEffect(() => {
    if (!catalog?.transferActive) return;
    const timer = window.setTimeout(() => void refresh().catch((failure) => setError(errorMessage(failure))), 500);
    return () => window.clearTimeout(timer);
  }, [catalog, refresh]);

  async function run(key: string, work: () => Promise<unknown>) {
    setAction(key);
    setError(null);
    try {
      await work();
      await refresh();
    } catch (failure) {
      setError(errorMessage(failure));
      await refresh().catch(() => undefined);
    } finally {
      setAction(null);
    }
  }

  async function start(entry: Entry, restart: boolean) {
    await run(entry.artifact.key, async () => {
      if (await invoke<boolean>("start_model_download", { key: entry.artifact.key, restart })) setView("downloads");
    });
  }

  function showModel(entry: Entry) {
    setView("installed");
    if (entry.modelId !== null) onSelectModel(entry.modelId);
  }

  const entries = catalog?.entries ?? [];
  const downloads = entries.filter((entry) => inDownloads(entry) || entry.status === "installed");
  const blocked = localOnly || !!catalog?.localOnly;
  const networkBusy = action !== null || !!catalog?.transferActive;
  const installedEntries = entries.filter((entry) => entry.status === "removal_pending"
    || (entry.status === "incompatible" || entry.status === "missing") && entry.managedFile
    || installed(entry) && (entry.managedFile || entry.artifact.purpose === "knowledge"));

  return (
    <div className="model-manager">
      <div className="model-manager-toolbar">
        <Segmented label="Model library" options={views.map(({ value, label }) => ({
          value, label: value === "downloads" && catalog?.transferActive ? "Downloads •" : label,
        }))} value={view} onChange={setView} />
        <details className="model-import">
          <summary><UploadIcon size={14} />Manual import</summary>
          <div className="model-import-actions">
            <button type="button" className="secondary-button small" disabled={operationsBusy}
              onClick={() => { setView("installed"); onImportChat(); }}>Import Chat GGUF…</button>
            <button type="button" className="secondary-button small" disabled={operationsBusy || action !== null}
              onClick={() => { setView("installed"); void run("embedding", onImportEmbedding); }}>Import BGE GGUF…</button>
          </div>
        </details>
      </div>
      {error && <Callout tone="error" title="Model library" role="alert">{error}</Callout>}
      {view !== "installed" && blocked && <Callout title="Local-only mode is on">
        Turn it off in Providers to approve a download. You can use installed models or import files from this computer.
      </Callout>}
      {view !== "installed" && !catalog && !error && <p className="hint inline-status"><Spinner size={14} />Loading model catalog…</p>}

      {view === "discover" && catalog && (
        <div className="catalog-grid">
          {entries.map((entry) => {
            const { artifact } = entry;
            const { label, Icon } = purposes[artifact.purpose];
            const detailsId = `artifact-${artifact.key}`;
            return (
              <article className="catalog-card" key={artifact.key} aria-labelledby={detailsId}>
                <div className="catalog-purpose"><Icon size={16} />{label}</div>
                <div className="catalog-card-main">
                  <h4 id={detailsId}>{artifact.name}</h4>
                  <p>{artifact.description}</p>
                  <p className="catalog-meta">{formatBytes(artifact.bytes)} · {artifact.license}</p>
                  {artifact.license === "LFM Open License v1.0" && <p className="hint">Some commercial uses require a separate license.</p>}
                  <p className="hint">{entry.supported ? suitability(entry, catalog.availableMemory) : "This runtime is unavailable in this build."}</p>
                </div>
                <details className="catalog-technical">
                  <summary>Source & technical details</summary>
                  <dl>
                    <dt>Runtime</dt><dd>{artifact.runtime} · {artifact.format.toUpperCase()}</dd>
                    <dt>Capabilities</dt><dd>{[
                      artifact.capabilities.includes("core.generate/1") && "Chat",
                      artifact.thinking && "Thinking",
                      artifact.capabilities.includes("core.agent-decision/1") && "Agent tasks",
                      artifact.capabilities.includes("core.embed/1") && "Semantic search",
                    ].filter(Boolean).join(", ")}</dd>
                    <dt>Languages</dt><dd>{artifact.languages.map((language) => languages.of(language)).join(", ") || artifact.languageNote || "See model card"}</dd>
                    <dt>Source</dt><dd>{artifact.source}</dd>
                    <dt>File</dt><dd>{artifact.fileName} · {artifact.bytes.toLocaleString()} bytes</dd>
                    <dt>Revision</dt><dd>{artifact.revision}</dd>
                    <dt>SHA-256</dt><dd>{artifact.sha256}</dd>
                    <dt>License terms</dt><dd>{artifact.licenseUrl}</dd>
                  </dl>
                </details>
                <div className="catalog-card-footer">
                  <span className="hint">{entry.loaded ? "Loaded" : labels[entry.status]}</span>
                  {installed(entry) || entry.managedFile && !inDownloads(entry)
                    ? <button type="button" className="secondary-button small" onClick={() => showModel(entry)}>View installed</button>
                    : inDownloads(entry)
                      ? <button type="button" className="secondary-button small" onClick={() => setView("downloads")}>View download</button>
                      : <button type="button" className="primary-button small" disabled={blocked || networkBusy || !entry.supported
                        || entry.status === "incompatible"} onClick={() => void start(entry, false)}
                        aria-label={`Download ${artifact.name}`}>Download…</button>}
                </div>
              </article>
            );
          })}
        </div>
      )}

      {view === "downloads" && catalog && (downloads.length === 0
        ? <div className="library-empty"><CubeIcon size={22} /><p>No downloads in progress. Choose a model in Discover.</p></div>
        : <ul className="catalog-rows" aria-label="Current and interrupted downloads">
          {downloads.map((entry) => (
            <li key={entry.artifact.key} className="catalog-row">
              <div className="catalog-row-title"><strong>{entry.artifact.name}</strong><span>{labels[entry.status]}</span></div>
              {(active(entry) || entry.received > 0) && <>
                <progress max={entry.artifact.bytes} value={entry.received} aria-label={`Download progress for ${entry.artifact.name}`} />
                <p className="hint">{formatBytes(entry.received)} of {formatBytes(entry.artifact.bytes)}</p>
              </>}
              {entry.error && <p className="hint" role="status">{entry.error}</p>}
              {!active(entry) && entry.partial && entry.status !== "ready_to_finish" && entry.status !== "incompatible"
                && <p className="hint">{entry.resumable ? "Resume continues only if the server confirms the same file. Otherwise the app starts at zero."
                  : "Retry starts at zero because the partial file cannot be safely resumed."}</p>}
              <div className="catalog-row-actions">
                {active(entry) && <button className="secondary-button small" type="button" disabled={action !== null}
                  onClick={() => void run(entry.artifact.key, () => invoke("cancel_model_download", { key: entry.artifact.key }))}>Cancel</button>}
                {entry.status === "ready_to_finish" && <button className="primary-button small" type="button" disabled={networkBusy || operationsBusy}
                  onClick={() => void run(entry.artifact.key, () => invoke("finish_model_installation", { key: entry.artifact.key }))}>Finish installation</button>}
                {!active(entry) && !entry.managedFile && entry.status !== "ready_to_finish" && <>
                  <button className="primary-button small" type="button" disabled={networkBusy || blocked || !entry.supported}
                    onClick={() => void start(entry, !entry.resumable && entry.partial)}>{entry.resumable ? "Resume…" : "Retry…"}</button>
                  {entry.resumable && <button className="secondary-button small" type="button" disabled={networkBusy || blocked || !entry.supported}
                    onClick={() => void start(entry, true)}>Restart…</button>}
                </>}
                {!active(entry) && entry.partial && !entry.managedFile && <button className="ghost-button small" type="button" disabled={networkBusy}
                  onClick={() => setRemove({ entry, installed: false })}>Discard partial…</button>}
                {entry.status === "installed" && <button className="secondary-button small" type="button"
                  onClick={() => showModel(entry)}>View installed</button>}
                {!active(entry) && entry.managedFile && entry.status !== "installed" && <button className="ghost-button small" type="button" disabled={networkBusy || operationsBusy}
                  onClick={() => setRemove({ entry, installed: true })}>Remove file…</button>}
              </div>
            </li>
          ))}
        </ul>)}

      {view === "installed" && <>
        {embeddingNotice && <Callout tone={embeddingNotice.tone} title={embeddingNotice.title} role="status">
          {embeddingNotice.detail}
        </Callout>}
        {children}
        {installedEntries.length > 0 && <ul className="catalog-rows" aria-label="Verified model files">
          {installedEntries.map((entry) => (
            <li className="catalog-row" key={entry.artifact.key}>
              <div className="catalog-row-title"><strong>{entry.artifact.name}</strong><span>{entry.loaded ? "Loaded" : labels[entry.status]}</span></div>
              {entry.artifact.purpose === "knowledge" && <p className="hint">{entry.artifact.description}</p>}
              <div className="catalog-row-actions">
                {entry.artifact.purpose === "knowledge" && installed(entry) && (entry.loaded
                  ? <button type="button" className="secondary-button small" disabled={operationsBusy || networkBusy}
                    onClick={() => void run(entry.artifact.key, onUnloadEmbedding)}>Unload from Knowledge</button>
                  : entry.managedFile && <button type="button" className="secondary-button small" disabled={operationsBusy || networkBusy}
                    onClick={() => void run(entry.artifact.key, async () => {
                      await invoke("use_downloaded_embedding", { key: entry.artifact.key });
                      await onEmbeddingChanged();
                    })}>Use for Knowledge</button>)}
                {entry.managedFile && <button className="ghost-button small" type="button" disabled={networkBusy || operationsBusy || entry.loaded}
                  onClick={() => setRemove({ entry, installed: true })}>Remove managed file…</button>}
              </div>
              {entry.loaded && entry.managedFile && <p className="hint">Unload this model before removing its file.</p>}
            </li>
          ))}
        </ul>}
      </>}

      {remove && <Callout tone="warning" title={remove.installed ? "Remove managed model?" : "Discard the partial download?"}
        actions={<>
          <button type="button" className="danger-button small" disabled={networkBusy} onClick={() => {
            const { entry, installed } = remove;
            setRemove(null);
            void run(entry.artifact.key, async () => {
              await invoke("remove_model_download", { key: entry.artifact.key, installed });
              if (installed) await onEmbeddingChanged();
            });
          }}>{remove.installed ? "Remove file" : "Discard partial"}</button>
          <button type="button" className="secondary-button small" onClick={() => setRemove(null)}>Keep file</button>
        </>}>
        {remove.entry.artifact.name}. {remove.installed
          ? "You can download it again later. Existing settings and Knowledge indexes are kept."
          : "A later download will start at zero."}
      </Callout>}
    </div>
  );
}

function suitability(entry: Entry, available: number | null): string {
  if (available !== null && available <= entry.artifact.bytes) {
    return `Memory is tight. ${formatBytes(available)} is available now and inference also needs space for context and working memory.`;
  }
  if (!entry.recommended) return "CPU embedding model. Semantic indexing uses additional working memory.";
  return `Estimated starting point on this computer · ${entry.recommended.contextLength.toLocaleString()} tokens of context, ${entry.recommended.threads} CPU threads. Longer context needs more memory.`;
}
