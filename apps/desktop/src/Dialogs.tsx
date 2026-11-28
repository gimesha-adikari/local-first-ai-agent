import { useState, type FormEvent, type ReactNode } from "react";
import lfmLicense from "../../../third_party/model-licenses/LFM2.5-1.2B-Instruct/LICENSE?raw";
import { formatBytes, remainingLabel } from "./agent";
import { curatedArtifacts } from "./models";
import type { AgentScopeDisplay } from "./agentTypes";
import {
  lowOutputLimit, settingChanges, shortOutputLimit, validModelSettings,
  type Bootstrap, type BundledModel, type ModelReference, type ModelSettings, type RecommendedChatSettings,
} from "./chat";
import {
  AgentIcon, ChatIcon, CheckIcon, ChevronRightIcon, CloseIcon, CubeIcon, FileIcon, FolderIcon, LockIcon,
  PowerIcon, Spinner, ThinkIcon,
} from "./icons";
import type { ModelInfo } from "./inference";
import {
  builtInModelName, capabilityGuidance, modelCapabilities, modelLabel, modelOrigin, referenceProfile,
} from "./models";
import { Callout, Dialog, Switch } from "./ui";

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
            <input type="number" min={min} max={max} step={step} disabled={busy}
              value={Number.isNaN(settings[key]) ? "" : settings[key]}
              onChange={(event) => setSettings({ ...settings, [key]: event.currentTarget.valueAsNumber })} />
            <small className="hint">{loaded && reload ? `${hint} Applies when the model reloads.` : hint}</small>
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
export function settingsSummary(settings: ModelSettings) {
  return `Replies up to ${settings.maxTokens.toLocaleString()} tokens · ${settings.contextLength.toLocaleString()}-token `
    + `total context · Thinking ${settings.enableThinking ? "on" : "off"}`;
}

/** Collapsed section for numeric model controls. */
export function Advanced({ open, onToggle, children }: { open: boolean; onToggle: (open: boolean) => void; children: ReactNode }) {
  return (
    <details className="advanced-settings" open={open} onToggle={(event) => onToggle(event.currentTarget.open)}>
      <summary><ChevronRightIcon size={14} />Advanced settings</summary>
      <div className="advanced-body">{children}</div>
    </details>
  );
}

export function OutputLimitExplanation() {
  return (
    <p className="hint">
      Instructions, your input, conversation history and generated output all share the context. The output limit is a
      ceiling. For each reply the app measures the prompt with the model's tokenizer, reserves about a
      quarter of the context for output (a third with Thinking), fits recent history, then allows up to this limit in the
      remaining space. Thinking and the answer share it. A reply shows the effective limit when less was available.
    </p>
  );
}

export function Capabilities({ fileName, info }: { fileName: string; info: ModelInfo | null }) {
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
        LFM2.5-1.2B-Instruct by Liquid AI, Inc. is licensed under the LFM Open License v1.0. The app no longer ships it; earlier
        packages did, and you can import the same file as a checksum-verified local model. That license covers only the model
        file, not this app.
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

export type ModelAction = "import" | "load" | "unload" | "switch" | "save" | "reset";

/** Loaded-model summary shown above the library. */
function LoadedModelBanner({ snapshot }: { snapshot: Bootstrap }) {
  const info = snapshot.inference.model;
  const phase = snapshot.inference.phase;
  const reference = snapshot.models.find((model) => model.id === snapshot.activeModelId) ?? null;
  if (phase === "loading" || phase === "unloading") {
    return (
      <div className="loaded-banner" role="status">
        <Spinner size={13} /><span><strong>{phase === "loading" ? "Loading a model…" : "Unloading…"}</strong></span>
      </div>
    );
  }
  if (!info) {
    return (
      <div className="loaded-banner is-empty" role="status">
        <span className="status-dot" aria-hidden="true" />
        <span><strong>No model loaded</strong><small>Choose a model below and load it to chat.</small></span>
      </div>
    );
  }
  return (
    <div className="loaded-banner" role="status" aria-label="Loaded model">
      <span className="status-dot status-ready" aria-hidden="true" />
      <span>
        <strong>Loaded now: {modelLabel(info.fileName)}</strong>
        <small>
          {[reference ? modelOrigin(reference) : null,
            reference ? (reference.automaticSettings ? "Automatic settings" : "Custom settings") : null,
            `${info.config.contextLength.toLocaleString()}-token context`, `${info.config.threads} threads`,
          ].filter(Boolean).join(" · ")}
        </small>
      </span>
    </div>
  );
}

/** Curated manifest identity: a file is verified only by exact size and SHA-256. */
function verificationBadge(model: ModelReference) {
  if (model.available === false) return null;
  const artifact = model.artifact ? curatedArtifacts[model.artifact] : null;
  if (artifact) {
    return <span className="badge badge-verified" title={`${artifact.name} · ${artifact.license} · checksum verified`}>Verified</span>;
  }
  if (model.bundled) return null;
  return <span className="badge badge-muted" title="This file is not in the curated manifest. It was not verified.">Unverified</span>;
}

function ModelBadges({ model }: { model: ModelReference }) {
  return (
    <span className="model-badges">
      <span className={`badge ${model.bundled ? "badge-verified" : "badge-muted"}`}>{modelOrigin(model)}</span>
      {verificationBadge(model)}
      <span className={`badge ${model.automaticSettings ? "badge-ready" : "badge-warning"}`}>{model.automaticSettings ? "Automatic" : "Custom"}</span>
    </span>
  );
}

/**
 * The model library and per-model settings, shown as the Models category of
 * Settings. Drafts are owned by the caller so they survive category switches.
 */
export function ModelsPanel({
  snapshot, selectedId, onSelect, action, operationLocked, error, notice, onLoad, onUnload, onSaveSettings,
  recommended, onReload, bundled, onResetSettings, drafts, onDraft,
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
  recommended: RecommendedChatSettings | null;
  onReload: (reference: ModelReference, settings: ModelSettings) => void;
  bundled: BundledModel["state"] | "checking" | "unavailable";
  /** Return to automatic settings; `reload` also reloads the loaded model so context and threads apply. */
  onResetSettings: (reference: ModelReference, reload: boolean) => void;
  drafts: Readonly<Record<number, ModelSettings>>;
  onDraft: (id: number, settings: ModelSettings | null) => void;
}) {
  const selected = snapshot.models.find((model) => model.id === selectedId) ?? snapshot.models[0] ?? null;
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const settings = selected ? drafts[selected.id] ?? selected.settings : null;
  const info = snapshot.inference.model;
  const phase = snapshot.inference.phase;
  const loadedId = phase === "ready" || phase === "generating" ? snapshot.activeModelId : null;
  const selectedLoaded = selected !== null && selected.id === loadedId;
  const busy = operationLocked || action !== null || phase === "loading" || phase === "unloading" || phase === "generating";
  const valid = settings ? validModelSettings(settings) : false;
  const dirty = !!(settings && selected && JSON.stringify(settings) !== JSON.stringify(selected.settings));
  const setSettings = (next: ModelSettings) => selected && onDraft(selected.id, next);
  const discard = () => selected && onDraft(selected.id, null);
  const profile = selected ? referenceProfile(selected) : null;
  const selectedInfo = selectedLoaded ? info : null;
  const guidance = capabilityGuidance(selectedInfo);
  const hasBuiltIn = snapshot.models.some((model) => model.bundled);
  // Context and threads of a loaded model change only when it loads again.
  const pendingReload = !!(selectedLoaded && info && settings
    && (settings.contextLength !== info.config.contextLength || settings.threads !== info.config.threads));
  const savedPendingReload = !!(selectedLoaded && info && selected
    && (selected.settings.contextLength !== info.config.contextLength || selected.settings.threads !== info.config.threads));
  const recommendationDiffers = !!(selected && !selected.automaticSettings && selectedLoaded && recommended
    && settingChanges(selected.settings, recommended.settings).length > 0);

  return (
    <div className="models-panel">
      <LoadedModelBanner snapshot={snapshot} />
      {operationLocked && <p className="hint">Model changes are available after the current response or task ends.</p>}
      <div className="models-layout">
        <div className="models-library">
          <div className="library-heading">
            <h3>Chat models</h3>
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
                No Chat models in this library.
              </p>
            </div>
          ) : (
            <ul className="library-list" aria-label="Your models">
              {snapshot.models.map((model) => {
                const loaded = model.id === loadedId;
                const unsaved = !!drafts[model.id] && JSON.stringify(drafts[model.id]) !== JSON.stringify(model.settings);
                return (
                  <li key={model.id}>
                    <button type="button" className="library-item" aria-current={selected?.id === model.id ? "true" : undefined}
                      onClick={() => onSelect(model.id)}>
                      <CubeIcon size={16} />
                      <span className="library-text">
                        <span className="library-name" title={model.fileName}>{modelLabel(model.fileName)}</span>
                        <small>
                          {[loaded ? "Loaded" : null, modelOrigin(model),
                            model.available === false ? "File unavailable" : null,
                            model.automaticSettings ? "Automatic" : "Custom", unsaved ? "Unsaved changes" : null,
                          ].filter(Boolean).join(" · ")}
                        </small>
                      </span>
                      {loaded && <span className="status-dot status-ready" aria-label="Loaded" />}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="models-detail">
          {!selected || !settings ? (
            <p className="hint">Import a model to see its details and settings.</p>
          ) : (
            <>
              <div className="detail-heading">
                <div>
                  <h3 title={selected.fileName}>{modelLabel(selected.fileName)}</h3>
                  <ModelBadges model={selected} />
                  <p className="hint">{profile ? `${profile.role} · ${selected.fileName}` : selected.fileName}</p>
                </div>
                <div className="detail-actions">
                  {selectedLoaded ? (
                    <button type="button" className="secondary-button" disabled={busy} onClick={onUnload}>
                      {action === "unload" ? <Spinner size={13} /> : <PowerIcon size={14} />}
                      {action === "unload" ? "Unloading…" : "Unload"}
                    </button>
                  ) : (
                    <button type="button" className="primary-button" disabled={busy || !valid || selected.available === false}
                      onClick={() => onLoad(selected, settings)}>
                      {action === "load" || action === "switch" ? <Spinner size={13} /> : null}
                      {action === "load" ? "Loading…" : action === "switch" ? "Switching…" : loadedId ? "Switch to this model" : "Load model"}
                    </button>
                  )}
                </div>
              </div>

              {selected.available === false && <p className="hint">This file is unavailable. Reconnect its drive, import an available copy or use Discover.</p>}

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

              <section className={`settings-mode${selected.automaticSettings ? " is-automatic" : ""}`} aria-label="Configuration">
                <div className="section-heading-row">
                  <h4>{selected.automaticSettings ? "Automatic configuration" : "Custom configuration"}</h4>
                  {selected.automaticSettings && <span className="hint inline-ok"><CheckIcon size={13} />Recommended</span>}
                </div>
                <p className="hint">
                  {selected.automaticSettings
                    ? `Recommended settings for this model and computer are applied every time it loads. Nothing to do. ${settingsSummary(selected.settings)}.`
                    : `Your saved settings are used exactly as set. ${settingsSummary(selected.settings)}.`}
                </p>
                {recommendationDiffers && recommended && (
                  <p className="hint">
                    The current recommendation differs: {settingChanges(selected.settings, recommended.settings)
                      .map((change) => `${change.label} ${change.from} → ${change.to}`).join(" · ")}.
                  </p>
                )}
                {!selected.automaticSettings && (
                  <div className="recommended-actions">
                    {selectedLoaded ? (
                      <>
                        <button type="button" className="primary-button small" disabled={busy}
                          onClick={() => { discard(); onResetSettings(selected, true); }}>
                          {action === "reset" ? <Spinner size={13} /> : null}Reset to automatic and reload
                        </button>
                        <button type="button" className="ghost-button small" disabled={busy}
                          onClick={() => { discard(); onResetSettings(selected, false); }}>
                          Reset without reloading
                        </button>
                      </>
                    ) : (
                      <button type="button" className="primary-button small" disabled={busy}
                        onClick={() => { discard(); onResetSettings(selected, false); }}>
                        {action === "reset" ? <Spinner size={13} /> : null}Reset to automatic
                      </button>
                    )}
                  </div>
                )}
                {savedPendingReload && !dirty && info && (
                  <p className="hint reload-note">
                    The model is running with a {info.config.contextLength.toLocaleString()}-token context and {info.config.threads} threads.
                    The saved context and threads apply after it reloads.{" "}
                    <button type="button" className="link-button" disabled={busy}
                      onClick={() => onLoad(selected, selected.settings)}>Reload now</button>
                  </p>
                )}
              </section>

              <Advanced open={advancedOpen || dirty} onToggle={setAdvancedOpen}>
                <p className="hint">
                  Saved for {modelLabel(selected.fileName)} only.
                  {selected.automaticSettings ? " Saving a change switches this model to custom settings." : ""}
                </p>
                <ModelFields settings={settings} setSettings={setSettings} busy={busy} loaded={selectedLoaded}
                  supportsThinking={selectedInfo ? selectedInfo.supportsThinking : profile?.thinking ?? true} />
                {!valid && <p className="error-text">Keep the output limit below the context length and every value in range.</p>}
                {pendingReload && dirty && info && (
                  <p className="hint reload-note">
                    Context length and CPU threads apply only when the model loads, so saving these reloads it.
                  </p>
                )}
                <OutputLimitExplanation />
                <div className="dialog-actions">
                  {dirty && <button type="button" className="ghost-button" disabled={busy} onClick={discard}>Discard changes</button>}
                  {pendingReload && dirty ? (
                    <button type="button" className="primary-button" disabled={busy || !valid}
                      onClick={() => { discard(); onReload(selected, settings); }}>Save and reload</button>
                  ) : (
                    <button type="button" className="secondary-button" disabled={busy || !valid || !dirty}
                      onClick={() => { discard(); onSaveSettings(selected, settings); }}>
                      {action === "save" ? "Saving…" : "Save as custom settings"}
                    </button>
                  )}
                </div>
              </Advanced>
            </>
          )}
        </div>
      </div>
    </div>
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
