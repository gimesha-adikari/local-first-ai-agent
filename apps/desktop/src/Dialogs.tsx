import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import {
  validModelSettings,
  type Appearance, type Bootstrap, type ModelReference, type ModelSettings, type Preferences,
} from "./chat";
import { errorMessage, type ModelInfo } from "./inference";

export function Dialog({
  title,
  children,
  onClose,
  busy = false,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  busy?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    dialog.showModal();
    return () => dialog.close();
  }, []);

  return (
    <dialog ref={ref} className="dialog" aria-labelledby={titleId} onCancel={(event) => {
      event.preventDefault();
      if (!busy) onClose();
    }}>
      <header className="dialog-heading">
        <h2 id={titleId}>{title}</h2>
        <button className="quiet" type="button" onClick={onClose} disabled={busy}>Close</button>
      </header>
      {children}
    </dialog>
  );
}

const settingsFields = [
  { key: "contextLength", title: "Context length", min: 128, max: 32768, step: 1, reload: true },
  { key: "threads", title: "CPU threads", min: 1, max: 32, step: 1, reload: true },
  { key: "maxTokens", title: "Max tokens", min: 1, max: 4096, step: 1, reload: false },
  { key: "temperature", title: "Temperature", min: 0, max: 2, step: 0.01, reload: false },
  { key: "topP", title: "Top-p", min: 0.01, max: 1, step: 0.01, reload: false },
  { key: "topK", title: "Top-k", min: 1, max: 200, step: 1, reload: false },
] as const;

function ModelFields({
  settings, setSettings, busy, loaded = false, supportsThinking = true,
}: {
  settings: ModelSettings;
  setSettings: (next: ModelSettings) => void;
  busy: boolean;
  loaded?: boolean;
  supportsThinking?: boolean;
}) {
  return (
    <>
      <div className="fields-grid">
        {settingsFields.map(({ key, title, min, max, step, reload }) => (
          <label className="field" key={key}>
            <span>{title}</span>
            <input type="number" min={min} max={max} step={step}
              disabled={busy || (loaded && reload)}
              value={Number.isNaN(settings[key]) ? "" : settings[key]}
              onChange={(event) => setSettings({ ...settings, [key]: event.currentTarget.valueAsNumber })} />
          </label>
        ))}
      </div>
      <label className="check-field">
        <input type="checkbox" checked={supportsThinking && settings.enableThinking} disabled={busy || !supportsThinking}
          onChange={(event) => setSettings({ ...settings, enableThinking: event.currentTarget.checked })} />
        <span>Enable thinking</span>
      </label>
    </>
  );
}

export function SettingsDialog({ snapshot, onSave, onClose }: {
  snapshot: Bootstrap;
  onSave: (preferences: Preferences) => void;
  onClose: () => void;
}) {
  const [preferences, setPreferences] = useState(snapshot.preferences);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(event: FormEvent) {
    event.preventDefault();
    if (saving || !validModelSettings(preferences.defaults)) return;
    setError(null);
    setSaving(true);
    try {
      await invoke("save_preferences", { preferences });
      onSave(preferences);
      onClose();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog title="Settings" onClose={onClose} busy={saving}>
      <form className="dialog-form" onSubmit={(event) => void save(event)}>
        <label className="field">
          <span>Appearance</span>
          <select value={preferences.appearance} onChange={(event) =>
            setPreferences({ ...preferences, appearance: event.currentTarget.value as Appearance })} disabled={saving}>
            <option value="system">System</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </label>
        <label className="check-field">
          <input type="checkbox" checked={preferences.enterToSend} disabled={saving}
            onChange={(event) => setPreferences({ ...preferences, enterToSend: event.currentTarget.checked })} />
          <span>Enter sends a message</span>
        </label>

        <fieldset className="dialog-section">
          <legend>Default model settings</legend>
          <ModelFields settings={preferences.defaults} busy={saving}
            setSettings={(defaults) => setPreferences({ ...preferences, defaults })} />
          <p className="hint">Applies when you add a model. Shift+Enter adds a new line in the composer.</p>
        </fieldset>

        <div className="data-location">
          <span>Database location</span>
          <code>{snapshot.databasePath}</code>
        </div>
        {!validModelSettings(preferences.defaults) && <p className="hint">Set a token limit smaller than the context length.</p>}
        {error && <p className="error" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button className="primary" type="submit" disabled={saving || !validModelSettings(preferences.defaults)}>
            {saving ? "Saving…" : "Save preferences"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

const modelStatus = {
  empty: "No model loaded",
  loading: "Loading model…",
  ready: "Model ready",
  generating: "Generating…",
  unloading: "Unloading model…",
};

export function ModelDialog({
  snapshot, selectedModelId, onSelect, onModelChange, onClose,
}: {
  snapshot: Bootstrap;
  selectedModelId: number | null;
  onSelect: (id: number | null) => void;
  onModelChange: (snapshot: Bootstrap) => void;
  onClose: () => void;
}) {
  const current = snapshot.models.find(({ id }) => id === selectedModelId);
  const [settings, setSettings] = useState(current?.settings ?? snapshot.preferences.defaults);
  const [action, setAction] = useState<"choose" | "load" | "unload" | "save" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const info = snapshot.inference.model;
  const loaded = snapshot.inference.phase === "ready";
  const busy = action !== null;
  const valid = validModelSettings(settings);

  function select(id: number | null) {
    const reference = snapshot.models.find((model) => model.id === id);
    onSelect(id);
    setSettings(reference?.settings ?? snapshot.preferences.defaults);
    setNotice(null);
    setError(null);
  }

  async function run(kind: NonNullable<typeof action>) {
    if (busy) return;
    setError(null);
    setNotice(null);
    setAction(kind);
    try {
      if (kind === "choose") {
        const reference = await invoke<ModelReference | null>("choose_model");
        if (reference) {
          onSelect(reference.id);
          setSettings(reference.settings);
          onModelChange({ ...snapshot, models: [reference, ...snapshot.models.filter(({ id }) => id !== reference.id)] });
        }
      }
      if (kind === "load" && current) {
        const model = await invoke<ModelInfo>("load_model", { modelId: current.id, settings });
        onModelChange({
          ...snapshot,
          models: [{ ...current, settings }, ...snapshot.models.filter(({ id }) => id !== current.id)],
          activeModelId: current.id,
          inference: { phase: "ready", model },
        });
      }
      if (kind === "unload") {
        await invoke("unload_model");
        onModelChange({ ...snapshot, activeModelId: null, inference: { phase: "empty", model: null } });
      }
      if (kind === "save" && current) {
        await invoke("save_model_settings", { modelId: current.id, settings });
        onModelChange({
          ...snapshot,
          models: snapshot.models.map((reference) => reference.id === current.id ? { ...reference, settings } : reference),
        });
        setNotice("Model settings saved.");
      }
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setAction(null);
    }
  }

  return (
    <Dialog title="Model controls" onClose={onClose} busy={busy}>
      <div className="dialog-form">
        <div className="model-status" role="status">
          <strong>{action === "load" ? "Loading model…" : action === "unload" ? "Unloading model…" : modelStatus[snapshot.inference.phase]}</strong>
          {info && <span>{info.name} · {info.architecture} · {(info.fileBytes / 1024 ** 2).toFixed(1)} MiB · loaded in {info.loadMs} ms</span>}
        </div>
        <label className="field">
          <span>Recent model</span>
          <select value={selectedModelId ?? ""} disabled={busy || loaded}
            onChange={(event) => select(event.currentTarget.value ? Number(event.currentTarget.value) : null)}>
            <option value="">Choose a model</option>
            {snapshot.models.map((model) => <option value={model.id} key={model.id}>{model.fileName}</option>)}
          </select>
        </label>
        <div className="model-actions">
          <button type="button" disabled={busy || loaded} onClick={() => void run("choose")}>Choose model</button>
          <button type="button" className="primary" disabled={busy || loaded || !current || !valid}
            onClick={() => void run("load")}>Load model</button>
          <button type="button" disabled={busy || !loaded} onClick={() => void run("unload")}>Unload model</button>
        </div>
        <fieldset className="dialog-section">
          <legend>Inference settings</legend>
          <ModelFields settings={settings} setSettings={setSettings} busy={busy} loaded={loaded}
            supportsThinking={info?.supportsThinking ?? true} />
          {loaded && <p className="hint">Unload the model to change context length or CPU threads.</p>}
          {info && !info.supportsThinking && <p className="hint">This model’s chat template has no separate thinking mode.</p>}
        </fieldset>
        {!valid && <p className="hint">Set valid model settings with room in the context for your input.</p>}
        {error && <p role="alert" className="error">{error}</p>}
        {notice && <p role="status" className="notice">{notice}</p>}
        <div className="dialog-actions">
          <button type="button" disabled={busy || !current || !valid} onClick={() => void run("save")}>Save model settings</button>
        </div>
      </div>
    </Dialog>
  );
}
