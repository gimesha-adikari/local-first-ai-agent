import { invoke } from "@tauri-apps/api/core";

/** Provider configuration and status. No credential value ever appears here. */
export type ProviderLocality = "cloud" | "self_hosted";
export type ProviderRetention = "none" | "provider_policy" | "unknown";
export type Sharing = { history: boolean; memories: boolean; documents: boolean };
export type Pricing = { currency: string; inputMicrosPerMillion: number; outputMicrosPerMillion: number };
export type Budget = { monthlyTokens: number | null; monthlyCostMicros: number | null };

export type ProviderSettings = {
  name: string;
  endpoint: string;
  locality: ProviderLocality;
  model: string;
  contextTokens: number;
  maxOutputTokens: number;
  temperature: number;
  imageInput: boolean;
  streamUsage: boolean;
  requiresKey: boolean;
  retention: ProviderRetention;
  retentionSummary: string;
  sharing: Sharing;
  pricing: Pricing | null;
  budget: Budget;
};

export type CredentialStatus = "stored" | "missing" | "locked" | "unavailable";
export type Readiness = "ready" | "deletion_pending" | "invalid_configuration" | "needs_credential" | "credential_unavailable" | "needs_retention" | "local_only";

export type UsageSummary = {
  requests: number;
  reportedTokens: number;
  estimatedTokens: number;
  reservedTokens: number;
  costMicros: number;
  unpricedRequests: number;
};

export type ProviderView = {
  id: number;
  settings: ProviderSettings;
  origin: string;
  network: "unknown" | "public" | "loopback" | "local_network";
  encrypted: boolean;
  credential: CredentialStatus | null;
  period: string;
  usage: UsageSummary;
  modalities: string[];
  readiness: Readiness;
  active: boolean;
};

export type IntelligenceSettings = { localOnly: boolean; activeProvider: number | null };
export type ProviderTest = { status: number; models: string[]; modelListed: boolean | null; elapsedMs: number };
export type EntryOutcome = "stored" | "cancelled";

export const listProviders = () => invoke<ProviderView[]>("provider_list");
export const saveProvider = (id: number | null, settings: ProviderSettings) =>
  invoke<ProviderView>("provider_save", { id, settings });
export const deleteProvider = (id: number) => invoke<void>("provider_delete", { id });
export const testProvider = (id: number) => invoke<ProviderTest>("provider_test", { id });
export const enterCredential = (id: number) => invoke<EntryOutcome>("provider_enter_credential", { id });
export const deleteCredential = (id: number) => invoke<CredentialStatus>("provider_delete_credential", { id });
export const saveIntelligence = (settings: IntelligenceSettings) =>
  invoke<void>("set_intelligence_settings", { settings });

/** A new provider draft. Everything optional starts off. */
export function newProvider(locality: ProviderLocality): ProviderSettings {
  const cloud = locality === "cloud";
  return {
    name: "",
    endpoint: cloud ? "https://" : "http://localhost:8080/v1",
    locality,
    model: "",
    contextTokens: cloud ? 128_000 : 8_192,
    maxOutputTokens: cloud ? 2_048 : 1_024,
    temperature: 0.7,
    imageInput: false,
    streamUsage: true,
    requiresKey: cloud,
    retention: cloud ? "provider_policy" : "none",
    retentionSummary: "",
    sharing: { history: true, memories: false, documents: false },
    pricing: null,
    budget: { monthlyTokens: cloud ? 1_000_000 : null, monthlyCostMicros: null },
  };
}

/** The provider Chat sends to, if any. */
export function activeRemote(settings: IntelligenceSettings | null, providers: ProviderView[]): ProviderView | null {
  if (!settings || settings.localOnly || settings.activeProvider === null) return null;
  return providers.find((provider) => provider.id === settings.activeProvider) ?? null;
}

const categoryNames: Record<keyof Sharing, string> = {
  history: "conversation history",
  memories: "saved memories",
  documents: "document excerpts",
};

/** What a request to this provider may and may not contain. */
export function sharingSummary(sharing: Sharing): { shared: string[]; withheld: string[] } {
  const shared = ["your message"];
  const withheld: string[] = [];
  for (const key of ["history", "memories", "documents"] as const) {
    (sharing[key] ? shared : withheld).push(categoryNames[key]);
  }
  return { shared, withheld };
}

export function listText(items: string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

export function localityLabel(view: Pick<ProviderView, "settings" | "network" | "encrypted">): string {
  if (view.settings.locality === "cloud") return "Cloud";
  const place = view.network === "loopback" ? "this computer" : view.network === "local_network" ? "your local network" : "your server";
  return `Self-hosted on ${place}${view.encrypted ? "" : ", unencrypted"}`;
}

export function retentionText(settings: ProviderSettings): string {
  switch (settings.retention) {
    case "none": return "Provider says requests are not retained.";
    case "provider_policy": return `Provider retention: ${settings.retentionSummary.trim() || "per its policy"}`;
    case "unknown": return "Retention unknown: nothing personal can be sent.";
  }
}

const integer = new Intl.NumberFormat("en-US");

export function formatTokens(value: number): string {
  return integer.format(Math.round(value));
}

/** Money from millionths of the currency unit. */
export function formatMoney(micros: number, currency: string): string {
  const amount = micros / 1_000_000;
  try {
    return new Intl.NumberFormat("en-US", {
      style: "currency", currency, minimumFractionDigits: 2, maximumFractionDigits: amount < 1 ? 4 : 2,
    }).format(amount);
  } catch {
    return `${amount.toFixed(4)} ${currency}`;
  }
}

/** Decimal text to millionths. Empty means unset; invalid is NaN. */
export function parseMicros(text: string): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  if (!/^\d+(\.\d{1,6})?$/.test(trimmed)) return Number.NaN;
  const [whole, fraction = ""] = trimmed.split(".");
  return Number(whole) * 1_000_000 + Number(fraction.padEnd(6, "0"));
}

export function microsText(micros: number | null | undefined): string {
  if (micros === null || micros === undefined) return "";
  const whole = Math.floor(micros / 1_000_000);
  const fraction = String(micros % 1_000_000).padStart(6, "0").replace(/0+$/, "");
  return fraction ? `${whole}.${fraction}` : String(whole);
}

/** Tokens counted against this month's budget, including in-flight reservations. */
export function usedTokens(usage: UsageSummary): number {
  return usage.reportedTokens + usage.estimatedTokens + usage.reservedTokens;
}

export function usageLines(view: ProviderView): string[] {
  const { usage, settings } = view;
  const lines: string[] = [];
  if (!usage.requests) lines.push(`No requests in ${view.period}.`);
  else {
    const parts = [`${formatTokens(usage.reportedTokens)} tokens reported by the provider`];
    if (usage.estimatedTokens) parts.push(`${formatTokens(usage.estimatedTokens)} estimated where no report arrived`);
    if (usage.reservedTokens) parts.push(`${formatTokens(usage.reservedTokens)} reserved in flight`);
    lines.push(`${view.period}: ${parts.join(" · ")}.`);
  }
  if (settings.pricing) {
    const cost = formatMoney(usage.costMicros, settings.pricing.currency);
    lines.push(usage.unpricedRequests
      ? `Estimated cost ${cost}, plus ${usage.unpricedRequests} unpriced ${usage.unpricedRequests === 1 ? "request" : "requests"}.`
      : `Estimated cost ${cost} from your prices. Your provider's bill is authoritative.`);
  } else if (usage.requests) {
    lines.push("Cost unknown: no prices configured.");
  }
  const limits: string[] = [];
  if (settings.budget.monthlyTokens !== null) {
    limits.push(`${formatTokens(Math.max(0, settings.budget.monthlyTokens - usedTokens(usage)))} of ${formatTokens(settings.budget.monthlyTokens)} tokens left`);
  }
  if (settings.budget.monthlyCostMicros !== null && settings.pricing) {
    limits.push(`spending limit ${formatMoney(settings.budget.monthlyCostMicros, settings.pricing.currency)} (estimated)`);
  }
  lines.push(limits.length ? `Monthly limit: ${limits.join(" · ")}.` : "No monthly limit set.");
  return lines;
}

export type ReadinessCopy = { tone: "ready" | "warning" | "error" | "muted"; label: string; detail: string | null };

export function readinessCopy(view: ProviderView): ReadinessCopy {
  switch (view.readiness) {
    case "ready":
      return { tone: "ready", label: view.credential === "stored" ? "Ready · API key in keyring" : "Ready", detail: null };
    case "deletion_pending":
      return { tone: "warning", label: "Removal pending", detail: "This provider is disabled. Retry removal to finish deleting its key and configuration." };
    case "invalid_configuration":
      return { tone: "error", label: "Invalid provider configuration", detail: "Edit the provider address and API key settings, or remove this provider." };
    case "needs_credential":
      return { tone: "warning", label: "Needs an API key", detail: "Enter the key in the system dialog. It is stored in your keyring, never in the app." };
    case "credential_unavailable":
      return view.credential === "locked"
        ? { tone: "warning", label: "Keyring locked", detail: "Unlock your system keyring, then try again. Cloud providers stay off until then." }
        : { tone: "error", label: "Secure storage unavailable", detail: "No keyring service is available, so providers that need a key stay off. Local models still work." };
    case "needs_retention":
      return { tone: "warning", label: "Retention not declared", detail: "Say how the provider retains requests before anything can be sent." };
    case "local_only":
      return { tone: "muted", label: "Off · local-only mode", detail: null };
  }
}

/** The one-line disclosure shown with the composer before a remote request. */
export function disclosure(view: ProviderView): string {
  const { shared, withheld } = sharingSummary(view.settings.sharing);
  const parts = [
    `Sends ${listText(shared)} to ${view.settings.name} (${localityLabel(view).toLowerCase()})`,
    withheld.length ? `not ${listText(withheld)}` : null,
  ].filter(Boolean);
  return `${parts.join("; ")}.`;
}

/** Client-side hints for the draft. The backend validates authoritatively. */
export function draftProblem(settings: ProviderSettings): string | null {
  if (!settings.name.trim()) return "Name the provider.";
  if (!/^https?:\/\/\S+$/i.test(settings.endpoint.trim())) return "Enter the provider's API URL, for example https://api.example.com/v1.";
  if (settings.locality === "cloud" && !/^https:\/\//i.test(settings.endpoint.trim())) return "Cloud providers must use https.";
  if (!settings.model.trim() || /\s/.test(settings.model.trim())) return "Enter the model identifier, without spaces.";
  if (!(settings.contextTokens >= 1024 && settings.contextTokens <= 2_000_000)) return "Context must be between 1,024 and 2,000,000 tokens.";
  if (!(settings.maxOutputTokens >= 1 && settings.maxOutputTokens <= 32_768 && settings.maxOutputTokens < settings.contextTokens)) {
    return "Use 1–32,768 output tokens, fewer than the context.";
  }
  if (!(settings.temperature >= 0 && settings.temperature <= 2)) return "Temperature must be between 0 and 2.";
  if (settings.retention === "provider_policy" && !settings.retentionSummary.trim()) return "Summarize the provider's retention policy.";
  if (settings.budget.monthlyCostMicros !== null && !settings.pricing) return "A spending limit needs prices.";
  if (Number.isNaN(settings.budget.monthlyCostMicros ?? 0) || Number.isNaN(settings.budget.monthlyTokens ?? 0)) return "Enter valid limits.";
  if (settings.pricing && (Number.isNaN(settings.pricing.inputMicrosPerMillion) || Number.isNaN(settings.pricing.outputMicrosPerMillion))) {
    return "Enter valid prices.";
  }
  return null;
}
