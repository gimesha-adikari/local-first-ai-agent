import { Channel, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { AlertIcon, ArrowUpIcon, CameraIcon, CheckIcon, ChevronDownIcon, CloseIcon, CloudIcon, CopyIcon, CpuIcon, CropIcon, ExternalIcon, FileIcon,
  GlobeIcon, ImageIcon, LanguagesIcon, LaunchIcon, MemoryIcon, QuoteIcon, ScanTextIcon, SearchIcon, ShieldIcon, SlidersIcon, Spinner, StopIcon, ThinkIcon, TrashIcon } from "./icons";
import { CameraPicker, CameraStatus, OcrLanguages, SnapshotReview } from "./VisualInput";
import {
  cameraSummary, queryProblem, searchDisclosure, searchUnavailable, suggestQuery, visualClassNames,
  type CameraNode, type PendingSnapshot, type Region, type Taken, type VisualSearch,
} from "./visual";
import { errorMessage } from "./inference";
import { classNames, inputProblem, prepareGlance, recipes, type CropInfo, type Estimate, type GlanceReceipt, type GlanceInput,
  languageName, type GlanceProvider, type GlanceResult, type GlanceState, type OcrRecord, type Recipe, type ScreenReceipt } from "./glance";
import { IconButton } from "./ui";
import "./glance.css";

type Review = { input: GlanceInput; provider: GlanceProvider; documents: string[]; estimate: Estimate };

function cropUrl(crop: CropInfo) {
  return `glance-crop://localhost/${crop.id}.png`;
}

function costText(estimate: Estimate) {
  const tokens = `about ${estimate.inputTokens.toLocaleString()} input tokens (including ${estimate.imageTokens.toLocaleString()} for the image) and at most ${estimate.maxOutputTokens.toLocaleString()} output tokens`;
  if (estimate.costMicros === null) return `${tokens}. No pricing is configured for this provider.`;
  return `${tokens}, at most ${(estimate.costMicros / 1_000_000).toLocaleString(undefined, { maximumFractionDigits: 4 })} ${estimate.currency ?? ""}.`;
}

function providerLocation(provider: GlanceProvider) {
  return ["self_hosted", "self-hosted"].includes(provider.locality) ? "Self-hosted" : "Cloud";
}

function retentionText(value: string) {
  if (value === "none") return "Provider declares no retention.";
  if (value === "provider_policy" || value === "provider_declared") return "The provider’s retention policy applies.";
  if (value === "unknown" || !value) return "Provider retention is unknown.";
  return value;
}

function ScreenDetails({ screen }: { screen: ScreenReceipt }) {
  const { capture, camera, region, ocr } = screen;
  if (!capture && camera) return <>
    <dt>Camera</dt><dd>{`${camera.source}${camera.portalVersion ? ` v${camera.portalVersion}` : ""}: ${cameraSummary(camera)}`}</dd>
    <dt>Authorization</dt><dd>{`${camera.authorization}; desktop permission answered in ${camera.consentMs} ms`}</dd>
    <dt>Area</dt><dd>{`${region.region.width} × ${region.region.height} at ${region.region.x}, ${region.region.y} of ${region.frameWidth} × ${region.frameHeight}`}</dd>
    <dt>Full snapshot</dt><dd>{screen.fullFrame}</dd>
    <dt>Recognition</dt><dd>{ocr ? `${ocr.engine} (${ocr.languages.map(languageName).join(", ")}), ${ocr.characters} characters in ${ocr.elapsedMs} ms, on this device` : "Not used"}</dd>
    <dt>Image</dt><dd>{screen.imageAttached ? "The chosen area of the snapshot was attached to this request" : "Not sent"}</dd>
  </>;
  if (!capture) return null;
  return <>
    <dt>Capture</dt><dd>{`${capture.source}${capture.portalVersion ? ` v${capture.portalVersion}` : ""}, fresh consent (persist mode ${capture.persistMode}, restore token ${capture.restoreTokenReturned ? "returned and discarded" : "none"}), cursor ${capture.cursor}`}</dd>
    <dt>Frame</dt><dd>{`${capture.stream.width} × ${capture.stream.height} ${capture.stream.format}, ${capture.stream.frames} frame(s), ${capture.stream.settled}, screen sharing ended after ${capture.sessionClosedMs} ms${capture.sessionCloseConfirmed ? " (confirmed)" : " (not confirmed by the desktop)"}`}</dd>
    <dt>Region</dt><dd>{`${region.region.width} × ${region.region.height} at ${region.region.x}, ${region.region.y} of ${region.frameWidth} × ${region.frameHeight}`}</dd>
    <dt>Full frame</dt><dd>{screen.fullFrame}</dd>
    <dt>Recognition</dt><dd>{ocr ? `${ocr.engine} (${ocr.languages.map(languageName).join(", ")}), ${ocr.characters} characters in ${ocr.elapsedMs} ms, on this device` : "Not used"}</dd>
    <dt>Image</dt><dd>{screen.imageAttached ? "The selected region was attached to this request" : "Not sent"}</dd>
  </>;
}

function ReceiptDetails({ receipt, screen }: { receipt: GlanceReceipt; screen?: ScreenReceipt | null }) {
  const sourceKinds = (sources: GlanceReceipt["used"]) => [...new Set(sources.map(item => item.kind.replaceAll("_", " ")))].join(", ") || "None";
  return <details className="glance-receipt">
    <summary><ShieldIcon size={14} />Processing receipt <ChevronDownIcon size={13} /></summary>
    <dl>
      {screen && <ScreenDetails screen={screen} />}
      <dt>Status</dt><dd>{receipt.state}</dd>
      <dt>Processing</dt><dd>{receipt.provider ? `${receipt.provider.id} · ${receipt.provider.locality.replaceAll("_", " ")}` : "No provider recorded"}</dd>
      <dt>Sources used</dt><dd>{sourceKinds(receipt.used)}</dd>
      <dt>Sources withheld</dt><dd>{sourceKinds(receipt.withheld)}</dd>
      <dt>Transmission</dt><dd>{receipt.external.length
        ? receipt.external.map(item => `${item.origin} · ${item.transmission}`).join(", ") : "No external request recorded"}</dd>
      <dt>App retention</dt><dd>{receipt.application_retention.replaceAll("_", " ")}</dd>
    </dl>
    <details><summary>Full metadata <ChevronDownIcon size={12} /></summary><pre>{JSON.stringify(receipt, null, 2)}</pre></details>
  </details>;
}

export default function Glance() {
  const [state, setState] = useState<GlanceState | null>(null);
  const [text, setText] = useState("");
  const [source, setSource] = useState("Manual");
  const [recipe, setRecipe] = useState<Recipe>("explain");
  const [language, setLanguage] = useState("");
  const [question, setQuestion] = useState("");
  const [providerId, setProviderId] = useState<number | null>(null);
  const [memory, setMemory] = useState(false);
  const [documents, setDocuments] = useState<number[]>([]);
  const [review, setReview] = useState<Review | null>(null);
  const [running, setRunning] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [output, setOutput] = useState("");
  const [result, setResult] = useState<GlanceResult | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [remembered, setRemembered] = useState(false);
  const [crop, setCrop] = useState<CropInfo | null>(null);
  const [useImage, setUseImage] = useState(false);
  const [ocrLanguages, setOcrLanguages] = useState<string[]>([]);
  const [recognized, setRecognized] = useState<OcrRecord | null>(null);
  const [cameraWaiting, setCameraWaiting] = useState(false);
  const [cameras, setCameras] = useState<CameraNode[] | null>(null);
  const [snapshot, setSnapshot] = useState<PendingSnapshot | null>(null);
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [described, setDescribed] = useState(false);
  const [found, setFound] = useState<VisualSearch | null>(null);
  const editor = useRef<HTMLTextAreaElement>(null);
  const reviewBox = useRef<HTMLElement>(null);
  const outputBox = useRef<HTMLElement>(null);
  const activeRun = useRef(0);
  const runLock = useRef(false);
  const actionLock = useRef(false);

  useEffect(() => {
    let mounted = true;
    // A warm panel loads hidden and has no session until it is summoned, so
    // nothing is requested before the window is shown.
    const start = () => {
      if (document.visibilityState !== "visible") return;
      document.removeEventListener("visibilitychange", start);
      invoke<GlanceState>("glance_state").then(value => {
        if (mounted) {
          setState(value); setOcrLanguages(value.ocr?.defaults ?? []);
          setLanguage(current => current || value.visual?.translationTarget || "");
        }
      }).catch(e => { if (mounted) setError(errorMessage(e)); });
      editor.current?.focus();
    };
    document.addEventListener("visibilitychange", start);
    start();
    return () => { mounted = false; activeRun.current++; document.removeEventListener("visibilitychange", start); };
  }, []);

  const dismiss = useCallback(async () => {
    try { await invoke("glance_dismiss"); } catch (e) { setError(errorMessage(e)); }
  }, []);

  useEffect(() => {
    function escape(event: KeyboardEvent) {
      if (event.key !== "Escape" || event.isComposing || event.defaultPrevented) return;
      event.preventDefault();
      // Escape backs out of a pending sharing review before it closes the panel.
      if (review) { setReview(null); editor.current?.focus(); } else void dismiss();
    }
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [dismiss, review]);
  useEffect(() => {
    if (review) reviewBox.current?.focus();
    if (running) outputBox.current?.scrollIntoView({ block: "nearest" });
  }, [review, running]);

  const provider = state?.providers.find(p => p.id === providerId);
  const input = state && (providerId === null || provider)
    ? prepareGlance(state, provider, recipe, text, language, question, memory, documents, useImage && crop ? crop.id : undefined) : null;
  const imageReady = !!crop && !!provider?.imageInput && crop.imageEligible;
  const locked = running || !!review || !!pending;
  const allowMemory = state?.memoryEnabled && (!provider || provider.sharing.memories);
  const allowDocuments = !provider || provider.sharing.documents;
  const contextCount = (allowMemory && memory ? 1 : 0) + (allowDocuments ? documents.length : 0);
  const invalid = inputProblem(text, recipe, language, question);

  async function action(name: string, completed: (value: unknown) => void = () => {}, args?: Record<string, unknown>) {
    if (actionLock.current) return;
    actionLock.current = true;
    setPending(name); setError(""); setNotice("");
    try { completed(await invoke(name, args)); }
    catch (e) { setError(errorMessage(e)); }
    finally { setPending(null); actionLock.current = false; }
  }

  async function run(request: GlanceInput) {
    if (runLock.current) return;
    runLock.current = true;
    const id = ++activeRun.current;
    setReview(null); setError(""); setNotice(""); setResult(null); setOutput(""); setRemembered(false); setRunning(true);
    try {
      const deltas = new Channel<string>();
      deltas.onmessage = delta => { if (activeRun.current === id) setOutput(previous => previous + delta); };
      const final = await invoke<GlanceResult>("glance_run", { input: request, deltas });
      if (activeRun.current === id) {
        setOutput(final.text); setResult(final);
        if (final.error) setError(final.error);
      }
    } catch (e) { if (activeRun.current === id) setError(errorMessage(e)); }
    finally {
      if (activeRun.current === id) { activeRun.current++; setRunning(false); }
      runLock.current = false;
    }
  }

  function submit() {
    if (!input || locked) return;
    setError(""); setNotice("");
    if (input.recipe === "translate" && input.language !== state?.visual?.translationTarget) saveVisual(ocrLanguages, input.language ?? "");
    if (provider) {
      const chosen = provider;
      void action("glance_estimate", value => {
        setReview({ input, provider: chosen, estimate: value as Estimate, documents: state!.documents.filter(d => input.documents.includes(d.id)).map(d => d.name) });
      }, { input });
    } else { void run(input); }
  }

  function capture() {
    void action("glance_capture", value => {
      const info = value as CropInfo;
      setCrop(info); setUseImage(false); setRecognized(null);
      setNotice("Region captured and the screen stream closed. Nothing has been processed yet.");
    });
  }

  function extract() {
    if (!crop) return;
    void action("glance_ocr", value => {
      const { text: recognizedText, record } = value as { text: string; record: OcrRecord };
      setText(recognizedText); setSource("Screen text"); setRecognized(record);
      setNotice(recognizedText ? `Recognized on this device with ${record.engine} (${record.languages.map(languageName).join(", ")}) in ${record.elapsedMs} ms. Check the text before processing it.`
        : "No text was recognized in this region.");
      editor.current?.focus();
    }, { request: { crop: crop.id, languages: ocrLanguages } });
  }

  function discard() {
    void action("glance_capture_clear", () => { setCrop(null); setUseImage(false); setRecognized(null); });
  }

  /** Persist the shared visual choices; the panel keeps working if this fails. */
  function saveVisual(languages: string[], target: string) {
    invoke("glance_visual_settings", { settings: { ocrLanguages: languages, translationTarget: target.trim() } })
      .catch(e => setError(errorMessage(e)));
  }

  function changeLanguages(next: string[]) {
    setOcrLanguages(next);
    saveVisual(next, language);
  }

  function camera(device?: string) {
    setCameras(null); setSnapshot(null); setCameraWaiting(true);
    void action("glance_camera", value => {
      const taken = value as Taken;
      if (taken.kind === "choose") { setCameras(taken.cameras); return; }
      setSnapshot(taken.snapshot);
      setNotice("Snapshot taken and the camera turned off. Nothing has been processed yet.");
    }, { device: device ?? null }).finally(() => setCameraWaiting(false));
  }

  function useSnapshot(region: Region | null) {
    if (!snapshot) return;
    void action("glance_camera_confirm", value => {
      setCrop(value as CropInfo); setSnapshot(null); setUseImage(false); setRecognized(null); setFound(null);
      setNotice(region ? "Selected area kept; the rest of the snapshot was discarded. Nothing has been processed yet."
        : "Snapshot ready. Nothing has been processed yet.");
    }, { request: { id: snapshot.id, region } });
  }

  function openSearch(start: string, fromResult: boolean) {
    setQuery(suggestQuery(start)); setDescribed(fromResult); setFound(null); setSearching(true);
  }

  /** The text selected in the source box, or all of it. */
  function sourceSelection() {
    const box = editor.current;
    if (box && box.selectionEnd > box.selectionStart) return text.slice(box.selectionStart, box.selectionEnd);
    return text;
  }

  function search() {
    if (!state?.search?.provider || queryProblem(query)) return;
    void action("glance_search", value => {
      const result = value as VisualSearch;
      setFound(result);
      setNotice(result.results.length ? `${result.results.length} results from ${result.providerName}.` : `${result.providerName} returned no results.`);
    }, { request: { query, provider: state.search.provider, described } });
  }

  function choose(nextRecipe: Recipe) {
    setRecipe(nextRecipe);
    if (nextRecipe === "translate" && !text.trim() && crop && !provider?.imageInput) {
      setNotice("Extract the text first. It is translated by the model you choose under Process with.");
    }
  }

  function changeProvider(id: string) {
    setProviderId(id ? Number(id) : null); setMemory(false); setDocuments([]);
  }

  return <main className="glance">
    <header className="glance-bar">
      <span className="glance-mark"><QuoteIcon size={18} /></span><h1>Glance</h1>
      <span className="spacer" />
      <button className="glance-link" type="button" onClick={() => void action("glance_open_main")} disabled={!!pending}>
        Open main <LaunchIcon size={13} />
      </button>
      <IconButton label="Dismiss Glance (Esc)" onClick={() => void dismiss()}><CloseIcon /></IconButton>
    </header>
    <div className="glance-body">
      <section className="glance-source" aria-labelledby="source-label">
        <div className="glance-section-head">
          <label id="source-label" htmlFor="glance-source">Source text</label>
          <span>{text ? `${source} · ${Array.from(text).length.toLocaleString()} characters` : "Manual"}</span>
        </div>
        <textarea id="glance-source" ref={editor} value={text} readOnly={locked} dir="auto" placeholder="Paste or type the text to work with…"
          onChange={e => { setText(e.target.value); setSource(old => old === "Manual" ? old : old.includes("Screen") || old.includes("screen") ? "Edited screen text" : "Edited clipboard"); }} />
        <div className="glance-source-actions">
          <button type="button" disabled={locked} onClick={() => void action("glance_clipboard", value => {
            if (typeof value !== "string") return;
            setText(value); setSource("Clipboard"); editor.current?.focus();
            if (!value) setNotice("The clipboard contains no readable text.");
          })}><CopyIcon size={14} />Read clipboard</button>
          <button type="button" disabled={locked} onClick={capture}><CropIcon size={14} />Capture region</button>
          <button type="button" disabled={locked} onClick={() => camera()}><CameraIcon size={14} />Camera</button>
          <button type="button" disabled={locked} onClick={() => setNotice("Selection capture is unavailable on this desktop. Copy the selection, then choose Read clipboard, or paste it above.")}>
            <QuoteIcon size={14} />Use selection</button>
        </div>
      </section>
      {cameraWaiting && <CameraStatus stage="waiting" onCancel={() => void invoke("glance_stop").catch(e => setError(errorMessage(e)))} />}
      {cameras && !cameraWaiting && <CameraPicker cameras={cameras} disabled={locked} onChoose={key => camera(key)} onCancel={() => setCameras(null)} />}
      {snapshot && <SnapshotReview snapshot={snapshot} url={`glance-crop://localhost/${snapshot.id}.png`} disabled={locked} useLabel="Use"
        onUse={useSnapshot} onDiscard={() => void action("glance_camera_discard", () => { setSnapshot(null); setNotice("Snapshot discarded."); }, { id: snapshot.id })} />}
      {crop && <section className="glance-crop" aria-label={crop.source === "camera" ? "Camera snapshot" : "Screen region"}>
        <img src={cropUrl(crop)} alt={`${crop.source === "camera" ? "Camera snapshot" : "Selected screen region"}, ${crop.width} by ${crop.height} pixels`} />
        <div className="glance-crop-meta">
          <strong>{crop.source === "camera" ? "Camera snapshot" : "Screen region"}</strong>
          <span>{crop.width} × {crop.height} px · from a {crop.region.frameWidth} × {crop.region.frameHeight} {crop.source === "camera" ? "camera image" : "screen"}</span>
          {recognized && <span>{recognized.characters} characters recognized ({recognized.languages.map(languageName).join(", ")})</span>}
          <div className="glance-crop-actions" role="group" aria-label="Image actions">
            <button type="button" disabled={locked || !state?.ocr?.available || !ocrLanguages.length} onClick={extract}><ScanTextIcon size={14} />Extract text</button>
            <button type="button" disabled={locked} aria-pressed={recipe === "translate"} onClick={() => choose("translate")}><LanguagesIcon size={14} />Translate</button>
            <button type="button" disabled={locked} aria-pressed={recipe === "explain"} onClick={() => choose("explain")}><ThinkIcon size={14} />Explain</button>
            <button type="button" disabled={locked} aria-pressed={recipe === "custom"} onClick={() => choose("custom")}><QuoteIcon size={14} />Ask</button>
            <button type="button" disabled={locked} onClick={() => openSearch(sourceSelection(), false)}><SearchIcon size={14} />Search the web</button>
            {recognized && text && <button type="button" disabled={locked} onClick={() => void action("glance_copy", () => setNotice("Text copied."), { text })}><CopyIcon size={14} />Copy text</button>}
            <button type="button" disabled={locked} onClick={discard}><TrashIcon size={14} />Discard</button>
          </div>
          {state?.ocr?.available && <OcrLanguages available={state.ocr.languages} chosen={ocrLanguages} disabled={locked}
            missing={state.ocr.missingRecommended} onChange={changeLanguages} />}
          {state?.ocr?.guidance && <p className="glance-note" role="note">{state.ocr.guidance}</p>}
          {provider?.imageInput && <label className="glance-choice"><input type="checkbox" checked={useImage && imageReady} disabled={locked || !imageReady}
            onChange={e => setUseImage(e.target.checked)} /><ImageIcon size={14} />Send this image to {provider.name}</label>}
          {provider?.imageInput && !crop.imageEligible && <p className="glance-note">This image is too large to send.</p>}
          {!provider?.imageInput && !recognized && <p className="glance-note">The {provider ? `${provider.name} setup` : "local model"} reads text only and can't see images. Extract the text, or choose a provider set up for images.</p>}
        </div>
      </section>}
      {searching && <section className="glance-search" aria-labelledby="search-heading">
        <div className="glance-section-head"><h2 id="search-heading"><GlobeIcon size={15} />Search the web with visual clues</h2>
          <button type="button" className="glance-link" disabled={!!pending} onClick={() => { setSearching(false); setFound(null); }}>Close</button></div>
        {searchUnavailable(state?.search) ? <p className="glance-note">{searchUnavailable(state?.search)}</p> : <>
          <label htmlFor="glance-query">Search query (edit before searching)</label>
          <textarea id="glance-query" className="glance-query" dir="auto" value={query} readOnly={locked} rows={2}
            onChange={e => setQuery(e.target.value)} placeholder="Words to search for" />
          <div className="glance-source-actions">
            <button type="button" disabled={locked || !text.trim()} onClick={() => { setQuery(suggestQuery(sourceSelection())); setDescribed(false); }}><ScanTextIcon size={14} />Use {recognized ? "recognized" : "source"} text</button>
            <button type="button" disabled={locked || !output.trim() || !result || !!result.error} onClick={() => { setQuery(suggestQuery(output)); setDescribed(true); }}><ThinkIcon size={14} />Use the result</button>
          </div>
          {!text.trim() && !output.trim() && <p className="glance-note">There is no recognized text yet. Extract text from the image, describe the image with a provider set up for images and use its result, or type a query. A text-only local model can't recognize objects in an image.</p>}
          <p className="glance-note"><ShieldIcon size={13} /> {searchDisclosure(state!.search!, described)}</p>
          {queryProblem(query) && query.trim() && <p className="glance-error">{queryProblem(query)}</p>}
          <button type="button" className="primary-button" disabled={locked || !!queryProblem(query)} onClick={search}>
            {pending === "glance_search" ? <Spinner size={14} /> : <SearchIcon size={14} />}Search once with {state?.search?.name}</button>
        </>}
        {found && <div className="glance-results">
          <p className="hint">Results for “{found.query}” from {found.providerName}. Results are untrusted web content{state?.search?.personalUseOnly ? " and are not saved" : " and are not saved by this panel"}.</p>
          <ol>{found.results.map((item, index) => <li key={`${index}-${item.url}`}>
            <button type="button" className="glance-result-link" disabled={!!pending} onClick={() => void action("glance_search_open", () => {}, { index })}>
              <span dir="auto">[{index + 1}] {item.title}</span><ExternalIcon size={12} /></button>
            <span className="hint">{item.host}{item.published ? ` · ${item.published}` : ""}</span>
            {item.snippet && <p dir="auto">{item.snippet}</p>}
          </li>)}</ol>
          <details className="glance-receipt"><summary><ShieldIcon size={14} />Search receipt <ChevronDownIcon size={13} /></summary>
            <dl>
              <dt>Provider</dt><dd>{found.providerName}</dd>
              <dt>Sent</dt><dd>The query text only ({found.egress.map(record => `${record.bodyBytes ?? 0} bytes`).join(", ") || "no request completed"})</dd>
              <dt>Transmission</dt><dd>{found.egress.map(record => `${record.origin || record.destination} · ${record.transmission}`).join(", ") || "None"}</dd>
              <dt>Data classes</dt><dd>{[...new Set(found.egress.flatMap(record => record.labels))].map(id => visualClassNames[id] ?? id).join(", ") || "None"}</dd>
              <dt>Not sent</dt><dd>Images, screenshots, camera snapshots, documents, memories and conversation history</dd>
            </dl>
          </details>
        </div>}
      </section>}
      <fieldset disabled={locked} className="glance-options">
        <div className="glance-pair">
          <label>Action<select value={recipe} onChange={e => setRecipe(e.target.value as Recipe)}>
            {Object.entries(recipes).map(([value, name]) => <option key={value} value={value}>{name}</option>)}
          </select></label>
          <label>Process with<select value={providerId ?? ""} onChange={e => changeProvider(e.target.value)}>
            <option value="">Local model{state && !state.localAvailable ? " · unavailable" : ""}</option>
            {state?.providers.map(p => <option key={p.id} value={p.id}>{p.name} · {providerLocation(p)}</option>)}
          </select></label>
        </div>
        {recipe === "translate" && <label>Target language<input autoFocus value={language} placeholder="e.g. French" onChange={e => setLanguage(e.target.value)} /></label>}
        {recipe === "translate" && <p className="glance-note">The source box holds the original{recognized ? " recognized" : ""} text; correct it before translating. The translation is written by {provider ? `${provider.name} (${providerLocation(provider)}, after you review the request)` : "the local model on this device"}. Its accuracy and language coverage depend on that model.{recognized ? ` Text was recognized as ${recognized.languages.map(languageName).join(", ")}.` : ""}</p>}
        {recipe === "custom" && <label>Your question<input autoFocus value={question} placeholder="What would you like to know?" onChange={e => setQuestion(e.target.value)} /></label>}
        <details className="glance-context"><summary><SlidersIcon size={15} />Context<span>{contextCount ? `${contextCount} selected` : "None selected"}</span><ChevronDownIcon size={14} /></summary>
          <div className="glance-context-body">
            <label className="glance-choice"><input type="checkbox" checked={!!allowMemory && memory} disabled={!allowMemory} onChange={e => setMemory(e.target.checked)} />
              <MemoryIcon size={14} />Use saved memories</label>
            {!allowMemory && <p>{provider && !provider.sharing.memories ? "This provider’s settings withhold memories." : "Enable memory in the main window to use it here."}</p>}
            <h3>Knowledge documents</h3>
            {!allowDocuments ? <p>This provider’s settings withhold documents.</p> : !state?.documents.length ? <p>No documents are available.</p>
              : state.documents.map(doc => <label className="glance-choice" key={doc.id}><input type="checkbox"
                checked={documents.includes(doc.id)} onChange={e => setDocuments(current => e.target.checked ? [...current, doc.id] : current.filter(id => id !== doc.id))} />
                <FileIcon size={14} />{doc.name}</label>)}
          </div>
        </details>
      </fieldset>
      {state && !state.localAvailable && !provider && <p className="glance-note">Load a local model in the main window{state.providers.length ? " or choose a configured provider" : ""}.</p>}
      {review && <section className="glance-review" aria-labelledby="review-heading" tabIndex={-1} ref={reviewBox}>
        <h2 id="review-heading"><ShieldIcon size={16} />Review this request</h2>
        <p><strong>{review.provider.name}</strong> · {review.provider.model} · {providerLocation(review.provider)}</p>
        {review.input.image && crop && <img className="glance-review-image" src={cropUrl(crop)} alt={crop.source === "camera" ? "The exact snapshot area that will be sent" : "The exact region that will be sent"} />}
        <p>This sends {review.input.image ? "the image above" : ""}{review.input.image && review.input.text.trim() ? " and " : ""}{review.input.text.trim() ? "the source text above" : ""} with your “{recipes[review.input.recipe]}” instruction
          {review.input.question ? ` (“${review.input.question}”)` : review.input.language ? ` (to ${review.input.language})` : ""}.
          {review.input.memory ? " Saved memories may be included." : " Memories are off."}
          {review.documents.length ? ` Selected documents: ${review.documents.join(", ")}.` : " Documents are off."}
          {review.input.image ? (crop?.source === "camera" ? " Only this area of the snapshot is sent; the camera stays off." : " The rest of the screen is never sent.") : ""}</p>
        <p>Data: {review.estimate.classes.map(id => classNames[id] ?? id).join(", ")}.</p>
        <p>{retentionText(review.provider.retention)}{review.provider.retentionSummary ? ` ${review.provider.retentionSummary}` : ""}</p>
        <p>Estimated usage: {costText(review.estimate)}</p>
      </section>}
      {(running || output || result?.receipt) && <section aria-labelledby="result-title" ref={outputBox} className="glance-result">
        <div className="glance-section-head"><h2 id="result-title">Result</h2>{running && <span role="status"><Spinner size={13} />Generating</span>}</div>
        {output && <div className="glance-output" dir="auto">{output}</div>}
        {result && output && <div className="glance-result-actions">
          <button type="button" disabled={!!pending} onClick={() => void action("glance_copy", () => setNotice("Result copied."))}><CopyIcon size={14} />Copy</button>
          <button type="button" disabled={!!pending || remembered || result.receipt?.state !== "completed" || !!result.error} onClick={() => void action("glance_remember", () => { setRemembered(true); setNotice("Result saved to memory."); })}>
            {remembered ? <CheckIcon size={14} /> : <MemoryIcon size={14} />}{remembered ? "Remembered" : "Remember"}</button>
          {crop && <button type="button" disabled={!!pending || !!result.error} onClick={() => openSearch(output, true)}><SearchIcon size={14} />Search with this</button>}
        </div>}
        {result?.receipt && <ReceiptDetails receipt={result.receipt} screen={result.screen} />}
      </section>}
      {notice && <p role="status" className="glance-note">{notice}</p>}
      {invalid && <p role="status" className="glance-error">{invalid}</p>}
      {error && <p role="alert" className="glance-error"><AlertIcon size={15} />{error}</p>}
    </div>
    <footer className="glance-foot">
      <span className="glance-target">{provider ? <CloudIcon size={15} /> : <CpuIcon size={15} />}
        {provider ? provider.name : "On this device"}</span>
      <span className="spacer" />
      {running ? <button type="button" className="secondary-button" disabled={!!pending}
          onClick={() => void action("glance_stop", () => setNotice("Stop requested."))}><StopIcon size={14} />Stop</button>
        : review ? <><button type="button" className="ghost-button" onClick={() => setReview(null)}>Edit</button>
          <button type="button" className="primary-button" onClick={() => void run({ ...review.input, share: true })}><ArrowUpIcon size={15} />Send once</button></>
        : <button type="button" className="primary-button" disabled={!input || locked} onClick={submit}>
          {provider ? <ShieldIcon size={15} /> : <ThinkIcon size={15} />}{provider ? "Review request" : recipes[recipe]}</button>}
    </footer>
  </main>;
}
