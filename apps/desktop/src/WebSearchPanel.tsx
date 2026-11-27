import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useState } from "react";
import { LockIcon, Spinner } from "./icons";
import { errorMessage } from "./inference";
import type { SearchProviderKind, WebResearchSettings, WebResearchView } from "./research";
import { Segmented, Switch } from "./ui";

type Choice = "none" | SearchProviderKind;

const networkLabel: Record<string, string> = {
  loopback: "This computer",
  local_network: "Local network address",
  public: "Public internet (HTTPS)",
};

/**
 * Search provider configuration. Keys are entered in a native dialog and kept
 * in the system keyring; this window never sees them.
 */
export function WebSearchPanel({ view, busy, onChanged }: {
  view: WebResearchView | null;
  busy: boolean;
  onChanged: (view: WebResearchView) => void;
}) {
  const endpointId = useId();
  const [endpoint, setEndpoint] = useState(view?.settings.searxngEndpoint ?? "");
  const [working, setWorking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => setEndpoint(view?.settings.searxngEndpoint ?? ""), [view?.settings.searxngEndpoint]);
  // Key status can change elsewhere: refresh when the panel opens.
  useEffect(() => {
    invoke<WebResearchView>("web_research_settings").then(onChanged).catch(() => {});
  }, []);

  if (!view) return <p className="hint">Web search settings are unavailable.</p>;
  const settings = view.settings;
  const disabled = busy || working !== null;

  async function run(kind: string, work: () => Promise<void>) {
    setWorking(kind);
    setError(null);
    setNotice(null);
    try {
      await work();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setWorking(null);
    }
  }

  const save = (next: WebResearchSettings, message?: string) => run("save", async () => {
    onChanged(await invoke<WebResearchView>("web_research_save", { settings: next }));
    if (message) setNotice(message);
  });

  const refresh = async () => onChanged(await invoke<WebResearchView>("web_research_settings"));

  return (
    <>
      <section className="settings-section">
        <h4>Search provider</h4>
        <p className="hint">Used when you turn on Search for a Chat message. Reading a specific webpage needs no provider.</p>
        <Segmented<Choice> label="Search provider" disabled={disabled} value={settings.provider ?? "none"}
          options={[
            { value: "none", label: "None" }, { value: "brave", label: "Brave Search" },
            { value: "searxng", label: "SearXNG" }, { value: "purili", label: "Purili (experimental)" },
          ]}
          onChange={(choice) => void save({ ...settings, provider: choice === "none" ? null : choice })} />
        {!settings.puriliPersonalUse && (
          <p className="hint">To select Purili, first confirm personal, non-commercial use in its section below.</p>
        )}
      </section>
      <section className="settings-section">
        <h4>Brave Search API</h4>
        <p className="hint">
          Uses your own Brave Search API subscription. Each search counts toward your plan's usage and rate limits.
          The key is sent only to api.search.brave.com.
        </p>
        <p className="hint">Key: {view.braveKey === "stored" ? "saved in your system keyring"
          : view.braveKey === "missing" ? "not added" : "secure storage is unavailable or locked"}</p>
        <div className="provider-actions">
          <button type="button" className="secondary-button small" disabled={disabled}
            onClick={() => void run("key", async () => {
              const outcome = await invoke<"stored" | "cancelled">("web_research_enter_key");
              await refresh();
              if (outcome === "stored") setNotice("Brave Search key saved in your keyring.");
            })}>
            {working === "key" ? <Spinner size={13} /> : <LockIcon size={14} />}
            {view.braveKey === "stored" ? "Replace API key…" : "Enter API key…"}
          </button>
          {view.braveKey === "stored" && (
            <button type="button" className="ghost-button small" disabled={disabled}
              onClick={() => void run("remove", async () => {
                await invoke("web_research_delete_key");
                await refresh();
                setNotice("Brave Search key removed.");
              })}>Remove key</button>
          )}
        </div>
        <Switch label="My Brave plan allows storing results" checked={settings.braveSaveResults} disabled={disabled}
          description="When on, the links and titles of sources from Brave are saved with the conversation. Off by default: results are shown only while the app runs. Excerpts are never saved."
          onChange={(value) => void save({ ...settings, braveSaveResults: value })} />
      </section>
      <section className="settings-section">
        <h4>SearXNG instance</h4>
        <p className="hint">
          An instance you run or trust, with JSON output enabled (search.formats in settings.yml). Public instances are never
          chosen automatically. Your instance contacts external search engines, so searching through it is not offline.
        </p>
        <label className="field" htmlFor={endpointId}>
          <span>Instance URL</span>
          <input id={endpointId} type="url" value={endpoint} disabled={disabled} maxLength={512}
            placeholder="http://127.0.0.1:8888" onChange={(event) => setEndpoint(event.currentTarget.value)} />
        </label>
        {view.searxngOrigin && (
          <p className="hint">{view.searxngOrigin} · {networkLabel[view.searxngNetwork ?? ""] ?? "Unknown network"}</p>
        )}
        {view.searxngError && <p className="error-text">{view.searxngError}</p>}
        <div className="provider-actions">
          <button type="button" className="secondary-button small"
            disabled={disabled || endpoint.trim() === settings.searxngEndpoint}
            onClick={() => void save({
              ...settings, searxngEndpoint: endpoint.trim(),
              provider: !endpoint.trim() && settings.provider === "searxng" ? null : settings.provider,
            }, "SearXNG address saved.")}>
            {working === "save" ? <Spinner size={13} /> : null}Save address
          </button>
        </div>
        <Switch label="Save result links found through SearXNG" checked={settings.searxngSaveResults} disabled={disabled}
          description="When on, links and titles of cited sources are saved with the conversation. Excerpts are never saved."
          onChange={(value) => void save({ ...settings, searxngSaveResults: value })} />
      </section>
      <section className="settings-section">
        <h4>Purili (experimental)</h4>
        <p className="hint">
          A free public search service with its own web index. No account or API key. Its terms (puri.li/terms) allow only
          personal, non-commercial use.
        </p>
        <p className="hint">
          Your query is sent to puri.li, which says it keeps access logs (IP address, path and time) for up to 7 days. It
          promises no uptime, its own index may miss things, and some languages may return nothing.
          Its results are never saved with the conversation.
        </p>
        <Switch label="I will use Purili only for personal, non-commercial purposes" checked={settings.puriliPersonalUse}
          disabled={disabled} description="Required to select Purili. Turning this off also deselects it."
          onChange={(value) => void save({
            ...settings, puriliPersonalUse: value,
            provider: !value && settings.provider === "purili" ? null : settings.provider,
          })} />
      </section>
      <section className="settings-section">
        <h4>What web research sends</h4>
        <ul className="plain-list hint">
          <li>Only your message, or the query you edit, goes to the search provider. Conversation history, memories and documents are not sent.</li>
          <li>Up to 3 public HTTPS pages are read directly from their sites, without cookies, sign-in, scripts or images.</li>
          <li>A cloud or self-hosted model receives page excerpts only when you allow it for that message.</li>
          <li>Searches and page reads are listed in each answer's receipt.</li>
        </ul>
      </section>
      {notice && <p className="hint" role="status">{notice}</p>}
      {error && <p className="error-text" role="alert">{error}</p>}
    </>
  );
}
