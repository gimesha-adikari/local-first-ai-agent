import { Channel, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { AlertIcon, ArrowUpIcon, CheckIcon, ChevronDownIcon, CloseIcon, CloudIcon, CopyIcon, CpuIcon, CropIcon, FileIcon,
  ImageIcon, LaunchIcon, MemoryIcon, QuoteIcon, ScanTextIcon, ShieldIcon, SlidersIcon, Spinner, StopIcon, ThinkIcon, TrashIcon } from "./icons";
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
  const { capture, region, ocr } = screen;
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
        if (mounted) { setState(value); setOcrLanguages(value.ocr?.defaults ?? []); }
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
          <button type="button" disabled={locked} onClick={() => setNotice("Selection capture is unavailable on this desktop. Copy the selection, then choose Read clipboard, or paste it above.")}>
            <QuoteIcon size={14} />Use selection</button>
        </div>
      </section>
      {crop && <section className="glance-crop" aria-label="Screen region">
        <img src={cropUrl(crop)} alt={`Selected screen region, ${crop.width} by ${crop.height} pixels`} />
        <div className="glance-crop-meta">
          <strong>Screen region</strong>
          <span>{crop.width} × {crop.height} px · from a {crop.region.frameWidth} × {crop.region.frameHeight} screen</span>
          {recognized && <span>{recognized.characters} characters recognized ({recognized.languages.map(languageName).join(", ")})</span>}
          <div className="glance-crop-actions">
            <button type="button" disabled={locked || !state?.ocr?.available || !ocrLanguages.length} onClick={extract}><ScanTextIcon size={14} />Extract text</button>
            {recognized && text && <button type="button" disabled={locked} onClick={() => void action("glance_copy", () => setNotice("Text copied."), { text })}><CopyIcon size={14} />Copy text</button>}
            <button type="button" disabled={locked} onClick={discard}><TrashIcon size={14} />Discard</button>
          </div>
          {state?.ocr?.available && <details className="glance-languages"><summary>Languages: {ocrLanguages.map(languageName).join(", ") || "none"}<ChevronDownIcon size={12} /></summary>
            <div>{state.ocr.languages.map(code => <label className="glance-choice" key={code} title={code}><input type="checkbox" disabled={locked || (!ocrLanguages.includes(code) && ocrLanguages.length >= 4)}
              checked={ocrLanguages.includes(code)} onChange={e => setOcrLanguages(current => e.target.checked
                ? (current.length < 4 ? [...current, code] : current) : current.filter(item => item !== code))} />{languageName(code)}</label>)}</div>
            <p>Recognition quality differs by language and script. Choose up to four.</p>
            {state.ocr.missingRecommended.length > 0 && <p>More languages: install <code>{state.ocr.missingRecommended.join(" ")}</code> with apt, then reopen this panel.</p>}
          </details>}
          {state?.ocr?.guidance && <p className="glance-note" role="note">{state.ocr.guidance}</p>}
          {provider?.imageInput && <label className="glance-choice"><input type="checkbox" checked={useImage && imageReady} disabled={locked || !imageReady}
            onChange={e => setUseImage(e.target.checked)} /><ImageIcon size={14} />Send this image to {provider.name}</label>}
          {provider?.imageInput && !crop.imageEligible && <p className="glance-note">This region is too large to send as an image.</p>}
        </div>
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
      {state?.localOnly && <p className="glance-note">Local-only mode is on: requests stay on this device. Screen capture and text recognition still work.</p>}
      {state && !state.localAvailable && !provider && <p className="glance-note">Load a local model in the main window{state.providers.length ? " or choose a configured provider" : ""}.</p>}
      {review && <section className="glance-review" aria-labelledby="review-heading" tabIndex={-1} ref={reviewBox}>
        <h2 id="review-heading"><ShieldIcon size={16} />Review this request</h2>
        <p><strong>{review.provider.name}</strong> · {review.provider.model} · {providerLocation(review.provider)}</p>
        {review.input.image && crop && <img className="glance-review-image" src={cropUrl(crop)} alt="The exact region that will be sent" />}
        <p>This sends {review.input.image ? "the image above" : ""}{review.input.image && review.input.text.trim() ? " and " : ""}{review.input.text.trim() ? "the source text above" : ""} with your “{recipes[review.input.recipe]}” instruction
          {review.input.question ? ` (“${review.input.question}”)` : review.input.language ? ` (to ${review.input.language})` : ""}.
          {review.input.memory ? " Saved memories may be included." : " Memories are off."}
          {review.documents.length ? ` Selected documents: ${review.documents.join(", ")}.` : " Documents are off."}
          {review.input.image ? " The rest of the screen is never sent." : ""}</p>
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
