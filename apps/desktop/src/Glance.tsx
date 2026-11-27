import { Channel, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { AlertIcon, ArrowUpIcon, CheckIcon, ChevronDownIcon, CloseIcon, CloudIcon, CopyIcon,
  CpuIcon, FileIcon, LaunchIcon, MemoryIcon, QuoteIcon, ShieldIcon, SlidersIcon, Spinner, StopIcon, ThinkIcon } from "./icons";
import { errorMessage } from "./inference";
import { inputProblem, prepareGlance, recipes, type GlanceReceipt, type GlanceInput, type GlanceProvider, type GlanceResult, type GlanceState, type Recipe } from "./glance";
import { IconButton } from "./ui";
import "./glance.css";

type Review = { input: GlanceInput; provider: GlanceProvider; documents: string[] };

function providerLocation(provider: GlanceProvider) {
  return ["self_hosted", "self-hosted"].includes(provider.locality) ? "Self-hosted" : "Cloud";
}

function retentionText(value: string) {
  if (value === "none") return "Provider declares no retention.";
  if (value === "provider_policy" || value === "provider_declared") return "The provider’s retention policy applies.";
  if (value === "unknown" || !value) return "Provider retention is unknown.";
  return value;
}

function ReceiptDetails({ receipt }: { receipt: GlanceReceipt }) {
  const sourceKinds = (sources: GlanceReceipt["used"]) => [...new Set(sources.map(item => item.kind.replaceAll("_", " ")))].join(", ") || "None";
  return <details className="glance-receipt">
    <summary><ShieldIcon size={14} />Processing receipt <ChevronDownIcon size={13} /></summary>
    <dl>
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
  const editor = useRef<HTMLTextAreaElement>(null);
  const reviewBox = useRef<HTMLElement>(null);
  const outputBox = useRef<HTMLElement>(null);
  const activeRun = useRef(0);
  const runLock = useRef(false);
  const actionLock = useRef(false);

  useEffect(() => {
    let mounted = true;
    invoke<GlanceState>("glance_state").then(value => {
      if (mounted) setState(value);
    }).catch(e => { if (mounted) setError(errorMessage(e)); });
    editor.current?.focus();
    return () => { mounted = false; activeRun.current++; };
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
    ? prepareGlance(state, provider, recipe, text, language, question, memory, documents) : null;
  const locked = running || !!review || !!pending;
  const allowMemory = state?.memoryEnabled && (!provider || provider.sharing.memories);
  const allowDocuments = !provider || provider.sharing.documents;
  const contextCount = (allowMemory && memory ? 1 : 0) + (allowDocuments ? documents.length : 0);
  const invalid = inputProblem(text, recipe, language, question);

  async function action(name: string, completed: (value: unknown) => void = () => {}) {
    if (actionLock.current) return;
    actionLock.current = true;
    setPending(name); setError(""); setNotice("");
    try { completed(await invoke(name)); }
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
      setReview({ input, provider, documents: state!.documents.filter(d => input.documents.includes(d.id)).map(d => d.name) });
    } else { void run(input); }
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
        <textarea id="glance-source" ref={editor} value={text} readOnly={locked} placeholder="Paste or type the text to work with…"
          onChange={e => { setText(e.target.value); setSource(old => old === "Manual" ? old : "Edited clipboard"); }} />
        <div className="glance-source-actions">
          <button type="button" disabled={locked} onClick={() => void action("glance_clipboard", value => {
            if (typeof value !== "string") return;
            setText(value); setSource("Clipboard"); editor.current?.focus();
            if (!value) setNotice("The clipboard contains no readable text.");
          })}><CopyIcon size={14} />Read clipboard</button>
          <button type="button" disabled={locked} onClick={() => setNotice("Selection capture is unavailable on this desktop. Copy the selection, then choose Read clipboard, or paste it above.")}>
            <QuoteIcon size={14} />Use selection</button>
        </div>
      </section>
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
      {state && !state.localAvailable && !provider && <p className="glance-note">Load a local model in the main window{state.providers.length ? " or choose a configured provider" : ""}.</p>}
      {review && <section className="glance-review" aria-labelledby="review-heading" tabIndex={-1} ref={reviewBox}>
        <h2 id="review-heading"><ShieldIcon size={16} />Review this request</h2>
        <p><strong>{review.provider.name}</strong> · {review.provider.model} · {providerLocation(review.provider)}</p>
        <p>This sends the source text above and your “{recipes[review.input.recipe]}” instruction
          {review.input.question ? ` (“${review.input.question}”)` : review.input.language ? ` (to ${review.input.language})` : ""}.
          {review.input.memory ? " Saved memories may be included." : " Memories are off."}
          {review.documents.length ? ` Selected documents: ${review.documents.join(", ")}.` : " Documents are off."}</p>
        <p>{retentionText(review.provider.retention)}</p>
      </section>}
      {(running || output || result?.receipt) && <section aria-labelledby="result-title" ref={outputBox} className="glance-result">
        <div className="glance-section-head"><h2 id="result-title">Result</h2>{running && <span role="status"><Spinner size={13} />Generating</span>}</div>
        {output && <div className="glance-output">{output}</div>}
        {result && output && <div className="glance-result-actions">
          <button type="button" disabled={!!pending} onClick={() => void action("glance_copy", () => setNotice("Result copied."))}><CopyIcon size={14} />Copy</button>
          <button type="button" disabled={!!pending || remembered || result.receipt?.state !== "completed" || !!result.error} onClick={() => void action("glance_remember", () => { setRemembered(true); setNotice("Result saved to memory."); })}>
            {remembered ? <CheckIcon size={14} /> : <MemoryIcon size={14} />}{remembered ? "Remembered" : "Remember"}</button>
        </div>}
        {result?.receipt && <ReceiptDetails receipt={result.receipt} />}
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
