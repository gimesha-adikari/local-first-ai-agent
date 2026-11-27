// Web research contracts and pure helpers. The Rust engine validates and
// enforces everything; these helpers only explain and pre-check for the UI.

export type SearchProviderKind = "brave" | "searxng" | "purili";
export type ResearchMode = "search" | "page";
export type ResearchStage = "searching" | "reading" | "ranking" | "answering" | "done";
export type CredentialStatus = "stored" | "missing" | "locked" | "unavailable";

export type WebResearchSettings = {
  provider: SearchProviderKind | null;
  braveSaveResults: boolean;
  searxngEndpoint: string;
  searxngSaveResults: boolean;
  /** The user confirmed personal, non-commercial use of Purili. */
  puriliPersonalUse: boolean;
};

export type WebResearchView = {
  settings: WebResearchSettings;
  braveKey: CredentialStatus | null;
  searxngOrigin: string | null;
  searxngNetwork: string | null;
  searxngError: string | null;
  localOnly: boolean;
};

export type WebSourceRecord = {
  evidence: "page" | "snippet";
  saved: boolean;
  url?: string;
  title?: string;
  host?: string;
  queryRemoved?: boolean;
};

export type ResearchRecord = {
  version: number;
  mode: ResearchMode;
  provider?: SearchProviderKind;
  synthesis: "local" | "remote" | "none";
  discovered: number;
  retrieved: number;
  unavailable: number;
  sources: WebSourceRecord[];
  transientResults: boolean;
};

export type WebSourceView = {
  citationId: string;
  evidence: "page" | "snippet";
  supplied: boolean;
  cited: boolean;
  title: string | null;
  url: string | null;
  host: string | null;
  published: string | null;
  excerpt: string | null;
  saved: boolean;
  openable: boolean;
};

export type DiscoveredView = {
  title: string;
  url: string;
  host: string;
  snippet: string;
  published: string | null;
  attempted: boolean;
};

export type ResearchView = {
  record: ResearchRecord;
  sources: WebSourceView[];
  discovered: DiscoveredView[];
  limitations: string[];
  query: string | null;
  transient: boolean;
};

/** What the person set up for the next message. Search is off after each send. */
export type ResearchDraft = {
  enabled: boolean;
  mode: ResearchMode;
  /** An explicit edit of the outgoing query; null sends the message itself. */
  query: string | null;
  url: string;
  resultsOnly: boolean;
  /** Separate consent to send the question and excerpts to the selected provider. */
  share: boolean;
  /** Attachments whose recognized text the query started from (visual-assisted
   * search, Stage 04-E). Only their classes travel with the query; the
   * attachments themselves are not sent. */
  queryAttachments: string[];
};

export const emptyResearch: ResearchDraft = {
  enabled: false, mode: "search", query: null, url: "", resultsOnly: false, share: false, queryAttachments: [],
};

export const maxQueryChars = 400;
export const maxQueryWords = 50;
export const maxPages = 3;

export function providerLabel(provider: SearchProviderKind | null | undefined): string {
  switch (provider) {
    case "brave": return "Brave Search";
    case "searxng": return "SearXNG";
    case "purili": return "Purili (experimental)";
    default: return "No search provider";
  }
}

/** Whitespace collapsed exactly as the engine does before sending. */
export function normalizeQuery(text: string): string {
  return text.split(/\s+/u).filter(Boolean).join(" ");
}

export function queryProblem(text: string): string | null {
  const query = normalizeQuery(text);
  if (!query) return "Enter something to search for.";
  if ([...query].length > maxQueryChars || query.split(" ").length > maxQueryWords) {
    return `Shorten the search to at most ${maxQueryChars} characters and ${maxQueryWords} words.`;
  }
  return null;
}

/** A quick pre-check. The engine applies the full public-address rules. */
export function urlProblem(text: string): string | null {
  const value = text.trim();
  if (!value) return "Enter the webpage address.";
  if (/\s/u.test(value)) return "Remove spaces from the address.";
  if (/^http:\/\//iu.test(value)) return "Only https:// webpages can be read.";
  if (!/^https:\/\//iu.test(value)) return "Enter a complete https:// address.";
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return "Enter a complete https:// address.";
  }
  if (url.username || url.password) return "Remove the user name or password from the address.";
  if (url.port) return "Only webpages on the standard HTTPS port can be read.";
  const host = url.hostname.replace(/\.$/u, "");
  if (/^\[|^\d+(\.\d+){0,3}$|^0x/iu.test(host) || !host.includes(".")
    || /(^|\.)(localhost|local|lan|internal|home\.arpa|test|invalid|onion)$/iu.test(host)) {
    return "Only public websites can be read, not addresses or local names.";
  }
  return null;
}

export type ResearchPlan = {
  canSend: boolean;
  problem: string | null;
  /** Shown before sending: who receives what. */
  destination: string;
  outgoing: string | null;
  answer: "local" | "remote" | "none";
  answerNote: string;
};

/** Explain exactly what the next research request would do, before it is sent. */
export function researchPlan({ draft, message, view, remoteName, modelReady }: {
  draft: ResearchDraft;
  message: string;
  view: WebResearchView | null;
  remoteName: string | null;
  modelReady: boolean;
}): ResearchPlan {
  const provider = view?.settings.provider ?? null;
  let problem: string | null = null;
  let destination: string;
  let outgoing: string | null = null;
  if (view?.localOnly) problem = "Local-only mode is on. Web research needs internet access, even with a local model.";
  if (draft.mode === "search") {
    destination = providerLabel(provider);
    outgoing = normalizeQuery(draft.query ?? message);
    if (!provider) problem ??= "Choose a search provider in Settings → Web search, or read a specific page.";
    else if (provider === "brave" && view?.braveKey !== "stored") problem ??= "Add your Brave Search API key in Settings → Web search.";
    else if (provider === "searxng" && !view?.searxngOrigin) problem ??= "Enter your SearXNG address in Settings → Web search.";
    if (message.trim() || draft.query !== null) problem ??= queryProblem(draft.query ?? message);
  } else {
    destination = "the webpage's own site";
    problem ??= urlProblem(draft.url);
  }
  const pages = draft.mode === "search" ? `up to ${maxPages} public pages` : "the page";
  let answer: ResearchPlan["answer"];
  let answerNote: string;
  if (draft.resultsOnly) {
    answer = "none";
    answerNote = `Results only: ${pages} will be read and listed without asking a model.`;
  } else if (remoteName) {
    answer = draft.share ? "remote" : "none";
    answerNote = draft.share
      ? `${remoteName} receives your question and selected page excerpts to write the answer.`
      : `Sources will be listed. Allow ${remoteName} to receive the excerpts to get a written answer.`;
  } else if (modelReady) {
    answer = "local";
    answerNote = "The answer is written on this device. Searching and reading pages still use the internet.";
  } else {
    answer = "none";
    answerNote = "No model is loaded, so you'll get the sources without a written answer.";
  }
  return { canSend: !problem && !!message.trim(), problem, destination, outgoing, answer, answerNote };
}

/** The per-message research request, or undefined when Search is off. */
export function researchOptions(draft: ResearchDraft, view: WebResearchView | null) {
  if (!draft.enabled) return undefined;
  if (draft.mode === "page") {
    return {
      mode: "page" as const, url: draft.url.trim(),
      ...(draft.resultsOnly ? { resultsOnly: true } : {}),
      ...(draft.share ? { shareWithProvider: true } : {}),
    };
  }
  return {
    mode: "search" as const,
    provider: view?.settings.provider ?? undefined,
    ...(draft.query !== null ? { query: normalizeQuery(draft.query) } : {}),
    ...(draft.query !== null && draft.queryAttachments.length ? { queryAttachments: draft.queryAttachments } : {}),
    ...(draft.resultsOnly ? { resultsOnly: true } : {}),
    ...(draft.share ? { shareWithProvider: true } : {}),
  };
}

export function stageLabel(stage: ResearchStage, mode: ResearchMode = "search"): string {
  switch (stage) {
    case "searching": return "Searching the web…";
    case "reading": return mode === "page" ? "Reading the page…" : "Reading public pages…";
    case "ranking": return "Selecting relevant evidence…";
    case "answering": return "Writing the answer from sources…";
    case "done": return "Done";
  }
}

const plural = (count: number, word: string, many = `${word}s`) => `${count} ${count === 1 ? word : many}`;

/** One-line honest summary of what a research attempt did. */
export function researchSummary(record: ResearchRecord): string {
  const parts: string[] = [];
  if (record.mode === "search") {
    parts.push(`${providerLabel(record.provider)} · ${plural(record.discovered, "result")}`);
  } else {
    parts.push("Direct page");
  }
  parts.push(`${plural(record.retrieved, "page")} read`);
  if (record.unavailable) parts.push(`${record.unavailable} unavailable`);
  parts.push(record.synthesis === "local" ? "answered on this device"
    : record.synthesis === "remote" ? "answered by your provider" : "no written answer");
  return parts.join(" · ");
}

/** What is missing when the session that ran the search has ended. */
export function historyNote(view: ResearchView): string | null {
  if (view.transient) return null;
  if (view.record.provider === "purili") {
    return "Purili results are kept only for the app session that ran them. "
      + "The [S#] markers refer to sources shown at that time.";
  }
  if (view.sources.some((source) => !source.saved)) {
    return "Search results are kept only for the app session that ran them, per the search provider's storage terms. "
      + "The [S#] markers refer to sources shown at that time.";
  }
  return "Excerpts and other results were shown only during the session that ran this search.";
}

export function supplyLabel(source: WebSourceView, synthesis: ResearchRecord["synthesis"]): string {
  if (synthesis === "none") return "Listed";
  if (!source.supplied) return "Not supplied";
  return source.cited ? "Cited" : "Supplied, not cited";
}

export function researchUsageLabel(sources: number): string {
  return `Supplied to the model: ${plural(sources, "web source")}`;
}

/** Error copy for research failures, keyed by the typed IPC code. */
export function researchErrorTitle(code: string): string {
  switch (code) {
    case "credential_missing": return "Search key needed";
    case "provider_authentication": return "Search key rejected";
    case "provider_rate_limited": return "Search limit reached";
    case "budget_exceeded": return "Search quota used up";
    case "policy_denied": return "Web research blocked";
    case "network_failed": return "Couldn't reach the search service";
    default: return "Web research failed";
  }
}
