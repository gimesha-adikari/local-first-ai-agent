import { useState } from "react";
import { CubeIcon, EditIcon, LockIcon, PlusIcon, RefreshIcon, ShieldIcon, Spinner, TrashIcon } from "./icons";
import { errorMessage } from "./inference";
import {
  type IntelligenceSettings, type ProviderSettings, type ProviderTest, type ProviderView,
  deleteCredential, deleteProvider, disclosure, draftProblem, enterCredential, listText, localityLabel, microsText, newProvider,
  parseMicros, readinessCopy, retentionText, saveIntelligence, saveProvider, sharingSummary, testProvider, usageLines,
} from "./providers";
import { Callout, Switch } from "./ui";

/** Providers and the Chat target. Credentials are entered natively and never pass through this view. */
export function ProvidersPanel({ providers, intelligence, busy, onChanged }: {
  providers: ProviderView[];
  intelligence: IntelligenceSettings;
  busy: boolean;
  onChanged: () => Promise<void>;
}) {
  const [editing, setEditing] = useState<{ id: number | null; draft: ProviderSettings } | null>(null);
  const [working, setWorking] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ tone: "error" | "accent" | "warning"; title: string; detail?: string } | null>(null);
  const [tests, setTests] = useState<Record<number, ProviderTest>>({});
  const locked = busy || working !== null;

  async function run(label: string, action: () => Promise<void>) {
    if (working) return;
    setWorking(label);
    setNotice(null);
    try {
      await action();
    } catch (failure) {
      setNotice({ tone: "error", title: "That didn't work", detail: errorMessage(failure) });
    } finally {
      setWorking(null);
      await onChanged().catch(() => undefined);
    }
  }

  const choose = (activeProvider: number | null) => run("mode", () => saveIntelligence({ activeProvider }));

  return (
    <div className="settings-stack">
      <section className="settings-section">
        <fieldset className="provider-choice" disabled={locked}>
          <legend>Chat answers with</legend>
          <label className="provider-option">
            <input type="radio" name="chat-target" checked={intelligence.activeProvider === null}
              onChange={() => void choose(null)} />
            <span><strong>Local model</strong><small>Runs on this computer. Nothing is sent anywhere.</small></span>
          </label>
          {providers.map((provider) => (
            <label key={provider.id} className="provider-option">
              <input type="radio" name="chat-target" disabled={provider.readiness !== "ready"}
                checked={intelligence.activeProvider === provider.id}
                onChange={() => void choose(provider.id)} />
              <span>
                <strong>{provider.settings.name}</strong>
                <small>{provider.settings.model} · {localityLabel(provider)}</small>
              </span>
            </label>
          ))}
        </fieldset>
        {providers.filter((provider) => provider.id === intelligence.activeProvider).map((provider) => (
          <details key={provider.id} className="hint">
            <summary>What Chat shares</summary>
            <p>{disclosure(provider)} {retentionText(provider.settings)}</p>
          </details>
        ))}
        <p className="hint">
          There is no automatic fallback. If the selected provider fails, Chat reports the error and nothing is sent elsewhere.
          Agent mode always uses the local model.
        </p>
      </section>

      {notice && (
        <Callout tone={notice.tone} role={notice.tone === "error" ? "alert" : "status"} title={notice.title}
          actions={<button type="button" className="link-button" onClick={() => setNotice(null)}>Dismiss</button>}>
          {notice.detail}
        </Callout>
      )}

      <section className="settings-section">
        <div className="setting-row">
          <h4>Providers</h4>
          {!editing && (
            <div className="provider-add">
              <button type="button" className="secondary-button small" disabled={locked}
                onClick={() => setEditing({ id: null, draft: newProvider("cloud") })}><PlusIcon size={14} />Cloud provider</button>
              <button type="button" className="secondary-button small" disabled={locked}
                onClick={() => setEditing({ id: null, draft: newProvider("self_hosted") })}><PlusIcon size={14} />Self-hosted server</button>
            </div>
          )}
        </div>
        {!providers.length && !editing && (
          <p className="hint">
            Add an OpenAI-compatible cloud service or a server you run, such as llama.cpp, Ollama, vLLM or LM Studio.
            Compatible servers differ: only the features you enable are used.
          </p>
        )}
        {editing && (
          <ProviderForm initial={editing.draft} creating={editing.id === null} disabled={locked}
            onCancel={() => setEditing(null)}
            onSave={(draft) => run("save", async () => {
              const saved = await saveProvider(editing.id, draft);
              setEditing(null);
              setNotice(saved.credential === "missing"
                ? { tone: "accent", title: `${saved.settings.name} saved`, detail: "Enter its API key to use it. The key goes to your system keyring through a separate dialog." }
                : { tone: "accent", title: `${saved.settings.name} saved` });
            })} />
        )}
        <ul className="provider-list">
          {providers.map((provider) => (
            <ProviderCard key={provider.id} provider={provider} test={tests[provider.id]} working={working} disabled={locked || !!editing}
              onEdit={() => setEditing({ id: provider.id, draft: provider.settings })}
              onKey={() => run(`key-${provider.id}`, async () => {
                const outcome = await enterCredential(provider.id);
                if (outcome === "stored") setNotice({ tone: "accent", title: "API key saved in your keyring" });
              })}
              onRemoveKey={() => run(`unkey-${provider.id}`, async () => { await deleteCredential(provider.id); })}
              onTest={() => run(`test-${provider.id}`, async () => {
                const result = await testProvider(provider.id);
                setTests((current) => ({ ...current, [provider.id]: result }));
              })}
              onDelete={() => run(`delete-${provider.id}`, async () => { await deleteProvider(provider.id); })} />
          ))}
        </ul>
      </section>

      <section className="settings-section">
        <h4><ShieldIcon size={14} /> How remote requests are protected</h4>
        <ul className="privacy-list">
          <li>Only the categories you allow are sent. Anything else is withheld before the request is built, and each request is authorized once, for that request only.</li>
          <li>Connections go only to the exact address you configured, with verified TLS for cloud providers. Redirects, proxies and automatic retries are never used.</li>
          <li>API keys stay in your system keyring and are attached only to requests for that provider's address. They are never shown in this window or saved in the app's database.</li>
          <li>Data already sent can't be recalled. It falls under the provider's retention, which each answer's details show.</li>
          <li>Budgets limit estimated use before each request. Providers bill by their own count, so the bill can differ.</li>
        </ul>
      </section>
    </div>
  );
}

function ProviderCard({ provider, test, working, disabled, onEdit, onKey, onRemoveKey, onTest, onDelete }: {
  provider: ProviderView;
  test: ProviderTest | undefined;
  working: string | null;
  disabled: boolean;
  onEdit: () => void;
  onKey: () => void;
  onRemoveKey: () => void;
  onTest: () => void;
  onDelete: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const status = readinessCopy(provider);
  const { shared, withheld } = sharingSummary(provider.settings.sharing);
  const spinning = (label: string) => working === `${label}-${provider.id}`;
  return (
    <li className="provider-card" aria-label={provider.settings.name}>
      <div className="provider-heading">
        <span className={`status-dot${status.tone === "ready" ? " status-ready" : status.tone === "warning" ? " status-warning"
          : status.tone === "error" ? " status-error" : ""}`} aria-hidden="true" />
        <div className="provider-title">
          <strong>{provider.settings.name}</strong>
          <small>{provider.settings.model} · {localityLabel(provider)}</small>
        </div>
        {provider.active && <span className="badge badge-ready">Chat</span>}
      </div>
      <p className="hint" role="status">{status.label}{status.detail ? `. ${status.detail}` : ""}</p>
      <dl className="provider-facts">
        <div><dt>Address</dt><dd><code>{provider.settings.endpoint}</code></dd></div>
        <div><dt>Shares</dt><dd>{listText(shared)}{withheld.length ? `. Never ${listText(withheld)}.` : "."}</dd></div>
        <div><dt>Retention</dt><dd>{retentionText(provider.settings)}</dd></div>
        <div><dt>Capabilities</dt><dd>Text Chat with streaming · {provider.settings.contextTokens.toLocaleString("en-US")}-token context
          {provider.modalities.includes("image") ? " · image input" : ""}{provider.settings.streamUsage ? " · usage reporting" : ""}</dd></div>
        <div><dt>Usage</dt><dd>{usageLines(provider).map((line) => <span key={line} className="provider-usage-line">{line}</span>)}</dd></div>
      </dl>
      {test && (
        <p className="hint" role="status">
          Connected in {test.elapsedMs} ms. {test.modelListed === true ? `${provider.settings.model} is available.`
            : test.modelListed === false ? `${provider.settings.model} isn't in the provider's list (${test.models.length} models).`
              : "The provider didn't list its models."}
        </p>
      )}
      <div className="provider-actions">
        {provider.credential !== null && (
          <button type="button" className="secondary-button small" disabled={disabled} onClick={onKey}>
            {spinning("key") ? <Spinner size={13} /> : <LockIcon size={14} />}{provider.credential === "stored" ? "Replace API key…" : "Enter API key…"}
          </button>
        )}
        {provider.credential === "stored" && (
          <button type="button" className="ghost-button small" disabled={disabled} onClick={onRemoveKey}>Remove key</button>
        )}
        <button type="button" className="secondary-button small" disabled={disabled || provider.readiness !== "ready"}
          onClick={onTest}>
          {spinning("test") ? <Spinner size={13} /> : <RefreshIcon size={14} />}Test connection
        </button>
        <button type="button" className="ghost-button small" disabled={disabled || provider.readiness === "deletion_pending"} onClick={onEdit}><EditIcon size={14} />Edit</button>
        {provider.readiness === "deletion_pending" ? (
          <button type="button" className="ghost-button small danger-text" disabled={disabled} onClick={onDelete}>
            <TrashIcon size={14} />Retry removal
          </button>
        ) : confirming ? (
          <>
            <button type="button" className="ghost-button small danger-text" disabled={disabled}
              onClick={() => { setConfirming(false); onDelete(); }}>Remove provider and key</button>
            <button type="button" className="ghost-button small" onClick={() => setConfirming(false)}>Keep</button>
          </>
        ) : (
          <button type="button" className="ghost-button small danger-text" disabled={disabled} onClick={() => setConfirming(true)}>
            <TrashIcon size={14} />Remove
          </button>
        )}
      </div>
    </li>
  );
}

function numberOr(text: string, fallback: number): number {
  const value = Number(text);
  return text.trim() === "" || !Number.isFinite(value) ? fallback : value;
}

function ProviderForm({ initial, creating, disabled, onCancel, onSave }: {
  initial: ProviderSettings;
  creating: boolean;
  disabled: boolean;
  onCancel: () => void;
  onSave: (draft: ProviderSettings) => void;
}) {
  const [draft, setDraft] = useState(initial);
  const [prices, setPrices] = useState({
    currency: initial.pricing?.currency ?? "USD",
    input: microsText(initial.pricing?.inputMicrosPerMillion),
    output: microsText(initial.pricing?.outputMicrosPerMillion),
    spend: microsText(initial.budget.monthlyCostMicros),
  });
  const [tokens, setTokens] = useState(initial.budget.monthlyTokens === null ? "" : String(initial.budget.monthlyTokens));
  const set = (patch: Partial<ProviderSettings>) => setDraft((current) => ({ ...current, ...patch }));
  const input = parseMicros(prices.input);
  const output = parseMicros(prices.output);
  const priced = input !== null || output !== null;
  const candidate: ProviderSettings = {
    ...draft,
    pricing: priced ? { currency: prices.currency.trim().toUpperCase(), inputMicrosPerMillion: input ?? 0, outputMicrosPerMillion: output ?? 0 } : null,
    budget: {
      monthlyTokens: tokens.trim() === "" ? null : Math.floor(Number(tokens)),
      monthlyCostMicros: parseMicros(prices.spend),
    },
  };
  const problem = draftProblem(candidate);
  const cloud = draft.locality === "cloud";
  return (
    <form className="provider-form" aria-label={creating ? "New provider" : `Edit ${initial.name}`}
      onSubmit={(event) => { event.preventDefault(); if (!problem) onSave(candidate); }}>
      <h4><CubeIcon size={14} />{creating ? (cloud ? "New cloud provider" : "New self-hosted server") : `Edit ${initial.name}`}</h4>
      <div className="form-grid">
        <label className="field"><span>Name</span>
          <input value={draft.name} maxLength={64} placeholder={cloud ? "My cloud provider" : "My server"}
            onChange={(event) => set({ name: event.target.value })} /></label>
        <label className="field"><span>Model identifier</span>
          <input value={draft.model} maxLength={200} spellCheck={false} placeholder="model-name"
            onChange={(event) => set({ model: event.target.value })} /></label>
        <label className="field field-wide"><span>API URL <small>{cloud ? "https only" : "http is allowed on this computer or a local-network IP address"}</small></span>
          <input value={draft.endpoint} maxLength={512} spellCheck={false} inputMode="url"
            onChange={(event) => set({ endpoint: event.target.value })} /></label>
        <label className="field"><span>Context window <small>tokens</small></span>
          <input type="number" min={1024} max={2000000} value={draft.contextTokens}
            onChange={(event) => set({ contextTokens: numberOr(event.target.value, 0) })} /></label>
        <label className="field"><span>Output limit <small>tokens per reply</small></span>
          <input type="number" min={1} max={32768} value={draft.maxOutputTokens}
            onChange={(event) => set({ maxOutputTokens: numberOr(event.target.value, 0) })} /></label>
        <label className="field"><span>Temperature</span>
          <input type="number" min={0} max={2} step={0.1} value={draft.temperature}
            onChange={(event) => set({ temperature: numberOr(event.target.value, -1) })} /></label>
      </div>
      {!cloud && (
        <Switch label="Requires an API key" checked={draft.requiresKey} disabled={disabled}
          description="Turn off for local servers without authentication."
          onChange={(requiresKey) => set({ requiresKey })} />
      )}
      <Switch label="Model accepts images" checked={draft.imageInput} disabled={disabled}
        description="Only turn this on if the model supports image input. It is used by later screen features."
        onChange={(imageInput) => set({ imageInput })} />
      <Switch label="Ask for usage in the stream" checked={draft.streamUsage} disabled={disabled}
        description="Recommended. Turn off if the server rejects the stream_options field; usage is then estimated."
        onChange={(streamUsage) => set({ streamUsage })} />

      <fieldset className="form-group">
        <legend>Provider-side retention</legend>
        {(["none", "provider_policy", "unknown"] as const).map((retention) => (
          <label key={retention} className="provider-option">
            <input type="radio" name="retention" checked={draft.retention === retention} onChange={() => set({ retention })} />
            <span>
              <strong>{retention === "none" ? "Not retained" : retention === "provider_policy" ? "Retained per the provider's policy" : "Unknown"}</strong>
              <small>{retention === "none" ? "A server you control without logging, or a zero-retention agreement."
                : retention === "provider_policy" ? "Summarize what its data policy says."
                  : "Nothing personal can be sent until retention is declared."}</small>
            </span>
          </label>
        ))}
        {draft.retention === "provider_policy" && (
          <label className="field"><span>Retention summary</span>
            <textarea rows={2} maxLength={500} value={draft.retentionSummary}
              placeholder="For example: kept up to 30 days for abuse monitoring; not used for training."
              onChange={(event) => set({ retentionSummary: event.target.value })} /></label>
        )}
      </fieldset>

      <fieldset className="form-group">
        <legend>What may be sent</legend>
        <p className="hint">Your message always goes to the provider you choose for Chat. Everything else stays on this computer unless allowed here.</p>
        <Switch label="Conversation history" checked={draft.sharing.history} disabled={disabled}
          description="Earlier turns of the conversation, including answers from your local model."
          onChange={(history) => set({ sharing: { ...draft.sharing, history } })} />
        <Switch label="Saved memories" checked={draft.sharing.memories} disabled={disabled}
          description="Relevant memories, when memory is on. Off means they are withheld."
          onChange={(memories) => set({ sharing: { ...draft.sharing, memories } })} />
        <Switch label="Document excerpts" checked={draft.sharing.documents} disabled={disabled}
          description="Passages from selected Knowledge documents. Off means document questions stay on the local model."
          onChange={(documents) => set({ sharing: { ...draft.sharing, documents } })} />
      </fieldset>

      <fieldset className="form-group">
        <legend>Prices and monthly limits</legend>
        <p className="hint">Optional prices from your provider's price list give estimated costs. Leave them empty if unknown.</p>
        <div className="form-grid">
          <label className="field"><span>Currency</span>
            <input value={prices.currency} maxLength={3} onChange={(event) => setPrices({ ...prices, currency: event.target.value })} /></label>
          <label className="field"><span>Input <small>per 1M tokens</small></span>
            <input value={prices.input} inputMode="decimal" placeholder="unknown"
              onChange={(event) => setPrices({ ...prices, input: event.target.value })} /></label>
          <label className="field"><span>Output <small>per 1M tokens</small></span>
            <input value={prices.output} inputMode="decimal" placeholder="unknown"
              onChange={(event) => setPrices({ ...prices, output: event.target.value })} /></label>
          <label className="field"><span>Token limit <small>per month</small></span>
            <input type="number" min={1} value={tokens} placeholder="no limit" onChange={(event) => setTokens(event.target.value)} /></label>
          <label className="field"><span>Spending limit <small>estimated, per month</small></span>
            <input value={prices.spend} inputMode="decimal" placeholder="no limit" disabled={!priced}
              onChange={(event) => setPrices({ ...prices, spend: event.target.value })} /></label>
        </div>
        <p className="hint">Limits are checked against a high estimate before each request. They can't guarantee the provider's bill.</p>
      </fieldset>

      {cloud && creating && (
        <p className="hint"><LockIcon size={13} /> After saving, enter the API key in a separate system dialog. It never appears in this window.</p>
      )}
      {problem && <p className="hint" role="status">{problem}</p>}
      <div className="dialog-actions">
        <button type="button" className="ghost-button" onClick={onCancel} disabled={disabled}>Cancel</button>
        <button type="submit" className="primary-button" disabled={disabled || !!problem}>{creating ? "Save provider" : "Save changes"}</button>
      </div>
    </form>
  );
}
