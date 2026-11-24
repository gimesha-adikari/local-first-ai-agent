import { Channel, invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import {
  appendDelta, errorMessage,
  type GenerationStats, type ModelConfig, type ModelInfo, type Output, type Phase, type Status, type TextDelta,
} from "./inference";

const field = "w-full rounded-md border border-slate-600 bg-slate-950 px-3 py-2 text-slate-100";
const button = "rounded-md border border-slate-600 px-4 py-2 font-medium enabled:hover:bg-slate-700 disabled:opacity-45";
const primary = "rounded-md bg-teal-300 px-4 py-2 font-semibold text-slate-950 enabled:hover:bg-teal-200 disabled:opacity-45";
const panel = "rounded-xl border border-slate-700 bg-slate-900 p-5";

const phaseLabel: Record<Phase, string> = {
  empty: "No model loaded",
  loading: "Loading model…",
  ready: "Model ready",
  generating: "Generating…",
  unloading: "Unloading model…",
};

const finishLabel: Record<GenerationStats["reason"], string> = {
  end_of_generation: "Complete",
  token_limit: "Token limit reached",
  cancelled: "Stopped",
};

function NumberField({ label, value, onChange, min, max, step = 1 }: {
  label: string;
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  step?: number;
}) {
  return (
    <label className="grid gap-1 text-sm text-slate-300">
      {label}
      <input className={field} type="number" value={Number.isNaN(value) ? "" : value}
        min={min} max={max} step={step} onChange={(event) => onChange(event.currentTarget.valueAsNumber)} />
    </label>
  );
}

export default function App() {
  const [phase, setPhase] = useState<Phase | "initializing" | "choosing">("initializing");
  const [selected, setSelected] = useState<string | null>(null);
  const [model, setModel] = useState<ModelInfo | null>(null);
  const [threads, setThreads] = useState(4);
  const [contextLength, setContextLength] = useState(2048);
  const [maxTokens, setMaxTokens] = useState(256);
  const [temperature, setTemperature] = useState(0.7);
  const [topP, setTopP] = useState(0.8);
  const [topK, setTopK] = useState(20);
  const [enableThinking, setEnableThinking] = useState(false);
  const [prompt, setPrompt] = useState("");
  const [output, setOutput] = useState<Output>({ id: 0, text: "" });
  const [stats, setStats] = useState<GenerationStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const nextId = useRef(0);

  useEffect(() => {
    let active = true;
    void invoke<Status>("inference_status").then((status) => {
      if (!active) return;
      setPhase(status.phase);
      setModel(status.model);
      if (status.model) {
        setSelected(status.model.fileName);
        setThreads(status.model.config.threads);
        setContextLength(status.model.config.contextLength);
      }
    }).catch((failure: unknown) => {
      if (!active) return;
      setError(errorMessage(failure));
      setPhase("empty");
    });
    return () => { active = false; };
  }, []);

  async function choose() {
    setError(null);
    setPhase("choosing");
    try {
      const name = await invoke<string | null>("choose_model");
      if (name) setSelected(name);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setPhase("empty");
    }
  }

  async function load() {
    setError(null);
    setPhase("loading");
    try {
      const config: ModelConfig = { contextLength, threads };
      setModel(await invoke<ModelInfo>("load_model", { config }));
      setEnableThinking(false);
      setOutput({ id: nextId.current, text: "" });
      setStats(null);
      setPhase("ready");
    } catch (failure) {
      setError(errorMessage(failure));
      setPhase("empty");
    }
  }

  async function unload() {
    setError(null);
    setPhase("unloading");
    try {
      await invoke("unload_model");
      setModel(null);
      setPhase("empty");
    } catch (failure) {
      setError(errorMessage(failure));
      setPhase("ready");
    }
  }

  async function generate() {
    setError(null);
    setStats(null);
    setPhase("generating");
    const id = ++nextId.current;
    setOutput({ id, text: "" });
    const tokens = new Channel<TextDelta>((delta) => {
      setOutput((previous) => appendDelta(previous, id, delta));
    });
    try {
      const request = { prompt, maxTokens, temperature, topP, topK, enableThinking };
      setStats(await invoke<GenerationStats>("generate", { request, tokens }));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setStopping(false);
      setPhase("ready");
    }
  }

  async function stop() {
    setStopping(true);
    try {
      if (!await invoke<boolean>("stop_generation")) setStopping(false);
    } catch (failure) {
      setError(errorMessage(failure));
      setStopping(false);
    }
  }

  const validModelConfig = Number.isInteger(threads) && threads >= 1 && threads <= 32
    && Number.isInteger(contextLength) && contextLength >= 128 && contextLength <= 32768;
  const validGeneration = Number.isInteger(maxTokens) && maxTokens >= 1 && maxTokens <= 4096
    && temperature >= 0 && temperature <= 2 && topP > 0 && topP <= 1
    && Number.isInteger(topK) && topK >= 1 && topK <= 200;

  return (
    <main className="min-h-screen bg-slate-950 px-4 py-6 text-slate-100 sm:px-7">
      <div className="mx-auto grid max-w-3xl gap-4">
        <h1 className="text-2xl font-semibold">Local inference</h1>

        <section aria-labelledby="model-heading" className={panel}>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0">
              <h2 id="model-heading" className="font-semibold">GGUF model</h2>
              <p className="mt-1 break-all text-sm text-slate-300">{selected ?? "Choose a local GGUF file"}</p>
            </div>
            <p role="status" className="text-sm text-teal-200">
              {phase === "initializing" ? "Starting…" : phase === "choosing" ? "Choosing model…" : phaseLabel[phase]}
            </p>
          </div>
          <fieldset disabled={phase !== "empty"} className="mt-4 grid grid-cols-2 gap-3">
            <NumberField label="Context length" value={contextLength} onChange={setContextLength} min={128} max={32768} />
            <NumberField label="CPU threads" value={threads} onChange={setThreads} min={1} max={32} />
          </fieldset>
          <div className="mt-4 flex flex-wrap gap-2">
            <button className={button} type="button" disabled={phase !== "empty"} onClick={choose}>Choose model</button>
            <button className={button} type="button" disabled={phase !== "empty" || !selected || !validModelConfig} onClick={load}>Load model</button>
            <button className={button} type="button" disabled={phase !== "ready"} onClick={unload}>Unload model</button>
          </div>
          {model && (
            <p className="mt-3 break-words text-sm text-slate-300">
              {model.name} · {model.architecture} · {(model.fileBytes / 1024 ** 2).toFixed(1)} MiB · loaded in {model.loadMs} ms
            </p>
          )}
        </section>

        <section aria-labelledby="prompt-heading" className={panel}>
          <h2 id="prompt-heading" className="font-semibold">Prompt</h2>
          <textarea aria-label="Prompt" className={`${field} mt-3 min-h-24 resize-y`} value={prompt}
            maxLength={65536} disabled={phase === "generating"} onChange={(event) => setPrompt(event.currentTarget.value)} />
          <fieldset disabled={phase === "generating"} className="mt-3 grid grid-cols-2 gap-3 sm:grid-cols-4">
            <NumberField label="Max tokens" value={maxTokens} onChange={setMaxTokens} min={1} max={4096} />
            <NumberField label="Temperature" value={temperature} onChange={setTemperature} min={0} max={2} step={0.05} />
            <NumberField label="Top-p" value={topP} onChange={setTopP} min={0.01} max={1} step={0.01} />
            <NumberField label="Top-k" value={topK} onChange={setTopK} min={1} max={200} />
          </fieldset>
          {model?.supportsThinking && (
            <label className="mt-3 flex items-center gap-2 text-sm text-slate-300">
              <input type="checkbox" checked={enableThinking} disabled={phase !== "ready"}
                onChange={(event) => setEnableThinking(event.currentTarget.checked)} />
              Enable thinking
            </label>
          )}
          <div className="mt-4 flex gap-2">
            <button className={primary} type="button" disabled={phase !== "ready" || !prompt.trim() || !validGeneration} onClick={generate}>Generate</button>
            <button className={button} type="button" disabled={phase !== "generating" || stopping} onClick={stop}>
              {stopping ? "Stopping…" : "Stop"}
            </button>
          </div>
          {error && <p role="alert" className="mt-3 text-sm text-red-300">{error}</p>}
        </section>

        <section aria-labelledby="response-heading" className={panel}>
          <h2 id="response-heading" className="font-semibold">Response</h2>
          <div tabIndex={0} aria-label="Generated text"
            className="mt-3 max-h-96 min-h-20 overflow-auto whitespace-pre-wrap break-words rounded-md bg-slate-950 p-3">
            {output.text}
          </div>
          {stats && (
            <p role="status" className="mt-3 text-sm text-slate-300">
              {finishLabel[stats.reason]} · {stats.generatedTokens} tokens · {stats.tokensPerSecond.toFixed(1)} tokens/s
              {stats.timeToFirstTokenMs !== null && ` · first token in ${stats.timeToFirstTokenMs} ms`}
              {stats.promptTokensPerSecond !== null && ` · prompt ${stats.promptTokensPerSecond.toFixed(1)} tokens/s`}
            </p>
          )}
        </section>
      </div>
    </main>
  );
}
