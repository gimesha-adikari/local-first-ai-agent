import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useMemo, useState, type ReactNode } from "react";
import packageJson from "../package.json?raw";
import softwareNotice from "../../../third_party/software-notices/NOTICE?raw";
import type { Appearance, Bootstrap, ModelSettings, Preferences } from "./chat";
import {
  Advanced, Capabilities, ModelLicense, ModelsPanel, OutputLimitExplanation, type ModelAction,
} from "./Dialogs";
import {
  BookIcon, ChatIcon, ChevronRightIcon, CloudIcon, CpuIcon, CubeIcon, GearIcon, GlobeIcon, InfoIcon, MemoryIcon, MonitorIcon, MoonIcon,
  PowerIcon, ShieldIcon, Spinner, SunIcon,
} from "./icons";
import { errorMessage } from "./inference";
import { embeddingCopy } from "./knowledge";
import { builtInModelName, capabilityGuidance, modelLabel, modelOrigin, referenceProfile } from "./models";
import { Callout, Dialog, Segmented, Switch } from "./ui";
import { ModelManager } from "./ModelManager";
import { ProvidersPanel } from "./ProvidersPanel";
import type { IntelligenceSettings, ProviderView } from "./providers";
import type { WebResearchView } from "./research";
import { WebSearchPanel } from "./WebSearchPanel";
import type { KnowledgeController } from "./useKnowledge";

export type SettingsCategory = "general" | "chat" | "models" | "providers" | "web" | "knowledge" | "privacy" | "about";

const categories: { key: SettingsCategory; label: string; description: string; icon: ReactNode }[] = [
  { key: "general", label: "General", description: "Appearance, typing and the desktop shortcut.", icon: <GearIcon size={15} /> },
  { key: "chat", label: "Chat & Agent", description: "The loaded model's abilities, replies and file access.", icon: <ChatIcon size={15} /> },
  { key: "models", label: "Models", description: "Find local models, manage downloads and choose what runs on this computer.", icon: <CubeIcon size={15} /> },
  { key: "providers", label: "Providers", description: "Optional cloud and self-hosted intelligence, with what may be shared and monthly limits.", icon: <CloudIcon size={15} /> },
  { key: "web", label: "Web search", description: "Search providers for web research in Chat, and what is sent and saved.", icon: <GlobeIcon size={15} /> },
  { key: "knowledge", label: "Knowledge & Memory", description: "Semantic search, document context and saved memories.", icon: <BookIcon size={15} /> },
  { key: "privacy", label: "Privacy & Storage", description: "What is stored on this computer, and where.", icon: <ShieldIcon size={15} /> },
  { key: "about", label: "About & Licenses", description: "Version, model licenses and notices.", icon: <InfoIcon size={15} /> },
];

const appVersion = (() => {
  try {
    return (JSON.parse(packageJson) as { version?: string }).version ?? null;
  } catch {
    return null;
  }
})();

/** Everything the Models category needs, except the drafts this dialog owns. */
export type ModelsPanelProps = Omit<Parameters<typeof ModelsPanel>[0], "drafts" | "onDraft">;

export function SettingsDialog({
  snapshot, category, onCategory, onSavePreferences, onClose, models, modelAction, knowledge, knowledgeLock,
  memoryEnabled, onMemoryEnabledChange, onOpenKnowledge, onOpenMemory, onManageAccess, providers, webResearch,
}: {
  snapshot: Bootstrap;
  category: SettingsCategory;
  onCategory: (category: SettingsCategory) => void;
  onSavePreferences: (preferences: Preferences) => void;
  onClose: () => void;
  models: ModelsPanelProps;
  modelAction: ModelAction | null;
  knowledge: KnowledgeController;
  knowledgeLock: string | null;
  memoryEnabled: boolean | null;
  onMemoryEnabledChange: (enabled: boolean) => Promise<void>;
  onOpenKnowledge: () => void;
  onOpenMemory: () => void;
  onManageAccess: () => void;
  providers: { list: ProviderView[]; intelligence: IntelligenceSettings; busy: boolean; onChanged: () => Promise<void> };
  webResearch: { view: WebResearchView | null; busy: boolean; onChanged: (view: WebResearchView) => void };
}) {
  // Drafts live here so switching categories never discards them.
  const [preferences, setPreferences] = useState(snapshot.preferences);
  const [modelDrafts, setModelDrafts] = useState<Record<number, ModelSettings>>({});
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const panelId = useId();
  const preferencesDirty = JSON.stringify(preferences) !== JSON.stringify(snapshot.preferences);
  const modelsDirty = useMemo(() => Object.entries(modelDrafts).some(([id, draft]) => {
    const saved = snapshot.models.find((model) => model.id === Number(id));
    return !!saved && JSON.stringify(saved.settings) !== JSON.stringify(draft);
  }), [modelDrafts, snapshot.models]);
  const dirty: Partial<Record<SettingsCategory, boolean>> = { general: preferencesDirty, models: modelsDirty };
  const current = categories.find(({ key }) => key === category) ?? categories[0];
  const modelBusy = modelAction === "load" || modelAction === "switch" || modelAction === "unload" || modelAction === "reset";

  async function savePreferences() {
    if (saving) return;
    setError(null);
    setSaving(true);
    try {
      await invoke("save_preferences", { preferences });
      onSavePreferences(preferences);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog title="Settings" size="xl" onClose={onClose} busy={saving || modelBusy}>
      <div className="settings-layout">
        <nav className="settings-nav" aria-label="Settings categories">
          {categories.map(({ key, label, icon }) => (
            <button key={key} type="button" className="settings-nav-item" aria-current={key === category ? "page" : undefined}
              aria-controls={panelId} onClick={() => onCategory(key)}>
              {icon}<span>{label}</span>
              {dirty[key] && <span className="unsaved-dot" aria-label="Unsaved changes" />}
            </button>
          ))}
        </nav>
        <section className="settings-panel" id={panelId} aria-label={current.label}>
          <header className="settings-panel-heading">
            <h3>{current.label}</h3>
            <p className="hint">{current.description}</p>
          </header>
          {category === "general" && <GeneralPanel preferences={preferences} disabled={saving} onChange={setPreferences} />}
          {category === "chat" && <ChatPanel snapshot={snapshot} onManageAccess={onManageAccess} onModels={() => onCategory("models")} />}
          {category === "models" && (
            <ModelManager initialInstalled={snapshot.models.some((model) => model.available !== false)}
              operationsBusy={models.operationLocked || modelAction !== null || knowledge.holdsLease}
              onSelectModel={models.onSelect} onImportChat={models.onImport}
              onImportEmbedding={knowledge.chooseModel} onUnloadEmbedding={knowledge.unloadModel}
              onEmbeddingChanged={knowledge.refreshEmbedding} embeddingNotice={knowledge.notice}>
            <ModelsPanel {...models} drafts={modelDrafts}
              onDraft={(id, draft) => setModelDrafts((drafts) => {
                const next = { ...drafts };
                if (draft) next[id] = draft; else delete next[id];
                return next;
              })} />
            </ModelManager>
          )}
          {category === "providers" && (
            <ProvidersPanel providers={providers.list} intelligence={providers.intelligence} busy={providers.busy}
              onChanged={providers.onChanged} />
          )}
          {category === "web" && (
            <WebSearchPanel view={webResearch.view} busy={webResearch.busy} onChanged={webResearch.onChanged} />
          )}
          {category === "knowledge" && (
            <KnowledgePanel knowledge={knowledge} lockReason={knowledgeLock} memoryEnabled={memoryEnabled}
              onMemoryEnabledChange={onMemoryEnabledChange} onOpenKnowledge={onOpenKnowledge} onOpenMemory={onOpenMemory} />
          )}
          {category === "privacy" && <PrivacyPanel snapshot={snapshot} />}
          {category === "about" && <AboutPanel />}
        </section>
      </div>
      {(preferencesDirty || error) && (
        <div className="settings-savebar" role="status">
          {error ? <p className="error-text" role="alert">{error}</p> : <p className="hint">You have unsaved General settings.</p>}
          <div className="dialog-actions">
            <button type="button" className="ghost-button" disabled={saving || !preferencesDirty}
              onClick={() => { setPreferences(snapshot.preferences); setError(null); }}>Discard</button>
            <button type="button" className="primary-button" disabled={saving || !preferencesDirty} onClick={() => void savePreferences()}>
              {saving ? "Saving…" : "Save changes"}
            </button>
          </div>
        </div>
      )}
    </Dialog>
  );
}

function GeneralPanel({ preferences, disabled, onChange }: {
  preferences: Preferences;
  disabled: boolean;
  onChange: (preferences: Preferences) => void;
}) {
  return (
    <div className="settings-stack">
      <section className="settings-section">
        <h4>Appearance</h4>
        <Segmented<Appearance> label="Appearance" value={preferences.appearance} disabled={disabled}
          onChange={(appearance) => onChange({ ...preferences, appearance })}
          options={[
            { value: "system", label: "System", icon: <MonitorIcon size={14} /> },
            { value: "light", label: "Light", icon: <SunIcon size={14} /> },
            { value: "dark", label: "Dark", icon: <MoonIcon size={14} /> },
          ]} />
        <p className="hint">Light uses warm paper tones; Dark uses graphite. System follows your desktop.</p>
      </section>
      <section className="settings-section">
        <h4>Typing</h4>
        <Switch label="Enter sends a message" checked={preferences.enterToSend} disabled={disabled}
          description={preferences.enterToSend ? "Shift+Enter adds a new line." : "Enter adds a new line. Use the send button to send."}
          onChange={(enterToSend) => onChange({ ...preferences, enterToSend })} />
      </section>
      <ShortcutSettings />
    </div>
  );
}

type ShortcutStatus = {
  state: "available" | "configuring" | "active" | "denied" | "unavailable";
  trigger: string | null;
  canReconfigure: boolean;
};

function ShortcutSettings() {
  const [status, setStatus] = useState<ShortcutStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const fallback: ShortcutStatus = { state: "unavailable", trigger: null, canReconfigure: false };

  useEffect(() => {
    let mounted = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const refresh = async () => {
      const next = await invoke<ShortcutStatus>("shortcut_status").catch(() => null);
      if (!mounted) return;
      setStatus(next ?? { state: "unavailable", trigger: null, canReconfigure: false });
      if (next?.state === "configuring") timer = setTimeout(() => void refresh(), 500);
    };
    void refresh();
    window.addEventListener("focus", refresh);
    return () => { mounted = false; clearTimeout(timer); window.removeEventListener("focus", refresh); };
  }, []);

  async function configure() {
    setBusy(true);
    try {
      setStatus(await invoke<ShortcutStatus>("configure_shortcut"));
    } catch {
      setStatus(fallback);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section">
      <h4>Desktop shortcut</h4>
      <p className="hint" role="status">{status?.state === "active"
        ? `Press ${status.trigger} to open the contextual panel.`
        : status?.state === "unavailable" ? "The desktop shortcut portal is unavailable."
          : status?.state === "denied" ? "Shortcut permission was declined."
            : "Choose a shortcut to open the contextual panel from another app."}</p>
      {status?.state === "active" && !status.canReconfigure
        ? <p className="hint">Change this shortcut in your desktop’s keyboard settings.</p>
        : <button type="button" className="secondary-button small" disabled={busy || !status || status.state === "configuring"}
            onClick={() => void configure()}>{busy ? "Opening…" : "Configure desktop shortcut…"}</button>}
      <p className="hint">For a custom keyboard shortcut, use <code>local-first-ai-agent --summon</code>.</p>
    </section>
  );
}

function ChatPanel({ snapshot, onManageAccess, onModels }: { snapshot: Bootstrap; onManageAccess: () => void; onModels: () => void }) {
  const info = snapshot.inference.model;
  const reference = snapshot.models.find((model) => model.id === snapshot.activeModelId) ?? null;
  const guidance = capabilityGuidance(info);
  const [open, setOpen] = useState(false);
  return (
    <div className="settings-stack">
      <section className="settings-section">
        <h4>Loaded model</h4>
        <div className="setting-row">
          <div>
            <strong>{info ? modelLabel(info.fileName) : "No model loaded"}</strong>
            <p className="hint">
              {info && reference ? `${referenceProfile(reference)?.role ?? modelOrigin(reference)} · ${
                reference.automaticSettings ? "automatic" : "custom"} settings`
                : "Load a model in Models to chat."}
            </p>
          </div>
          <button type="button" className="secondary-button small" onClick={onModels}>Open Models</button>
        </div>
        {info && <Capabilities fileName={info.fileName} info={info} />}
        {guidance && <p className="hint">{guidance}</p>}
      </section>
      <section className="settings-section">
        <h4>Replies</h4>
        <p className="hint">
          Turn Thinking on or off per message with the composer's Thinking control. Each model keeps its own output limit
          and context in Models; automatic models get recommended values when they load.
        </p>
        <Advanced open={open} onToggle={setOpen}><OutputLimitExplanation /></Advanced>
      </section>
      <section className="settings-section">
        <h4>Agent file access</h4>
        <div className="setting-row">
          <p className="hint">
            The agent reads only files and folders you select. Access is read-only, expires automatically, and ends when the app closes.
          </p>
          <button type="button" className="secondary-button small" onClick={onManageAccess}>Manage file access…</button>
        </div>
      </section>
    </div>
  );
}

function KnowledgePanel({
  knowledge, lockReason, memoryEnabled, onMemoryEnabledChange, onOpenKnowledge, onOpenMemory,
}: {
  knowledge: KnowledgeController;
  lockReason: string | null;
  memoryEnabled: boolean | null;
  onMemoryEnabledChange: (enabled: boolean) => Promise<void>;
  onOpenKnowledge: () => void;
  onOpenMemory: () => void;
}) {
  const [memoryBusy, setMemoryBusy] = useState(false);
  return (
    <div className="settings-stack">
      <section className="settings-section">
        <h4>Semantic search model</h4>
        {knowledge.notice && (
          <Callout tone={knowledge.notice.tone} role={knowledge.notice.tone === "error" ? "alert" : "status"} title={knowledge.notice.title}
            actions={<button type="button" className="link-button" onClick={() => knowledge.setNotice(null)}>Dismiss</button>}>
            {knowledge.notice.detail}
          </Callout>
        )}
        <EmbeddingModelCard knowledge={knowledge} lockReason={lockReason} />
      </section>
      <section className="settings-section">
        <h4>Documents in Chat</h4>
        <p className="hint">
          Turn on Knowledge in the composer and select documents. <strong>Automatic</strong> keeps them available: exact searches,
          overviews and questions use the documents, and unrelated requests get an ordinary answer that says no document evidence was used.
          <strong> Documents only</strong> grounds every answer in the selection.
        </p>
        <div className="setting-row">
          <p className="hint">{knowledge.documents.length
            ? `${knowledge.documents.length} ${knowledge.documents.length === 1 ? "document" : "documents"} in your library.`
            : "Your library is empty."}</p>
          <button type="button" className="secondary-button small" onClick={onOpenKnowledge}>Open Knowledge library</button>
        </div>
      </section>
      <section className="settings-section">
        <h4>Personal memory</h4>
        <Switch label="Use saved memories in Chat" checked={!!memoryEnabled} disabled={memoryEnabled === null || memoryBusy}
          description="Up to three relevant memories are supplied as background. Memories are saved only when you add them."
          onChange={(enabled) => {
            setMemoryBusy(true);
            void onMemoryEnabledChange(enabled).finally(() => setMemoryBusy(false));
          }} />
        <div className="setting-row">
          <p className="hint">Add, edit and delete memories in Memory.</p>
          <button type="button" className="secondary-button small" onClick={onOpenMemory}><MemoryIcon size={14} />Manage memories</button>
        </div>
      </section>
    </div>
  );
}

/** Status and actions for the semantic search model, shared with the Knowledge library. */
export function EmbeddingModelCard({ knowledge, lockReason }: { knowledge: KnowledgeController; lockReason: string | null }) {
  const copy = embeddingCopy(knowledge.embedding, knowledge.modelAction);
  const counts = Object.values(knowledge.indexStates);
  const indexed = counts.filter((value) => value === "indexed").length;
  const outdated = counts.filter((value) => value === "incompatible").length;
  const total = knowledge.documents.length;
  const blocked = knowledge.modelAction !== null || !!lockReason;
  return (
    <div className={`embedding-card tone-${copy.tone}`} aria-label="Semantic search model">
      <div className="embedding-heading">
        <span className={`status-dot${copy.tone === "ready" ? " status-ready" : copy.tone === "busy" ? " status-loading"
          : copy.tone === "warning" ? " status-warning" : copy.tone === "error" ? " status-error" : ""}`} aria-hidden="true" />
        <div className="embedding-text">
          <strong>BGE small, English</strong>
          <small role="status">{copy.label}</small>
        </div>
        {copy.action === "unload" && (
          <button type="button" className="secondary-button small" disabled={blocked || knowledge.indexing}
            title={lockReason ?? undefined} onClick={() => void knowledge.unloadModel()}>
            <PowerIcon size={14} />Unload
          </button>
        )}
        {copy.action === "choose" && (
          <button type="button" className="secondary-button small" disabled={blocked}
            title={lockReason ?? undefined} onClick={() => void knowledge.chooseModel()}>
            {knowledge.modelAction === "choosing" ? <Spinner size={13} /> : <CpuIcon size={14} />}Choose model file…
          </button>
        )}
      </div>
      {copy.detail && <p className="hint">{copy.detail}</p>}
      {total > 0 && copy.tone !== "unsupported" && (
        <p className="hint">
          {indexed} of {total} {total === 1 ? "document" : "documents"} {indexed === 1 ? "has" : "have"} a semantic index
          {outdated ? `; ${outdated} ${outdated === 1 ? "needs" : "need"} rebuilding` : ""}.
        </p>
      )}
    </div>
  );
}

function PrivacyPanel({ snapshot }: { snapshot: Bootstrap }) {
  const [recoveryError, setRecoveryError] = useState<string | null>(null);

  async function openRecovery() {
    setRecoveryError(null);
    try {
      await invoke("database_recovery_key");
    } catch (failure) {
      setRecoveryError(errorMessage(failure));
    }
  }

  return (
    <div className="settings-stack">
      <section className="settings-section">
        <h4>On this computer</h4>
        <ul className="privacy-list">
          <li>Local models, Agent, Knowledge, Memory and text recognition run on this computer. Chat shows the selected provider, and web search and model downloads connect when you request them.</li>
          <li>The contextual panel reads the clipboard or the screen only when you ask. Each screen capture asks your desktop for permission again, holds one frame in memory, and keeps only the region you confirm until the panel closes. Nothing from the panel is saved unless you choose Remember.</li>
          <li>Provider API keys are kept in your system keyring, never in the database or this window.</li>
          <li>Conversations, imported document text, semantic indexes and saved memories are encrypted on disk. Your system keyring unlocks the database.</li>
          <li>The paths of models you choose, including the remembered semantic model, are stored locally and never shown to the interface or sent anywhere.</li>
          <li>The agent can read only files and folders you select. Access is read-only, expires automatically, and ends when the app closes.</li>
          <li>Saved answers can contain text or facts from files and documents you selected, even after a document is deleted.</li>
          <li>Deleting a conversation removes its messages and Agent history. Content-free permission audit entries can remain, and existing backups or copies aren't affected.</li>
        </ul>
      </section>
      <section className="settings-section">
        <h4>Database</h4>
        <div className="data-location">
          <span>Location</span>
          <code>{snapshot.databasePath}</code>
        </div>
        <button type="button" className="secondary-button small" onClick={() => void openRecovery()}>
          Show recovery key…
        </button>
        {recoveryError && <p className="error-text" role="alert">{recoveryError}</p>}
        {snapshot.plaintextBackup && (
          <>
            <div className="data-location" role="group" aria-label="Unencrypted migration backup">
              <span aria-hidden="true">Unencrypted migration backup</span>
              <code>{snapshot.plaintextBackup}</code>
            </div>
            <p className="hint">
              After you verify your records and save your recovery key, quit the app and remove this backup
              from your file manager if you no longer need it. Removing this file cannot erase older copies
              in system snapshots or backups, and does not guarantee erasure from an SSD.
            </p>
          </>
        )}
      </section>
    </div>
  );
}

function AboutPanel() {
  return (
    <div className="settings-stack">
      <section className="settings-section">
        <h4>Local-first AI Agent</h4>
        <p className="hint">{appVersion ? `Version ${appVersion}. ` : ""}Chat, Agent, Knowledge and Memory on your own computer, with optional cloud or self-hosted providers for Chat.</p>
      </section>
      <ModelLicense />
      <section className="settings-section" aria-label="Semantic search model license">
        <h4>Semantic search model</h4>
        <p className="hint">
          BGE-small-en-v1.5 by BAAI is MIT-licensed. It isn't bundled: you choose your own copy of the F16 GGUF, which the app
          verifies by checksum before use.
        </p>
      </section>
      <section className="settings-section" aria-label="Third-party notices">
        <h4>Third-party notices</h4>
        <details className="license-text">
          <summary><ChevronRightIcon size={14} />Software notices</summary>
          <pre tabIndex={0}>{softwareNotice.trim()}</pre>
        </details>
        <p className="hint">{builtInModelName}'s license text is included above and with the installed app.</p>
      </section>
    </div>
  );
}
