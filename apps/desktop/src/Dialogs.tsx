import { invoke } from "@tauri-apps/api/core";
import { useState, type FormEvent, type ReactNode } from "react";
import lfmLicense from "../../../third_party/model-licenses/LFM2.5-1.2B-Instruct/LICENSE?raw";
import { formatBytes, remainingLabel } from "./agent";
import type { AgentScopeDisplay } from "./agentTypes";
import {
  lowOutputLimit, matchesRecommended, recommendedChatSettings, settingChanges, shortOutputLimit, validModelSettings,
  type Appearance, type Bootstrap, type BundledModel, type ModelReference, type ModelSettings, type Preferences,
  type RecommendedChatSettings,
} from "./chat";
import {
  AgentIcon, ChatIcon, CheckIcon, ChevronRightIcon, CloseIcon, CubeIcon, FileIcon, FolderIcon, LockIcon, MonitorIcon,
  MoonIcon, PowerIcon, Spinner, SunIcon, ThinkIcon, UploadIcon,
} from "./icons";
import type { ModelInfo } from "./inference";
import {
  advancedModelName, builtInModelName, capabilityGuidance, modelCapabilities, modelLabel, modelProfile,
} from "./models";
import { errorMessage } from "./inference";
import { Callout, Dialog, Segmented, Switch } from "./ui";

export { Dialog } from "./ui";

const settingsFields = [
  { key: "maxTokens", title: "Output limit", unit: "tokens", min: 1, max: 4096, step: 1, reload: false,
    hint: "Longest reply. Thinking counts toward it." },
  { key: "contextLength", title: "Context length", unit: "tokens", min: 128, max: 32768, step: 1, reload: true,
    hint: "Total space for instructions, your input, history and generated output. Larger uses more memory." },
  { key: "temperature", title: "Temperature", unit: "", min: 0, max: 2, step: 0.01, reload: false,
    hint: "Higher gives more varied wording." },
  { key: "topP", title: "Top-p", unit: "", min: 0.01, max: 1, step: 0.01, reload: false, hint: "Nucleus sampling." },
  { key: "topK", title: "Top-k", unit: "", min: 1, max: 200, step: 1, reload: false, hint: "Candidate tokens per step." },
  { key: "threads", title: "CPU threads", unit: "", min: 1, max: 32, step: 1, reload: true,
    hint: "Processor threads used for replies." },
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
        {settingsFields.map(({ key, title, unit, min, max, step, reload, hint }) => (
          <label className="field" key={key}>
            <span>{title}{unit && <small> {unit}</small>}</span>
            <input type="number" min={min} max={max} step={step} disabled={busy || (loaded && reload)}
              value={Number.isNaN(settings[key]) ? "" : settings[key]}
              onChange={(event) => setSettings({ ...settings, [key]: event.currentTarget.valueAsNumber })} />
            <small className="hint">{loaded && reload ? "Takes effect after the model reloads. Unload it to edit here." : hint}</small>
          </label>
        ))}
      </div>
      <Switch label="Think by default" disabled={busy || !supportsThinking}
        checked={supportsThinking && settings.enableThinking}
        onChange={(enableThinking) => setSettings({ ...settings, enableThinking })}
        description={supportsThinking
          ? "Saved preference only. Each message still uses the Thinking toggle in the composer."
          : "This model has no separate Thinking mode."} />
      {lowOutputLimit(settings) && (
        <p className="hint">
          The output limit is below {shortOutputLimit} tokens. Longer replies, and replies with Thinking, may be cut off.
        </p>
      )}
    </>
  );
}

/** Plain-language summary of a settings set. */
function settingsSummary(settings: ModelSettings) {
  return `Replies up to ${settings.maxTokens.toLocaleString()} tokens · ${settings.contextLength.toLocaleString()}-token `
    + `total context · Thinking ${settings.enableThinking ? "on" : "off"}`;
}

/** Collapsed section for numeric model controls. */
function Advanced({ open, onToggle, children }: { open: boolean; onToggle: (open: boolean) => void; children: ReactNode }) {
  return (
    <details className="advanced-settings" open={open} onToggle={(event) => onToggle(event.currentTarget.open)}>
      <summary><ChevronRightIcon size={14} />Advanced settings</summary>
      <div className="advanced-body">{children}</div>
    </details>
  );
}

function OutputLimitExplanation() {
  return (
    <p className="hint">
      Instructions, your input, conversation history and generated output all share the context. The output limit is a
      ceiling. For each reply the app measures the prompt with the model's tokenizer, reserves about a
      quarter of the context for output (a third with Thinking), fits recent history, then allows up to this limit in the
      remaining space. Thinking and the answer share it. A reply shows the effective limit when less was available.
    </p>
  );
}

/**
 * Recommendation for the loaded model from `recommended_chat_settings`. Nothing
 * is saved until the person picks an explicit action; context and thread changes
 * need a reload.
 */
function RecommendedSettings({
  saved, recommended, disabled, onReview, onApply, onReload,
}: {
  saved: ModelSettings;
  recommended: RecommendedChatSettings;
  disabled: boolean;
  onReview: (settings: ModelSettings) => void;
  onApply: (settings: ModelSettings) => void;
  onReload: (settings: ModelSettings) => void;
}) {
  const next = recommended.settings;
  const changes = settingChanges(saved, next);
  const needsReload = changes.some((change) => change.reload);
  const valid = validModelSettings(next);
  const trained = recommended.trainedContext;
  return (
    <section className="recommended-panel" aria-label="Recommended settings">
      <div className="section-heading-row">
        <h4>Recommended settings</h4>
        {changes.length === 0 && <span className="hint inline-ok"><CheckIcon size={13} />In use</span>}
      </div>
      <p className="hint">{settingsSummary(next)}.</p>
      {trained === null ? (
        <p className="hint">The loaded model couldn't be checked, so this is the general recommendation.</p>
      ) : next.contextLength < Math.min(trained, 8192) ? (
        <p className="hint">The recommendation uses a smaller context based on available memory.</p>
      ) : trained < 8192 && (
        <p className="hint">This model was trained with {trained.toLocaleString()} tokens of context, so the recommendation uses that.</p>
      )}
      {changes.length > 0 && (
        <>
          <p className="hint">
            Changes from your saved settings: {changes.map((change) => `${change.label} ${change.from} → ${change.to}`).join(" · ")}.
            {needsReload && " The model reloads to apply them."}
          </p>
          <div className="recommended-actions">
            {needsReload ? (
              <button type="button" className="primary-button small" disabled={disabled || !valid} onClick={() => onReload(next)}>
                Use recommended and reload
              </button>
            ) : (
              <button type="button" className="primary-button small" disabled={disabled || !valid} onClick={() => onApply(next)}>
                Use recommended
              </button>
            )}
            <button type="button" className="ghost-button small" disabled={disabled} onClick={() => onReview(next)}>
              Review in Advanced settings
            </button>
          </div>
        </>
      )}
    </section>
  );
}

function Capabilities({ fileName, info }: { fileName: string; info: ModelInfo | null }) {
  const known = modelCapabilities(fileName, info);
  if (!known) return <p className="hint">Thinking and Agent support are checked when the model loads.</p>;
  const icons = { chat: <ChatIcon size={13} />, thinking: <ThinkIcon size={13} />, agent: <AgentIcon size={13} /> };
  return (
    <div className="capabilities">
      <ul aria-label="Capabilities">
        {known.capabilities.map(({ key, label, available }) => (
          <li key={key} className={available ? "available" : "unavailable"}>
            {icons[key]}{label}
            <span className="capability-state">{available ? <CheckIcon size={12} /> : <CloseIcon size={12} />}
              <span className="sr-only">{available ? "available" : "unavailable"}</span></span>
          </li>
        ))}
      </ul>
      {!known.checked && <small className="hint">Expected for this model. Confirmed when it loads.</small>}
    </div>
  );
}

/** The bundled model's license, notice summary and full text. */
const licenseText = lfmLicense.replace(/\r\n?/g, "\n").replace(/\n{3,}/g, "\n\n").trim();

export function ModelLicense() {
  return (
    <section className="settings-section" aria-label={`${builtInModelName} license`}>
      <h4>{builtInModelName} license and notices</h4>
      <p className="hint">
        The built-in model is LFM2.5-1.2B-Instruct by Liquid AI, Inc., distributed unmodified under the LFM Open License v1.0.
        That license covers only the model file, not this app.
      </p>
      <p className="hint">
        <strong>Commercial use is restricted.</strong> It is licensed only while the using organization, with its affiliates, has
        less than US$10 million in annual revenue. Larger organizations need a separate agreement with Liquid AI. Qualified
        non-profits are exempt for non-commercial or research use. If you redistribute the model, include its license. This
        summary isn't legal advice; the full license below is binding.
      </p>
      <details className="license-text">
        <summary><ChevronRightIcon size={14} />Full license text</summary>
        <pre tabIndex={0}>{licenseText}</pre>
      </details>
    </section>
  );
}

export function SettingsDialog({ snapshot, onSave, onClose, onManageModels }: {
  snapshot: Bootstrap;
  onSave: (preferences: Preferences) => void;
  onClose: () => void;
  onManageModels: () => void;
}) {
  const [preferences, setPreferences] = useState(snapshot.preferences);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const valid = validModelSettings(preferences.defaults);
  const changed = JSON.stringify(preferences) !== JSON.stringify(snapshot.preferences);
  // No model is loaded for this choice, so the general recommendation applies.
  const recommended = recommendedChatSettings().settings;
  const usingRecommended = matchesRecommended(preferences.defaults, recommended);
  const info = snapshot.inference.model;
  const active = snapshot.models.find((model) => model.id === snapshot.activeModelId) ?? null;
  const activeName = info ? modelLabel(info.fileName) : active ? modelLabel(active.fileName) : null;
  const guidance = capabilityGuidance(info);

  async function save(event: FormEvent) {
    event.preventDefault();
    if (saving || !valid) return;
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
    <Dialog title="Settings" onClose={onClose} busy={saving} size="md">
      <form className="dialog-form" onSubmit={(event) => void save(event)}>
        <section className="settings-section">
          <h3>Appearance</h3>
          <Segmented<Appearance> label="Appearance" value={preferences.appearance} disabled={saving}
            onChange={(appearance) => setPreferences({ ...preferences, appearance })}
            options={[
              { value: "system", label: "System", icon: <MonitorIcon size={14} /> },
              { value: "light", label: "Light", icon: <SunIcon size={14} /> },
              { value: "dark", label: "Dark", icon: <MoonIcon size={14} /> },
            ]} />
        </section>

        <section className="settings-section">
          <h3>Responses</h3>
          <Switch label="Enter sends a message" checked={preferences.enterToSend} disabled={saving}
            description={preferences.enterToSend ? "Shift+Enter adds a new line." : "Enter adds a new line. Use the send button to send."}
            onChange={(enterToSend) => setPreferences({ ...preferences, enterToSend })} />
          <div className="setting-row">
            <div>
              <strong>Settings for newly imported models</strong>
              <p className="hint">
                {usingRecommended ? "Recommended" : "Custom"}: {settingsSummary(preferences.defaults)}. Each model keeps its own
                settings; change those in Models.
              </p>
            </div>
            {usingRecommended ? (
              <span className="hint inline-ok"><CheckIcon size={13} />Recommended</span>
            ) : (
              <button type="button" className="secondary-button small" disabled={saving}
                title={settingsSummary(recommended)}
                onClick={() => setPreferences({ ...preferences, defaults: recommended })}>
                Use recommended
              </button>
            )}
          </div>
        </section>

        <section className="settings-section">
          <h3>Model</h3>
          <div className="setting-row">
            <div>
              <strong>{activeName ?? "No model loaded"}</strong>
              <p className="hint">
                {info ? modelProfile(info.fileName)?.role ?? "Loaded" : "Choose or import a model in Models."}
              </p>
            </div>
            <button type="button" className="secondary-button small" disabled={saving || changed}
              title={changed ? "Save or cancel your changes first" : undefined} onClick={onManageModels}>
              Manage models…
            </button>
          </div>
          {info && <Capabilities fileName={info.fileName} info={info} />}
          {guidance && <p className="hint">{guidance}</p>}
        </section>

        <section className="settings-section">
          <h3>Privacy and files</h3>
          <ul className="privacy-list">
            <li>Everything runs on this computer. Chat and Agent need no internet connection, and the app doesn't download models.</li>
            <li>Conversations, including your messages and the model's answers, are saved on this computer in an ordinary SQLite database. It isn't encrypted.</li>
            <li>The agent can read only files and folders you select. Access is read-only, expires automatically, and ends when the app closes.</li>
            <li>Saved answers can contain text or facts from files you selected. Raw file previews aren't saved; they stay in memory for recent turns only.</li>
            <li>Deleting a conversation removes its messages and Agent history. Content-free permission audit entries can remain, and existing backups or copies aren't affected.</li>
          </ul>
          <div className="data-location">
            <span>Database</span>
            <code>{snapshot.databasePath}</code>
          </div>
        </section>

        <section className="settings-section">
          <Advanced open={advancedOpen || !valid} onToggle={setAdvancedOpen}>
            <p className="hint">Defaults for newly imported models. Existing models keep their own saved settings.</p>
            <ModelFields settings={preferences.defaults} busy={saving}
              setSettings={(defaults) => setPreferences({ ...preferences, defaults })} />
            <OutputLimitExplanation />
          </Advanced>
        </section>

        <ModelLicense />

        {!valid && <p className="error-text">Set an output limit smaller than the context length, and keep every value in range.</p>}
        {error && <p className="error-text" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button type="button" className="ghost-button" onClick={onClose} disabled={saving}>Cancel</button>
          <button className="primary-button" type="submit" disabled={saving || !valid || !changed}>
            {saving ? "Saving…" : "Save settings"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

export type ModelAction = "import" | "load" | "unload" | "switch" | "save";

export function ModelsDialog({
  snapshot, selectedId, onSelect, action, operationLocked, error, notice, onImport, onLoad, onUnload, onSaveSettings, onClose,
  recommended, onReload, bundled,
}: {
  snapshot: Bootstrap;
  selectedId: number | null;
  onSelect: (id: number) => void;
  action: ModelAction | null;
  operationLocked: boolean;
  error: string | null;
  notice: string | null;
  onImport: () => void;
  onLoad: (reference: ModelReference, settings: ModelSettings) => void;
  onUnload: () => void;
  onSaveSettings: (reference: ModelReference, settings: ModelSettings) => void;
  onClose: () => void;
  recommended: RecommendedChatSettings | null;
  onReload: (reference: ModelReference, settings: ModelSettings) => void;
  bundled: BundledModel["state"] | "checking" | "unavailable";
}) {
  const selected = snapshot.models.find((model) => model.id === selectedId) ?? snapshot.models[0] ?? null;
  const [draft, setDraft] = useState<{ id: number; settings: ModelSettings } | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const settings = draft && selected && draft.id === selected.id ? draft.settings : selected?.settings ?? null;
  const info = snapshot.inference.model;
  const phase = snapshot.inference.phase;
  const loadedId = phase === "ready" || phase === "generating" ? snapshot.activeModelId : null;
  const selectedLoaded = selected !== null && selected.id === loadedId;
  const busy = operationLocked || action !== null || phase === "loading" || phase === "unloading" || phase === "generating";
  const valid = settings ? validModelSettings(settings) : false;
  const dirty = !!(settings && selected && JSON.stringify(settings) !== JSON.stringify(selected.settings));
  const setSettings = (next: ModelSettings) => selected && setDraft({ id: selected.id, settings: next });
  const profile = selected ? modelProfile(selected.fileName) : null;
  const selectedInfo = selectedLoaded ? info : null;
  const guidance = capabilityGuidance(selectedInfo);
  const hasBuiltIn = snapshot.models.some((model) => modelProfile(model.fileName)?.builtIn);

  return (
    <Dialog title="Models" size="lg" onClose={onClose} busy={action === "load" || action === "switch" || action === "unload"}
      description="Models run on this computer's processor. Nothing is sent online.">
      {operationLocked && <p className="hint">Model changes are available after the current response or task ends.</p>}
      <div className="models-layout">
        <div className="models-library">
          <div className="library-heading">
            <h3>Library</h3>
            <button type="button" className="secondary-button small" disabled={busy} onClick={onImport}>
              {action === "import" ? <Spinner size={13} /> : <UploadIcon size={14} />}Import model…
            </button>
          </div>
          {bundled === "corrupted" && (
            <Callout tone="error" title="Built-in assistant failed verification">
              Its file is damaged or was changed, so it wasn't added. Reinstall the app to restore it, or import a model.
            </Callout>
          )}
          {bundled === "checking" && !hasBuiltIn && (
            <p className="hint inline-status" role="status"><Spinner size={12} />Checking the built-in assistant…</p>
          )}
          {snapshot.models.length === 0 ? (
            <div className="library-empty">
              <CubeIcon size={22} />
              <p>
                {bundled === "checking" ? "No models yet."
                  : "No models yet. This copy of the app doesn't include the built-in assistant. Import a GGUF file to get started."}
              </p>
            </div>
          ) : (
            <ul className="library-list" aria-label="Your models">
              {snapshot.models.map((model) => {
                const loaded = model.id === loadedId;
                const tip = modelProfile(model.fileName);
                return (
                  <li key={model.id}>
                    <button type="button" className="library-item" aria-current={selected?.id === model.id ? "true" : undefined}
                      onClick={() => { onSelect(model.id); setDraft(null); }}>
                      <CubeIcon size={16} />
                      <span className="library-text">
                        <span className="library-name" title={model.fileName}>{modelLabel(model.fileName)}</span>
                        <small>{[loaded ? "Loaded" : null, tip?.role ?? (loaded ? null : "Not loaded")].filter(Boolean).join(" · ")}</small>
                      </span>
                      {loaded && <span className="status-dot status-ready" aria-label="Loaded" />}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          <div className="model-guidance">
            <h4>Which model?</h4>
            <ul>
              <li><strong>{builtInModelName}</strong><span>Included with the app. Quick, lightweight offline Chat. No Thinking or Agent.</span></li>
              <li><strong>{advancedModelName}</strong><span>For Agent tasks and Thinking. Larger and slower. Download the GGUF file yourself, then import it.</span></li>
            </ul>
          </div>
        </div>

        <div className="models-detail">
          {!selected || !settings ? (
            <p className="hint">Import a model to see its details and settings.</p>
          ) : (
            <>
              <div className="detail-heading">
                <div>
                  <h3 title={selected.fileName}>{modelLabel(selected.fileName)}</h3>
                  <p className="hint">{profile ? `${profile.role} · ${selected.fileName}` : selected.fileName}</p>
                </div>
                <div className="detail-actions">
                  {selectedLoaded ? (
                    <button type="button" className="secondary-button" disabled={busy} onClick={onUnload}>
                      {action === "unload" ? <Spinner size={13} /> : <PowerIcon size={14} />}
                      {action === "unload" ? "Unloading…" : "Unload"}
                    </button>
                  ) : (
                    <button type="button" className="primary-button" disabled={busy || !valid}
                      onClick={() => onLoad(selected, settings)}>
                      {action === "load" || action === "switch" ? <Spinner size={13} /> : null}
                      {action === "load" ? "Loading…" : action === "switch" ? "Switching…" : loadedId ? "Switch to this model" : "Load model"}
                    </button>
                  )}
                </div>
              </div>

              {/* Shown near the top so a failed action is visible without scrolling. */}
              {error && <Callout tone="error" role="alert" title="Model operation failed">{error}</Callout>}
              {notice && <p className="notice-text" role="status"><CheckIcon size={13} />{notice}</p>}

              <Capabilities fileName={selected.fileName} info={selectedInfo} />
              {guidance && <Callout tone="accent" title="Want Agent tasks or Thinking?">{guidance}</Callout>}

              {selectedLoaded && info ? (
                <dl className="model-facts">
                  <div><dt>Architecture</dt><dd>{info.architecture}</dd></div>
                  <div><dt>Parameters</dt><dd>{info.parameters >= 1e9 ? `${(info.parameters / 1e9).toFixed(1)} B` : `${Math.round(info.parameters / 1e6)} M`}</dd></div>
                  <div><dt>File size</dt><dd>{formatBytes(info.fileBytes)}</dd></div>
                  <div><dt>Loaded context</dt><dd>{info.config.contextLength.toLocaleString()} of {info.trainedContext.toLocaleString()} trained</dd></div>
                  <div><dt>Load time</dt><dd>{(info.loadMs / 1000).toFixed(1)} s</dd></div>
                </dl>
              ) : (
                <p className="hint model-unloaded">
                  Size, context and capabilities are read from the file when the model loads.
                  {loadedId !== null && " Switching unloads the current model first."}
                </p>
              )}

              {selectedLoaded && recommended ? (
                <RecommendedSettings saved={selected.settings} recommended={recommended} disabled={busy}
                  onReview={(next) => { setSettings(next); setAdvancedOpen(true); }}
                  onApply={(next) => { setDraft(null); onSaveSettings(selected, next); }}
                  onReload={(next) => { setDraft(null); onReload(selected, next); }} />
              ) : !selectedLoaded && (
                <p className="hint">Load this model to see its recommended settings.</p>
              )}

              <Advanced open={advancedOpen || dirty} onToggle={setAdvancedOpen}>
                <p className="hint">Saved for {modelLabel(selected.fileName)} only. {settingsSummary(settings)}.</p>
                <ModelFields settings={settings} setSettings={setSettings} busy={busy} loaded={selectedLoaded}
                  supportsThinking={selectedInfo ? selectedInfo.supportsThinking : profile?.thinking ?? true} />
                {!valid && <p className="error-text">Keep the output limit below the context length and every value in range.</p>}
                {selectedLoaded && info && (settings.contextLength !== info.config.contextLength || settings.threads !== info.config.threads) && (
                  <p className="hint">
                    The model is running with a {info.config.contextLength.toLocaleString()}-token context and {info.config.threads} threads.
                    These values apply after it reloads.
                    {valid && (
                      <> <button type="button" className="link-button" disabled={busy}
                        onClick={() => { setDraft(null); onReload(selected, settings); }}>Save and reload now</button></>
                    )}
                  </p>
                )}
                <OutputLimitExplanation />
                <div className="dialog-actions">
                  {dirty && <button type="button" className="ghost-button" disabled={busy} onClick={() => setDraft(null)}>Discard changes</button>}
                  <button type="button" className="secondary-button" disabled={busy || !valid || !dirty}
                    onClick={() => { onSaveSettings(selected, settings); }}>
                    {action === "save" ? "Saving…" : "Save settings"}
                  </button>
                </div>
              </Advanced>

              {profile?.builtIn && <ModelLicense />}
            </>
          )}
        </div>
      </div>
    </Dialog>
  );
}

export function AccessDialog({
  scopes, remaining, action, error, disabled, onGrant, onRevoke, onClose,
}: {
  scopes: AgentScopeDisplay[];
  remaining: (scope: AgentScopeDisplay) => number;
  action: string | null;
  error: string | null;
  disabled: boolean;
  onGrant: (kind: "file" | "directory") => void;
  onRevoke: (scope: AgentScopeDisplay) => void;
  onClose: () => void;
}) {
  const ordered = [...scopes].sort((a, b) => Number(b.state === "active") - Number(a.state === "active"));
  return (
    <Dialog title="File access" size="sm" onClose={onClose}
      description="The agent can only read files and folders you select here. Access is read-only, expires automatically and ends when the app closes. Earlier messages never grant access, and answers saved in conversations can include information from selected files.">
      <div className="dialog-form">
        <div className="grant-actions">
          <button type="button" className="secondary-button" disabled={disabled || action !== null} onClick={() => onGrant("file")}>
            {action === "file" ? <Spinner size={13} /> : <FileIcon size={14} />}Select a file…
          </button>
          <button type="button" className="secondary-button" disabled={disabled || action !== null} onClick={() => onGrant("directory")}>
            {action === "directory" ? <Spinner size={13} /> : <FolderIcon size={14} />}Select a folder…
          </button>
        </div>
        {disabled && <p className="hint">Selection is available when no response or task is running.</p>}
        {error && <p className="error-text" role="alert">{error}</p>}
        {ordered.length === 0 ? (
          <p className="empty-line">No files or folders selected in this session.</p>
        ) : (
          <ul className="grant-list" aria-label="Selected files and folders">
            {ordered.map((scope) => {
              const left = remaining(scope);
              return (
                <li key={scope.id} className={`grant grant-${scope.state}`}>
                  {scope.kind === "directory" ? <FolderIcon size={16} /> : <FileIcon size={16} />}
                  <div className="grant-text">
                    <strong title={scope.displayName}>{scope.displayName}</strong>
                    <small>
                      <LockIcon size={11} />{scope.kind === "directory" ? "Folder" : "File"} · Read-only ·{" "}
                      {scope.state === "active" ? remainingLabel(left) : scope.state === "revoked" ? "Revoked" : "Expired"}
                    </small>
                  </div>
                  {scope.state === "active" && (
                    <button type="button" className="ghost-button small danger-text" disabled={action !== null}
                      onClick={() => onRevoke(scope)}>
                      {action === scope.id ? "Revoking…" : "Revoke"}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </Dialog>
  );
}

export function EditConversationDialog({
  kind, conversation, onSubmit, onClose, busy, error,
}: {
  kind: "rename" | "delete" | "instructions";
  conversation: { title: string; systemPrompt: string };
  onSubmit: (value: string) => void;
  onClose: () => void;
  busy: boolean;
  error: string | null;
}) {
  const [value, setValue] = useState(kind === "rename" ? conversation.title : conversation.systemPrompt);
  const title = kind === "rename" ? "Rename conversation" : kind === "delete" ? "Delete conversation?" : "Conversation instructions";

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!busy && (kind !== "rename" || value.trim())) onSubmit(value);
  }

  return (
    <Dialog title={title} onClose={onClose} busy={busy} size="sm">
      <form className="dialog-form" onSubmit={submit}>
        {kind === "rename" && (
          <label className="field">
            <span>Title</span>
            <input value={value} maxLength={100} required disabled={busy} data-autofocus
              onChange={(event) => setValue(event.currentTarget.value)} />
          </label>
        )}
        {kind === "instructions" && (
          <label className="field">
            <span>System instructions</span>
            <textarea rows={6} maxLength={8192} value={value} disabled={busy} data-autofocus
              onChange={(event) => setValue(event.currentTarget.value)} />
            <small className="hint">Applies to future replies in this conversation.</small>
          </label>
        )}
        {kind === "delete" && (
          <>
            <p><strong>{conversation.title}</strong> will be deleted from this computer's database, including its messages,
              Agent turns and their execution history.</p>
            <p className="hint">Content-free permission audit entries can remain for record integrity. Existing backups or
              copies of the database aren't changed.</p>
          </>
        )}
        {error && <p className="error-text" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button type="button" className="ghost-button" onClick={onClose} disabled={busy}>Cancel</button>
          <button className={kind === "delete" ? "danger-button" : "primary-button"} type="submit"
            disabled={busy || (kind === "rename" && !value.trim())}>
            {kind === "rename" ? "Save" : kind === "delete" ? "Delete" : "Save instructions"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
