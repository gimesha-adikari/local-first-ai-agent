// Pure logic for the knowledge library, contextual Chat, source references and
// personal memory (v0.2.0 Task 01–03 handoffs). No React or Tauri imports, so
// the Node test runner can exercise it directly. Document names, excerpts and
// memories are untrusted text: callers render them as plain text only.

export type RetrievalMode = "keyword" | "semantic" | "hybrid";
export const retrievalModes: readonly RetrievalMode[] = ["keyword", "semantic", "hybrid"];

export type CommandError = { code: string; message: string };

export type PdfCoverage = { pageCount: number; pagesExamined: number; pagesWithoutText: number };

export type KnowledgeDocument = {
  id: number;
  name: string;
  format: "txt" | "markdown" | "pdf";
  fileBytes: number;
  textBytes: number;
  chunkCount: number;
  indexingStatus: "ready" | "partial";
  pdfCoverage: PdfCoverage | null;
  createdAt: number;
  updatedAt: number;
  revision: number;
};

export type LibraryDocument = KnowledgeDocument & { embeddingState: "ready" | "missing" | "incompatible" };

export function selectionIndexCoverage(ids: readonly number[], states: Readonly<Record<number, DocumentIndexState | null>>): IndexCoverage {
  const coverage = { indexedDocuments: 0, missingDocuments: 0, incompatibleDocuments: 0 };
  for (const id of new Set(ids)) {
    if (states[id] === "indexed") coverage.indexedDocuments++;
    else if (states[id] === "incompatible") coverage.incompatibleDocuments++;
    else coverage.missingDocuments++;
  }
  return coverage;
}

export type SourceReference = { kind: "page"; page: number } | { kind: "lines"; startLine: number; endLine: number };

export type KnowledgeHit = {
  documentId: number;
  documentName: string;
  ordinal: number;
  revision?: number;
  source: SourceReference;
  text: string;
  score: number;
};

export type IndexCoverage = { indexedDocuments: number; missingDocuments: number; incompatibleDocuments: number };
export type RetrievalResults = { mode: RetrievalMode; coverage: IndexCoverage | null; hits: KnowledgeHit[] };

export type EmbeddingStatus = {
  supported: boolean;
  available: boolean;
  name: string;
  sha256: string;
  preprocessing: number;
  dimensions: number;
  contextTokens: number;
  cpuThreads: number;
  /** Launch-time restore of the remembered model (Task 06-A). */
  restore?: EmbeddingRestore;
};

export type EmbeddingRestore = "not_remembered" | "pending" | "restored" | "selected" | "unloaded" | "missing" | "incompatible" | "failed";

export type ImportOutcome = { kind: "imported" | "replaced" | "duplicate"; documentId: number };
export type ImportState =
  | { phase: "selecting" | "confirming" | "reading" | "extracting" | "indexing" | "committing" | "cancelling" | "cancelled" }
  | { phase: "completed"; outcome: ImportOutcome }
  | { phase: "failed"; error: CommandError };
export type ImportStatus = { jobId: number; state: ImportState };

export type IndexPhase = "indexing" | "cancelling" | "committing" | "completed" | "cancelled" | "failed";
export type IndexProgress = {
  jobId: number;
  documentId: number;
  phase: IndexPhase;
  processedChunks: number;
  totalChunks: number;
  windowCount: number | null;
  error: CommandError | null;
};

/** How a request with selected documents was routed (Task 06-A). */
export type ChatIntent = "general" | "search" | "question" | "overview";
/** "auto" keeps documents available without blocking unrelated requests; "documents" always grounds. */
export type Grounding = "auto" | "documents";
export type OverviewCoverage = { documentId: number; suppliedChunks: number; totalChunks: number };

/** Coverage at the time of retrieval. */
export type KnowledgeCoverage = {
  mode: RetrievalMode;
  intent?: ChatIntent;
  grounding?: Grounding;
  overview?: OverviewCoverage[];
  /** Requested dense mode when automatic grounding ranked by keyword instead. */
  requestedMode?: RetrievalMode;
  embeddings: IndexCoverage | null;
  partialDocumentIds: number[];
  unavailableDocumentIds: number[];
  missingIndexDocumentIds?: number[];
  incompatibleIndexDocumentIds?: number[];
  unsearchedDocumentIds?: number[];
  retrieved: number;
};

/** Fitted counts saved with a contextual assistant message. */
export type ContextUsage = { sources: number; memories: number };
export type AttemptContext = {
  coverage: KnowledgeCoverage | null;
  memoryEnabled: boolean | null;
  fitted: ContextUsage | null;
};

export type SourceCitation = {
  citationId: string;
  documentId: number;
  revision: number;
  chunkOrdinal: number;
  documentName: string;
  source: SourceReference;
  available: boolean;
  cited: boolean;
};

export type PersonalMemory = { id: number; content: string; createdAt: number; updatedAt: number };

export type MemoryPage = { items: PersonalMemory[]; total: number; hasMore: boolean };

export type KnowledgeSelection = { enabled: boolean; mode: RetrievalMode; documentIds: number[]; grounding?: Grounding };
export const defaultSelection: KnowledgeSelection = { enabled: false, mode: "keyword", documentIds: [], grounding: "auto" };

/** Per-document semantic index state derived from the library snapshot. */
export type DocumentIndexState = "indexed" | "missing" | "incompatible";

// ------------------------------------------------------------------ limits
export const maxSelectedDocuments = 128;
export const maxQueryBytes = 256;
export const memoryMaxBytes = 1024;
export const memoryMaxCount = 512;
export const memoryPageSize = 50;
export const documentPageSize = 100;

const encoder = new TextEncoder();
export const utf8Bytes = (text: string) => encoder.encode(text).length;

export const validDocumentId = (id: unknown): id is number =>
  typeof id === "number" && Number.isSafeInteger(id) && id > 0;

// --------------------------------------------------------------- formatting
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export const formatLabel: Record<KnowledgeDocument["format"], string> = { txt: "Text", markdown: "Markdown", pdf: "PDF" };

export const modeLabel: Record<RetrievalMode, string> = { keyword: "Keyword", semantic: "Semantic", hybrid: "Hybrid" };
export const modeHint: Record<RetrievalMode, string> = {
  keyword: "Matches the words you use",
  semantic: "Finds passages with a similar meaning",
  hybrid: "Combines word and meaning matches",
};

export function sourceLabel(source: SourceReference): string {
  if (source.kind === "page") return `Page ${source.page}`;
  return source.startLine === source.endLine ? `Line ${source.startLine}` : `Lines ${source.startLine}–${source.endLine}`;
}

const plural = (count: number, one: string, many = `${one}s`) => `${count.toLocaleString("en-US")} ${count === 1 ? one : many}`;

export function documentSummary(document: KnowledgeDocument): string {
  const parts = [formatLabel[document.format] ?? document.format];
  if (document.pdfCoverage) parts.push(plural(document.pdfCoverage.pageCount, "page"));
  parts.push(formatBytes(document.fileBytes));
  return parts.join(" · ");
}

/** Why a document's text is incomplete, or null when all of it was read. */
export function textCoverageNote(document: KnowledgeDocument): string | null {
  const pdf = document.pdfCoverage;
  if (pdf) {
    const notes: string[] = [];
    if (pdf.pagesExamined < pdf.pageCount) {
      notes.push(`only the first ${plural(pdf.pagesExamined, "page")} of ${pdf.pageCount.toLocaleString("en-US")} were read`);
    }
    if (pdf.pagesWithoutText > 0) {
      notes.push(`${plural(pdf.pagesWithoutText, "page")} had no selectable text`);
    }
    if (notes.length) {
      const text = notes.join(" and ");
      return `${text[0].toUpperCase()}${text.slice(1)}. Scanned pages aren't read.`;
    }
  }
  return document.indexingStatus === "partial" ? "Some of this document's text couldn't be read." : null;
}

export function documentIndexState(coverage: IndexCoverage | null | undefined): DocumentIndexState | null {
  if (!coverage) return null;
  if (coverage.indexedDocuments > 0) return "indexed";
  if (coverage.incompatibleDocuments > 0) return "incompatible";
  if (coverage.missingDocuments > 0) return "missing";
  return null;
}

export const indexStateLabel: Record<DocumentIndexState, string> = {
  indexed: "Semantic index ready",
  missing: "No semantic index",
  incompatible: "Semantic index outdated",
};

// ---------------------------------------------------------- import/indexing
const importLabels: Record<ImportState["phase"], string> = {
  selecting: "Choose a file in the system dialog…",
  confirming: "Confirm keeping a copy of the text…",
  reading: "Reading the file…",
  extracting: "Extracting text…",
  indexing: "Preparing keyword search…",
  committing: "Saving to your library…",
  cancelling: "Cancelling…",
  completed: "Done",
  cancelled: "Cancelled",
  failed: "Failed",
};

export const importPhaseLabel = (state: ImportState) => importLabels[state.phase] ?? "Working…";
export const importTerminal = (state: ImportState) =>
  state.phase === "completed" || state.phase === "failed" || state.phase === "cancelled";
/** The host accepts cancellation only while the worker is processing; dialogs are dismissed instead. */
export const importCancellable = (state: ImportState) =>
  state.phase === "reading" || state.phase === "extracting" || state.phase === "indexing";
export const importInDialog = (state: ImportState) => state.phase === "selecting" || state.phase === "confirming";

export const indexTerminal = (phase: IndexPhase) => phase === "completed" || phase === "cancelled" || phase === "failed";

export function indexPercent(progress: Pick<IndexProgress, "processedChunks" | "totalChunks">): number | null {
  if (!(progress.totalChunks > 0)) return null;
  return Math.min(100, Math.max(0, Math.round((progress.processedChunks / progress.totalChunks) * 100)));
}

export function importOutcomeNotice(outcome: ImportOutcome, name: string | null): string {
  const label = name ? `“${name}”` : "the document";
  switch (outcome.kind) {
    case "imported":
      return `Added ${label} to your library. Build its semantic index to use semantic search.`;
    case "replaced":
      return `Replaced ${label}. Earlier citations to the previous version are now unavailable. Rebuild its semantic index to use semantic search.`;
    case "duplicate":
      return `${name ? label : "The document"} is already in your library, so nothing changed.`;
    default:
      return "The import finished.";
  }
}

/** Specific copy for knowledge, embedding and memory errors, keyed by typed IPC code. */
export function knowledgeErrorCopy(error: CommandError): { title: string; detail: string } {
  const message = error.message.trim();
  switch (error.code) {
    case "busy":
      return { title: "Another operation is running", detail: "Wait for the current answer, task, import or index to finish, then try again." };
    case "knowledge_limit":
      return { title: "The library is full", detail: message || "Delete documents you no longer need, then try again." };
    case "knowledge_not_found":
      return { title: "That document is no longer in your library", detail: "It may have been deleted. The list has been refreshed." };
    case "knowledge_revision_changed":
      return { title: "The document changed while indexing", detail: "It was replaced during indexing. Build its index again." };
    case "permission_denied":
      return { title: "The file couldn't be opened", detail: message || "Choose a regular file you can read." };
    case "embedding_unavailable":
      return { title: "The semantic search model isn't loaded", detail: "Choose the model file in Knowledge, or switch to Keyword." };
    case "embedding_incompatible":
      return { title: "The semantic index doesn't match the model", detail: "Rebuild the semantic index of the selected documents, or switch to Keyword." };
    case "embedding_index_unavailable":
      return { title: "The selected documents have no semantic index", detail: "Build their semantic index in Knowledge, or switch to Keyword." };
    case "timed_out":
      return { title: "The operation took too long", detail: message || "Try again with a smaller document." };
    case "memory_limit":
      return { title: `You've reached the ${memoryMaxCount}-memory limit`, detail: "Delete memories you no longer need to add more." };
    case "not_found":
      return { title: "That item no longer exists", detail: "The list has been refreshed." };
    case "invalid_arguments":
    case "invalid_config":
      return { title: "The request wasn't accepted", detail: message || "Check the input and try again." };
    case "cancelled":
      return { title: "Cancelled", detail: message };
    default:
      if (error.code.startsWith("pdf_")) {
        return { title: "The PDF couldn't be read", detail: message || "It may be damaged, encrypted or unsupported." };
      }
      return { title: "Something went wrong", detail: message || "Try again." };
  }
}

// ---------------------------------------------------------- chat selection
/** Drop IDs no longer in the library. Returns the same array when nothing changed. */
export function pruneSelection(selected: number[], documents: readonly { id: number }[]): number[] {
  const present = new Set(documents.map(({ id }) => id));
  const next = selected.filter((id) => present.has(id));
  return next.length === selected.length ? selected : next;
}

/** The `knowledge` request option, or null for ordinary Chat. Never widens the selection. */
export function knowledgeOption(selection: KnowledgeSelection): { mode: RetrievalMode; documentIds: number[]; grounding?: Grounding } | null {
  if (!selection.enabled) return null;
  const documentIds = [...new Set(selection.documentIds)].filter(validDocumentId);
  if (!documentIds.length || documentIds.length > maxSelectedDocuments || !retrievalModes.includes(selection.mode)) return null;
  return selection.grounding === "documents" ? { mode: selection.mode, documentIds, grounding: "documents" } : { mode: selection.mode, documentIds };
}

export type Readiness = {
  /** Why Send and Regenerate are blocked, with the explicit fixes to offer. */
  blocked: { title: string; detail: string; fix: "select" | "model" | "index" } | null;
  notes: string[];
};

/**
 * Check a knowledge selection before Send or Regenerate. Dense modes need the
 * loaded model and at least one indexed selected document; the mode is never
 * changed here, only explained.
 */
export function knowledgeReadiness({ selection, embedding, coverage }: {
  selection: KnowledgeSelection;
  embedding: EmbeddingStatus | null;
  /** Aggregate coverage of the selected documents, or null while unknown. */
  coverage: IndexCoverage | null;
}): Readiness {
  if (!selection.enabled) return { blocked: null, notes: [] };
  if (!knowledgeOption(selection)) {
    return {
      blocked: { title: "Select documents to search", detail: "Choose at least one document, or turn knowledge off.", fix: "select" },
      notes: [],
    };
  }
  if (selection.mode === "keyword") return { blocked: null, notes: [] };
  const name = modeLabel[selection.mode];
  if (embedding?.restore === "pending") {
    return selection.grounding === "documents"
      ? { blocked: { title: "The semantic model is still being verified", detail: "Wait a moment, or switch to Keyword.", fix: "model" }, notes: [] }
      : { blocked: null, notes: ["The remembered semantic model is still being verified. Word matches are used until it's ready."] };
  }
  const strict = selection.grounding === "documents";
  // Automatic grounding never blocks Chat on the dense model or index; the
  // host ranks by keyword instead and reports it in coverage.
  const advise = (readiness: Readiness): Readiness => strict || !readiness.blocked ? readiness : {
    blocked: null,
    notes: [`${readiness.blocked.title}. Word matches are used instead until it's available.`, ...readiness.notes],
  };
  if (embedding && !embedding.supported) {
    return advise({ blocked: { title: `${name} search isn't available on this system`, detail: "Switch to Keyword.", fix: "model" }, notes: [] });
  }
  if (!embedding?.available) {
    return advise({
      blocked: { title: "The semantic search model isn't loaded", detail: `${name} search needs it. Choose the model in Knowledge, or switch to Keyword.`, fix: "model" },
      notes: [],
    });
  }
  if (!coverage) return { blocked: null, notes: [] };
  const total = coverage.indexedDocuments + coverage.missingDocuments + coverage.incompatibleDocuments;
  if (coverage.indexedDocuments === 0 && total > 0) {
    return advise({
      blocked: {
        title: total === 1 ? "The selected document has no usable semantic index" : "None of the selected documents has a usable semantic index",
        detail: "Build the semantic index in Knowledge, or switch to Keyword.",
        fix: "index",
      },
      notes: [],
    });
  }
  const unindexed = coverage.missingDocuments + coverage.incompatibleDocuments;
  if (!unindexed) return { blocked: null, notes: [] };
  return {
    blocked: null,
    notes: [selection.mode === "semantic"
      ? `${plural(unindexed, "selected document")} ${unindexed === 1 ? "has" : "have"} no usable semantic index and won't be searched.`
      : `${plural(unindexed, "selected document")} ${unindexed === 1 ? "has" : "have"} no usable semantic index, so only word matches are used for ${unindexed === 1 ? "it" : "them"}.`],
  };
}

/** Status line and guidance for the semantic search model, including launch restore (Task 06-A). */
export type EmbeddingCopy = {
  tone: "ready" | "busy" | "idle" | "warning" | "error" | "unsupported";
  label: string;
  detail: string;
  /** The primary action to offer: choose a file or unload the loaded model. */
  action: "choose" | "unload" | null;
};

export function embeddingCopy(status: EmbeddingStatus | null, action: "choosing" | "unloading" | null): EmbeddingCopy {
  if (action === "choosing") return { tone: "busy", label: "Verifying and loading…", detail: "Checking the file's checksum before use.", action: null };
  if (action === "unloading") return { tone: "busy", label: "Unloading…", detail: "Saved indexes are kept.", action: null };
  if (status === null) return { tone: "busy", label: "Checking…", detail: "", action: null };
  if (!status.supported) {
    return { tone: "unsupported", label: "Not available on this system", detail: "Keyword search works for every document.", action: null };
  }
  const remembered = "The app remembers the file you choose and verifies it at each launch. Semantic indexes are saved and reused.";
  if (status.restore === "pending") {
    return { tone: "busy", label: "Restoring…", detail: "Verifying the remembered model file. Keyword search works meanwhile.", action: null };
  }
  if (status.available) {
    return {
      tone: "ready",
      label: status.restore === "restored" ? "Ready · restored at launch"
        : status.restore === "selected" ? "Ready · remembered for next launch" : "Ready",
      detail: "Unloading frees memory and keeps the file remembered, but it won't load at the next launch until you choose it again.",
      action: "unload",
    };
  }
  switch (status.restore) {
    case "unloaded":
      return { tone: "idle", label: "Unloaded", detail: "You unloaded it, so it isn't loaded at launch. Choose the file to use semantic search again.", action: "choose" };
    case "missing":
      return {
        tone: "warning", label: "Remembered file not found",
        detail: "The remembered model file isn't readable right now, for example on a disconnected drive. Reconnect it and restart the app, or choose the file again.",
        action: "choose",
      };
    case "incompatible":
      return {
        tone: "error", label: "Remembered file no longer matches",
        detail: "The file changed and failed verification, so the app forgot it. Choose the supported BGE-small-en-v1.5 F16 GGUF.",
        action: "choose",
      };
    case "failed":
      return { tone: "error", label: "Couldn't restore", detail: "The remembered model couldn't be loaded. Choose the file again.", action: "choose" };
    default:
      return { tone: "idle", label: "Not loaded", detail: `Choose the BGE-small-en-v1.5 F16 GGUF once. ${remembered}`, action: "choose" };
  }
}

/** One-line headline for how a contextual attempt used the selected documents (Task 06-A intents). */
export function coverageSummary(coverage: KnowledgeCoverage | null | undefined): { title: string; detail: string | null; tone: "neutral" | "accent" | "warning" } | null {
  if (!coverage) return null;
  const fallback = coverage.requestedMode && coverage.requestedMode !== coverage.mode
    ? `${modeLabel[coverage.requestedMode]} requested · ${modeLabel[coverage.mode]} used` : null;
  const ranked = fallback ?? `${modeLabel[coverage.mode]} retrieval`;
  switch (coverage.intent) {
    case "general":
      return { title: "Ordinary Chat · no document evidence", detail: fallback ?? "No selected passage was relevant", tone: "neutral" };
    case "search":
      return coverage.retrieved
        ? { title: `Exact search · ${plural(coverage.retrieved, "match", "matches")}`, detail: "Literal text in the selected documents", tone: "accent" }
        : { title: "Exact search · no match", detail: "The text wasn't found in the selected documents", tone: "warning" };
    case "overview": {
      const supplied = (coverage.overview ?? []).reduce((sum, item) => sum + item.suppliedChunks, 0);
      const total = (coverage.overview ?? []).reduce((sum, item) => sum + item.totalChunks, 0);
      if (!total) return { title: "Overview · no readable text", detail: null, tone: "warning" };
      return supplied < total
        ? { title: `Partial overview · ${supplied.toLocaleString("en-US")} of ${plural(total, "passage")}`, detail: "Read directly, spread across the selected documents", tone: "warning" }
        : { title: `Overview · all ${plural(total, "passage")}`, detail: "Read directly from the selected documents", tone: "accent" };
    }
    case "question":
    default:
      return coverage.retrieved
        ? { title: `${plural(coverage.retrieved, "relevant passage")}`, detail: ranked, tone: "accent" }
        : { title: "No relevant passages", detail: coverage.grounding === "documents" ? `${ranked} · answer limited to the documents` : ranked, tone: "warning" };
  }
}

/** Live preparation status after the knowledge event arrives. */
export function preparationLabel(coverage: KnowledgeCoverage): string {
  switch (coverage.intent) {
    case "general": return "No relevant passages. Answering as ordinary Chat…";
    case "search": return coverage.retrieved ? `Found ${plural(coverage.retrieved, "exact match", "exact matches")}. Starting the answer…` : "No exact match. Starting the answer…";
    case "overview": return `Reading ${plural(coverage.retrieved, "passage")} for an overview…`;
    default: return coverage.retrieved
      ? `Found ${plural(coverage.retrieved, "matching passage")}. Starting the answer…` : "No matching passages. Starting the answer…";
  }
}

/** Plain-language lines describing what a contextual attempt searched and used. */
export function coverageLines(
  coverage: KnowledgeCoverage | null | undefined,
  usage: ContextUsage | null | undefined,
  documentName: (id: number) => string | null,
  /** Skip lines that `coverageSummary` already states as the headline. */
  omitHeadline = false,
): string[] {
  const lines: string[] = [];
  if (coverage) {
    const intent = coverage.intent;
    const headline = omitHeadline && intent !== undefined;
    if (headline) {
      if (intent === "question" && coverage.retrieved && usage && usage.sources < coverage.retrieved) {
        lines.push(`${plural(coverage.retrieved, "passage")} matched; ${usage.sources.toLocaleString("en-US")} fit in the model's context.`);
      }
    } else if (coverage.requestedMode && coverage.requestedMode !== coverage.mode) {
      lines.push(`${modeLabel[coverage.requestedMode]} search wasn't available, so word matches were used.`);
    }
    if (headline) {
      // Stated by the headline.
    } else if (intent === "general") {
      lines.push("No passage in the selected documents was relevant, so this was answered as ordinary Chat without document evidence.");
    } else if (intent === "overview" && coverage.overview?.length) {
      const supplied = coverage.overview.reduce((sum, item) => sum + item.suppliedChunks, 0);
      const total = coverage.overview.reduce((sum, item) => sum + item.totalChunks, 0);
      lines.push(supplied >= total
        ? `Overview read all ${plural(total, "passage")} of the selected documents.`
        : `Overview read ${supplied.toLocaleString("en-US")} of ${plural(total, "passage")}, spread across the selected documents.`);
    } else if (coverage.retrieved === 0) {
      lines.push(intent === "search" ? "The search text wasn't found in the selected documents."
        : "No passages in the selected documents matched this question.");
    } else if (usage && usage.sources < coverage.retrieved) {
      lines.push(`${plural(coverage.retrieved, "passage")} matched; ${usage.sources.toLocaleString("en-US")} fit in the model's context.`);
    }
    const names = (ids: number[]) => ids.map((id) => {
      const name = documentName(id);
      return name ? `“${name}”` : `document #${id}`;
    }).join(", ");
    if (coverage.partialDocumentIds.length) {
      lines.push(`Only part of the text was searchable in ${names(coverage.partialDocumentIds)}.`);
    }
    if (coverage.unavailableDocumentIds.length) {
      lines.push(`${plural(coverage.unavailableDocumentIds.length, "selected document")} ${coverage.unavailableDocumentIds.length === 1 ? "is" : "are"} no longer in the library.`);
    }
    if (coverage.incompatibleIndexDocumentIds?.length) {
      lines.push(`The semantic index was incompatible for ${names(coverage.incompatibleIndexDocumentIds)}.`);
    }
    if ((coverage.unsearchedDocumentIds?.length ?? 0) > coverage.unavailableDocumentIds.length
      + (coverage.mode === "semantic" ? (coverage.embeddings?.missingDocuments ?? 0) + (coverage.embeddings?.incompatibleDocuments ?? 0) : 0)) {
      lines.push(`${plural(coverage.unsearchedDocumentIds!.length, "selected document")} skipped during retrieval.`);
    }
    const dense = coverage.embeddings;
    const unindexed = dense ? dense.missingDocuments + dense.incompatibleDocuments : 0;
    if (dense && unindexed > 0) {
      lines.push(coverage.mode === "hybrid"
        ? `${plural(unindexed, "document")} had no usable semantic index, so only word matches were used for ${unindexed === 1 ? "it" : "them"}.`
        : `${plural(unindexed, "document")} had no usable semantic index and ${unindexed === 1 ? "wasn't" : "weren't"} searched.`);
    }
  }
  return lines;
}

export function contextUsageLabel(usage: ContextUsage | null | undefined): string | null {
  if (!usage) return null;
  const parts: string[] = [];
  // A memory-only answer had no document search, so don't report "0 sources".
  if (usage.sources > 0 || usage.memories === 0) parts.push(plural(usage.sources, "passage"));
  if (usage.memories > 0) parts.push(plural(usage.memories, "memory", "memories"));
  return `Supplied to the model: ${parts.join(" · ")}`;
}

/** The attempt marker is available even when no generation stats were saved. */
export function isContextual(message: { context?: AttemptContext | null; stats?: { contextUsage?: ContextUsage | null } | null } | null | undefined): boolean {
  return !!(message?.context || message?.stats?.contextUsage);
}

// ---------------------------------------------------------------- citations
type MdNode = { type: string; value?: string; children?: MdNode[]; data?: Record<string, unknown> };

const citationPattern = /\[(S\d{1,2}(?:\s*[,;]\s*S\d{1,2})*)\]/g;

/** Split text into plain strings and runs of validated citation IDs. */
export function splitCitations(text: string, valid: ReadonlySet<string>): (string | { ids: string[]; raw: string })[] {
  const parts: (string | { ids: string[]; raw: string })[] = [];
  let last = 0;
  for (const match of text.matchAll(citationPattern)) {
    const ids = match[1].split(/\s*[,;]\s*/);
    // A marker links only when every ID in it was returned for this message.
    if (!ids.every((id) => valid.has(id))) continue;
    if (match.index > last) parts.push(text.slice(last, match.index));
    parts.push({ ids, raw: match[0] });
    last = match.index + match[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

const skipped = new Set(["code", "inlineCode", "html", "link", "linkReference", "definition"]);

/**
 * remark plugin: wrap validated citation markers in a span with
 * `data-citation`. Unknown markers, code and links stay unchanged.
 */
export function remarkCitations(options: { ids: ReadonlySet<string> }) {
  const valid = options?.ids ?? new Set<string>();
  const walk = (node: MdNode) => {
    if (!node.children || skipped.has(node.type)) return;
    const next: MdNode[] = [];
    for (const child of node.children) {
      if (child.type === "text" && typeof child.value === "string" && valid.size) {
        for (const part of splitCitations(child.value, valid)) {
          if (typeof part === "string") next.push({ type: "text", value: part });
          else {
            next.push({
              type: "citation",
              data: { hName: "span", hProperties: { dataCitation: part.ids.join(",") } },
              children: [{ type: "text", value: part.raw }],
            });
          }
        }
      } else {
        walk(child);
        next.push(child);
      }
    }
    node.children = next;
  };
  return (tree: MdNode) => walk(tree);
}

// ------------------------------------------------------------------- memory
export function memoryInputError(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return "Enter something to remember.";
  if (utf8Bytes(trimmed) > memoryMaxBytes) return `Keep each memory within ${memoryMaxBytes.toLocaleString("en-US")} bytes.`;
  return null;
}

export function memoryQueryValid(query: string): boolean {
  return utf8Bytes(query.trim()) <= maxQueryBytes;
}

export function knowledgeQueryValid(query: string): boolean {
  const trimmed = query.trim();
  return !!trimmed && /[\p{L}\p{N}]/u.test(trimmed) && utf8Bytes(trimmed) <= maxQueryBytes;
}

/** Merge a page into a descending-ID list without duplicates. */
export function mergePage<T extends { id: number }>(current: readonly T[], page: readonly T[]): T[] {
  const seen = new Set(current.map(({ id }) => id));
  return [...current, ...page.filter(({ id }) => !seen.has(id))].sort((a, b) => b.id - a.id);
}

// ---------------------------------------------------------------- ownership
/**
 * Hands out request tokens; only the newest token is current. Used to drop
 * stale IPC responses after the inputs that produced them changed.
 */
export function createLatest() {
  let current = 0;
  return {
    next: () => ++current,
    isCurrent: (token: number) => token === current,
    invalidate: () => { current++; },
  };
}

/** Whether a Chat event belongs to the request and conversation that started it. */
export function ownsChatEvent(owner: { generation: number; conversationId: number }, current: { generation: number; conversationId: number | null }): boolean {
  return owner.generation === current.generation && owner.conversationId === current.conversationId;
}

/**
 * What to show for a cited source. Unavailable references are never resolved,
 * and a null resolution means the original revision is gone.
 */
export function citationState(citation: Pick<SourceCitation, "available">, resolved?: string | null): "unavailable" | "ready" | "resolve" {
  if (!citation.available || resolved === null) return "unavailable";
  return typeof resolved === "string" ? "ready" : "resolve";
}
