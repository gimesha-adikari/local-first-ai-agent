// Pure Agent presentation logic. Keep this file free of React and Tauri imports
// so the Node test runner can exercise it directly.
import type {
  AgentEvent, ApprovalTarget, BudgetUsage, Execution, FinishedEvent, IpcError, NativeResult,
  TaskState, ToolId,
} from "./agentTypes";

export const defaultTaskTimeoutSeconds = 300;
export const maxTaskTimeoutSeconds = 600;

const toolLabels: Record<string, string> = {
  "math.calculate": "Calculator",
  "device.info": "Device information",
  "files.search": "File search",
  "files.read": "File preview",
  "apps.list": "Application discovery",
  "apps.launch": "Application launch",
};

const toolActive: Record<string, string> = {
  "math.calculate": "Calculating",
  "device.info": "Reading device information",
  "files.search": "Searching selected files",
  "files.read": "Reading selected file",
  "apps.list": "Looking up installed applications",
  "apps.launch": "Launching application",
};

const toolDone: Record<string, string> = {
  "math.calculate": "Calculated with the local calculator",
  "device.info": "Read device information",
  "files.search": "Searched selected files",
  "files.read": "Read selected file",
  "apps.list": "Looked up installed applications",
  "apps.launch": "Sent launch request",
};

export const toolLabel = (tool: string) => toolLabels[tool] ?? "Unknown tool";

/** Describe a durable execution record without overstating its outcome. */
export function executionLabel(record: Pick<Execution, "tool" | "status" | "error">): string {
  const name = toolLabel(record.tool);
  const reason = record.error ? ` (${humanize(record.error).toLowerCase()})` : "";
  switch (record.status) {
    case "succeeded": return toolDone[record.tool] ?? `${name} succeeded`;
    case "failed": return `${name} failed${reason}`;
    case "uncertain": return `${name} outcome is uncertain${reason}`;
    case "not_dispatched": return `${name} was not run${reason}`;
    default: return `${name}: ${humanize(record.status).toLowerCase()}${reason}`;
  }
}

const stateLabels: Record<TaskState, string> = {
  created: "Created",
  planning: "Planning",
  validating: "Validating",
  awaiting_authorization: "Awaiting authorization",
  authorized: "Authorized",
  executing: "Executing",
  observing: "Observing",
  completed: "Completed",
  denied: "Denied",
  cancelled: "Cancelled",
  failed: "Failed",
  timed_out: "Timed out",
  interrupted: "Interrupted",
};

export const stateLabel = (state: string) => stateLabels[state as TaskState] ?? humanize(state);

export function humanize(value: string): string {
  const text = value.replaceAll("_", " ").replaceAll(".", " ").trim();
  return text ? text[0].toUpperCase() + text.slice(1) : "Unknown";
}

export function isTerminal(state: string): boolean {
  return ["completed", "denied", "cancelled", "failed", "timed_out", "interrupted"].includes(state);
}

/** Merge native results by operation ID; a later delivery replaces an earlier one. */
export function mergeNativeResults(current: NativeResult[], incoming: readonly NativeResult[] | undefined): NativeResult[] {
  if (!incoming?.length) return current;
  const next = [...current];
  for (const item of incoming) {
    if (!item?.record || typeof item.record.operationId !== "string" || !item.observation) continue;
    const index = next.findIndex((existing) => existing.record.operationId === item.record.operationId);
    if (index === -1) next.push(item);
    else next[index] = item;
  }
  return next;
}

export type ProgressStep = {
  key: string;
  label: string;
  status: "active" | "done" | "failed" | "waiting" | "notice";
};

/**
 * Condense the low-level event stream into the few meaningful steps a person
 * cares about. Raw events remain available separately in the activity log.
 */
export function progressSteps(events: readonly AgentEvent[], running: boolean): ProgressStep[] {
  const steps: ProgressStep[] = [];
  const byOperation = new Map<string, number>();
  const upsert = (key: string, step: ProgressStep) => {
    const index = byOperation.get(key);
    if (index === undefined) {
      byOperation.set(key, steps.length);
      steps.push(step);
    } else steps[index] = step;
  };

  for (const event of events) {
    switch (event.kind) {
      case "proposal_rejected":
        steps.push({
          key: `rejected-${steps.length}`,
          label: "A proposed step failed validation and was not run",
          status: "notice",
        });
        break;
      case "dispatch_started":
        upsert(event.operationId, { key: event.operationId, label: toolActive[event.tool] ?? "Running a tool", status: "active" });
        break;
      case "approval_required":
        upsert(event.operationId, {
          key: event.operationId,
          label: event.target ? `Waiting for approval to launch ${event.target.displayName}` : "Waiting for your approval",
          status: "waiting",
        });
        break;
      case "execution": {
        const { record } = event;
        upsert(record.operationId, {
          key: record.operationId,
          label: executionLabel(record),
          status: record.status === "succeeded" ? "done" : record.status === "uncertain" || record.status === "not_dispatched" ? "notice" : "failed",
        });
        break;
      }
      case "observation": {
        const ok = event.observation.status === "success";
        upsert(event.record.operationId, {
          key: event.record.operationId,
          label: ok ? toolDone[event.record.tool] ?? "Completed a tool step" : `${toolLabel(event.record.tool)} failed`,
          status: ok ? "done" : "failed",
        });
        break;
      }
      case "finished":
        for (const step of steps) {
          if (step.status !== "waiting") continue;
          step.status = "notice";
          if (event.error?.code === "approval_declined") {
            step.label = step.label.replace(/^Waiting for (your )?approval( to)?/, "You declined$2");
          } else if (event.error?.code === "approval_expired") {
            step.label = "Application launch approval expired";
          } else {
            step.label = `${step.label.replace(/^Waiting for/, "No answer to")} (${stateLabel(event.state).toLowerCase()})`;
          }
        }
        break;
      default:
        break;
    }
  }

  if (!running) {
    for (const step of steps) if (step.status === "active" || step.status === "waiting") step.status = "notice";
  }
  return steps;
}

/** A short present-tense description of what the task is doing right now. */
export function currentActivity(events: readonly AgentEvent[]): string {
  let tool: ToolId | null = null;
  for (let index = events.length - 1; index >= 0; index--) {
    const event = events[index];
    if (event.kind === "dispatch_started" || event.kind === "tool_proposed") { tool = event.tool; break; }
  }
  for (let index = events.length - 1; index >= 0; index--) {
    const event = events[index];
    if (event.kind === "approval_required") {
      const answered = events.slice(index + 1).some((later) =>
        (later.kind === "execution" || later.kind === "observation") && later.record.operationId === event.operationId
        || later.kind === "state_changed" && later.state !== "awaiting_authorization");
      if (!answered) return "Waiting for your approval";
    }
    if (event.kind !== "state_changed") continue;
    switch (event.state) {
      case "created": return "Starting";
      case "planning": return "Working out the next step";
      case "validating": return "Checking the proposed step";
      case "awaiting_authorization": return "Checking permissions";
      case "authorized": return "Preparing the tool";
      case "executing": return tool ? toolActive[tool] ?? "Running a tool" : "Running a tool";
      case "observing": return "Reviewing the result";
      default: return stateLabel(event.state);
    }
  }
  return "Starting";
}

export function eventTitle(event: AgentEvent): string {
  switch (event.kind) {
    case "started": return "Task started";
    case "state_changed": return stateLabel(event.state);
    case "tool_proposed": return `Proposed ${toolLabel(event.tool)}`;
    case "proposal_rejected": return `Proposal rejected: ${humanize(event.code)}`;
    case "dispatch_started": return `Dispatched ${toolLabel(event.tool)}`;
    case "approval_required": return `Approval requested for ${toolLabel(event.tool)}`;
    case "execution": return `${toolLabel(event.record.tool)}: ${humanize(event.record.status)}`;
    case "observation": return `Native result: ${toolLabel(event.record.tool)}`;
    case "finished": return `Finished: ${stateLabel(event.state)}`;
    case "failed": return "Task failed";
  }
}

export function savedEventTitle(event: Record<string, unknown>): string {
  const kind = typeof event.kind === "string" ? event.kind : "event";
  if (kind === "transition" && event.transition && typeof event.transition === "object") {
    const item = event.transition as Record<string, unknown>;
    return `${stateLabel(String(item.from ?? ""))} → ${stateLabel(String(item.to ?? ""))}`;
  }
  if (kind === "dispatch_intent") return `Dispatch: ${toolLabel(String(event.tool ?? ""))}`;
  if (kind === "execution" && event.record && typeof event.record === "object") {
    const item = event.record as Record<string, unknown>;
    return `${toolLabel(String(item.tool ?? ""))}: ${humanize(String(item.status ?? "result"))}`;
  }
  if (kind === "recovered_interrupted") return "Recovered after an interrupted session";
  return humanize(kind);
}

export type Guidance = { title: string; detail: string };

/** Fixed, specific copy for native PDF reader failures (Task 03-A handoff). */
export const pdfGuidance: Record<string, Guidance> = {
  pdf_encrypted: {
    title: "The PDF is encrypted",
    detail: "Select an authorized copy without password protection, then ask again.",
  },
  pdf_no_text: {
    title: "No extractable text on these pages",
    detail: "The selected pages have no text layer, as in scanned images. OCR isn't available in this app.",
  },
  pdf_invalid: {
    title: "The PDF is invalid or damaged",
    detail: "Open it in a PDF viewer to check it, or select a repaired copy.",
  },
  pdf_unsupported: {
    title: "The PDF uses an unsupported feature",
    detail: "The local extractor can't handle this document's encoding or structure.",
  },
  pdf_limit: {
    title: "The PDF exceeded a reader limit",
    detail: "The file, page count or extracted text was too large. Ask for a smaller page range of up to four pages.",
  },
  pdf_unavailable: {
    title: "PDF reading is unavailable",
    detail: "The local PDF tools or Linux sandbox aren't available on this system. Text files can still be read.",
  },
  invalid_arguments: {
    title: "The request had invalid arguments",
    detail: "For PDFs, ask for pages that exist in the document, at most four per request.",
  },
};


const guidance: Record<string, Guidance> = {
  model_context_limit: {
    title: "The task didn't fit in the model's context",
    detail: "Try a shorter request, or unload the model and reload it with a larger context length.",
  },
  decision_output_limit: {
    title: "The model's reply was too long",
    detail: "Ask for a shorter answer or a narrower result.",
  },
  invalid_arguments: {
    title: "The model proposed a step that failed validation",
    detail: "Nothing was run for that step. Rephrase the request more specifically. Qwen3.5-4B is recommended for Agent tasks.",
  },
  invalid_proposal: {
    title: "The model's proposal couldn't be understood",
    detail: "Nothing was run for that step. Rephrase the request more specifically. Qwen3.5-4B is recommended for Agent tasks.",
  },
  unknown_tool: {
    title: "The model asked for a tool that isn't available",
    detail: "Nothing was run for that step. Rephrase the request using one of the available capabilities.",
  },
  permission_denied: {
    title: "Access was not granted",
    detail: "If the task needs a file or folder, select it with the + button, then try again.",
  },
  scope_kind_mismatch: {
    title: "The model used the wrong kind of file access",
    detail: "Keep the selected file and ask to read it directly, or select a folder to search it.",
  },
  timed_out: {
    title: "The task ran out of time",
    detail: "Increase the task time limit in composer options, or simplify the request.",
  },
  approval_expired: {
    title: "Application approval expired",
    detail: "Nothing was launched. Ask again in a follow-up if you still want to open the application; it will need a new approval.",
  },
  cancelled: { title: "Task cancelled", detail: "Any step that already ran is shown above." },
  budget_exhausted: {
    title: "The task reached its step budget",
    detail: "Split the request into smaller follow-up requests.",
  },
  tool_failed: { title: "A native tool failed", detail: "Check the native result above for the reason." },
  model_failed: {
    title: "The model didn't produce a usable answer",
    detail: "Verified native results, if any, are shown above. You can ask again in a follow-up.",
  },
  busy: { title: "Another operation is running", detail: "Wait for it to finish, then try again." },
  storage_failed: {
    title: "The task journal couldn't be saved",
    detail: "Don't rely on this task's history as a record of what ran.",
  },
  unsupported_platform: {
    title: "Not supported on this platform",
    detail: "This capability is available on Linux only.",
  },
  ...pdfGuidance,
};

export function errorGuidance(error: IpcError | null | undefined, state?: string): Guidance {
  if (state === "denied" && error?.code === "approval_declined") {
    return { title: "You declined the request", detail: "The action you declined was not run." };
  }
  const known = error?.code ? guidance[error.code] : undefined;
  return {
    title: known?.title ?? "The task failed",
    detail: [error?.message, known?.detail].filter(Boolean).join(" "),
  };
}

export function outcomeSummary(finished: Pick<FinishedEvent, "state" | "answer" | "error">, successfulResults: number): string {
  switch (finished.state) {
    case "completed":
      return finished.answer ? "Model replied" : "Ended without a written answer";
    case "denied":
      return finished.error?.code === "approval_declined" ? "You declined the request"
        : finished.error?.code === "scope_kind_mismatch" ? "Not run: the request didn't match the selection" : "Permission denied";
    case "cancelled": return "Cancelled";
    case "timed_out": return finished.error?.code === "approval_expired" ? "Approval expired" : "Timed out";
    case "interrupted": return "Interrupted";
    case "failed":
      return successfulResults > 0 ? "Native step succeeded, answer failed" : "Failed";
    default:
      return stateLabel(finished.state);
  }
}

export function usageFrom(value: unknown): BudgetUsage | null {
  if (!value || typeof value !== "object") return null;
  const item = value as Record<string, unknown>;
  const read = (camel: string, snake: string) => {
    const raw = item[camel] ?? item[snake];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : null;
  };
  const toolCalls = read("toolCalls", "tool_calls");
  const modelCalls = read("modelCalls", "model_calls");
  const retries = read("retries", "retries");
  const generatedTokens = read("generatedTokens", "generated_tokens");
  if (toolCalls === null || modelCalls === null || retries === null || generatedTokens === null) return null;
  return { toolCalls, modelCalls, retries, generatedTokens };
}

export function formatBytes(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "Unknown";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) { size /= 1024; unit++; }
  return `${unit === 0 ? size : size.toFixed(size >= 100 ? 0 : 1)} ${units[unit]}`;
}

export function durationLabel(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds} ${seconds === 1 ? "second" : "seconds"}`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest ? `${minutes} min ${rest} s` : `${minutes} min`;
}

export function clock(seconds: number): string {
  const safe = Math.max(0, Math.floor(seconds));
  return `${Math.floor(safe / 60)}:${String(safe % 60).padStart(2, "0")}`;
}

export function remainingLabel(ms: number): string {
  if (ms <= 0) return "expired";
  if (ms < 60_000) return `${Math.max(1, Math.ceil(ms / 1000))} s left`;
  if (ms < 3_600_000) return `${Math.ceil(ms / 60_000)} min left`;
  const hours = Math.floor(ms / 3_600_000);
  const minutes = Math.round((ms % 3_600_000) / 60_000);
  return minutes ? `${hours} h ${minutes} min left` : `${hours} h left`;
}

export type DeviceRow = { label: string; value: string };

export type ResultView =
  | { kind: "calculator"; value: number }
  | { kind: "device"; rows: DeviceRow[] }
  | { kind: "file-search"; paths: string[]; truncated: boolean; extension: string | null }
  | { kind: "file-read"; content: string; fileBytes: number | null; truncated: boolean }
  | { kind: "pdf"; pdf: PdfView }
  | { kind: "apps"; applications: { appId: string; displayName: string }[]; truncated: boolean }
  | { kind: "launch"; appId: string; dispatched: boolean; target: ApprovalTarget | null }
  | { kind: "failure"; error: IpcError }
  | { kind: "unrecognized"; json: string };

export type PdfPage = { pageNumber: number; text: string; truncated: boolean };

export type PdfView = {
  pageCount: number;
  firstPage: number;
  lastPage: number;
  pages: PdfPage[];
  fileBytes: number | null;
  truncated: boolean;
};

const positiveInt = (value: unknown) => (typeof value === "number" && Number.isInteger(value) && value >= 1 ? value : null);

/**
 * Validate a native PDF result. It is recognized only by a well-formed `pages`
 * array whose page numbers lie inside the reported range; anything else is
 * not reinterpreted.
 */
export function pdfView(value: Record<string, unknown>): PdfView | null {
  if (!Array.isArray(value.pages)) return null;
  const pageCount = positiveInt(value.page_count);
  const firstPage = positiveInt(value.first_page);
  const lastPage = positiveInt(value.last_page);
  if (pageCount === null || firstPage === null || lastPage === null || firstPage > lastPage || lastPage > pageCount) return null;
  const pages: PdfPage[] = [];
  for (const entry of value.pages) {
    if (!isRecord(entry)) return null;
    const pageNumber = positiveInt(entry.page_number);
    if (pageNumber === null || pageNumber < firstPage || pageNumber > lastPage || typeof entry.text !== "string") return null;
    if (pages.length && pageNumber <= pages[pages.length - 1].pageNumber) return null;
    pages.push({ pageNumber, text: entry.text, truncated: entry.truncated === true });
  }
  return { pageCount, firstPage, lastPage, pages, fileBytes: num(value.file_bytes), truncated: value.truncated === true };
}

const pageRange = (first: number, last: number) => (first === last ? `Page ${first}` : `Pages ${first}–${last}`);

/** Explicit source coverage, e.g. "Pages 2–3 of 5". */
export function pdfCoverage(pdf: Pick<PdfView, "pageCount" | "firstPage" | "lastPage">): string {
  return `${pageRange(pdf.firstPage, pdf.lastPage)} of ${pdf.pageCount}`;
}

export type PdfWarning = { kind: "omitted" | "clipped" | "missing"; text: string };

/** Coverage and truncation warnings. Never implies the rest of the document was read. */
export function pdfWarnings(pdf: PdfView): PdfWarning[] {
  const warnings: PdfWarning[] = [];
  const unread = pdf.pageCount - (pdf.lastPage - pdf.firstPage + 1);
  if (unread > 0) {
    warnings.push({
      kind: "omitted",
      text: `${unread} of ${pdf.pageCount} ${pdf.pageCount === 1 ? "page was" : "pages were"} not read in this step. `
        + "Any answer covers only the pages shown here unless other steps read more.",
    });
  }
  const returned = new Set(pdf.pages.map((page) => page.pageNumber));
  const missing: number[] = [];
  for (let page = pdf.firstPage; page <= pdf.lastPage; page++) if (!returned.has(page)) missing.push(page);
  if (missing.length) {
    warnings.push({ kind: "missing", text: `No text was returned for ${missing.length === 1 ? "page" : "pages"} ${missing.join(", ")}.` });
  }
  const clipped = pdf.pages.filter((page) => page.truncated).map((page) => page.pageNumber);
  if (clipped.length) {
    warnings.push({
      kind: "clipped",
      text: `Text was clipped on ${clipped.length === 1 ? "page" : "pages"} ${clipped.join(", ")}. Only the beginning of ${clipped.length === 1 ? "that page" : "those pages"} is shown.`,
    });
  } else if (pdf.truncated && unread <= 0 && !missing.length) {
    warnings.push({ kind: "clipped", text: "The native reader reported that some text was omitted." });
  }
  return warnings;
}

/**
 * When an answer follows native PDF reads, state the source pages it can rely
 * on. Returns null when no PDF was read or every read covered its whole file.
 */
export function pdfAnswerScope(results: readonly NativeResult[]): string | null {
  const notes: string[] = [];
  let partial = false;
  for (const result of results) {
    const view = resultView(result);
    if (view.kind !== "pdf") continue;
    const { pdf } = view;
    const whole = pdf.firstPage === 1 && pdf.lastPage === pdf.pageCount && !pdf.truncated;
    if (!whole) partial = true;
    notes.push(pdfCoverage(pdf));
  }
  if (!notes.length || !partial) return null;
  return `The local reader returned ${notes.join("; ")}. Statements about other pages aren't backed by this turn's native results.`;
}

/** The extension filter actually applied by the native search, as display text. */
export function searchFilterLabel(extension: string | null): string {
  return extension ? `Extension filter: .${extension} (final suffix, any letter case)` : "No extension filter";
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === "object" && !Array.isArray(value);
const num = (value: unknown) => (typeof value === "number" && Number.isFinite(value) ? value : null);
const text = (value: unknown) => (typeof value === "string" ? value : null);

function stringify(value: unknown): string {
  try { return JSON.stringify(value, null, 2) ?? String(value); } catch { return String(value); }
}

/**
 * Turn an untrusted native observation into a typed display model. Unexpected
 * shapes fall back to a raw view rather than being reinterpreted.
 */
export function resultView(result: NativeResult, targets?: ReadonlyMap<string, ApprovalTarget>): ResultView {
  const { observation } = result;
  if (observation.status === "failure") {
    return { kind: "failure", error: observation.error ?? { code: "tool_failed", message: "The native tool failed." } };
  }
  const value = observation.result;
  const raw = (): ResultView => ({ kind: "unrecognized", json: stringify(value) });
  if (!isRecord(value)) return raw();

  switch (observation.tool) {
    case "math.calculate": {
      const result = num(value.value);
      return result === null ? raw() : { kind: "calculator", value: result };
    }
    case "device.info": {
      const memoryTotal = num(value.memory_total_bytes);
      const memoryAvailable = num(value.memory_available_bytes);
      const storageTotal = num(value.storage_total_bytes);
      const storageAvailable = num(value.storage_available_bytes);
      const cpus = num(value.logical_cpus);
      const rows: DeviceRow[] = [
        { label: "Operating system", value: text(value.os) ?? "Not reported" },
        { label: "Architecture", value: text(value.architecture) ?? "Not reported" },
        { label: "Processor", value: text(value.cpu) ?? "Not reported" },
        { label: "Logical CPUs", value: cpus === null ? "Not reported" : String(cpus) },
        {
          label: "Memory",
          value: memoryTotal === null ? "Not reported"
            : memoryAvailable === null ? `${formatBytes(memoryTotal)} total`
              : `${formatBytes(memoryAvailable)} available of ${formatBytes(memoryTotal)}`,
        },
        {
          label: "Root storage",
          value: storageTotal === null ? "Not reported"
            : storageAvailable === null ? `${formatBytes(storageTotal)} total`
              : `${formatBytes(storageAvailable)} available of ${formatBytes(storageTotal)}`,
        },
        { label: "GPU", value: text(value.gpu) ?? "Not reported by this tool" },
      ];
      return { kind: "device", rows };
    }
    case "files.search": {
      if (!Array.isArray(value.paths)) return raw();
      return {
        kind: "file-search",
        paths: value.paths.filter((path): path is string => typeof path === "string"),
        truncated: value.truncated === true,
        extension: typeof value.extension === "string" && value.extension ? value.extension : null,
      };
    }
    case "files.read": {
      if (Array.isArray(value.pages)) {
        const pdf = pdfView(value);
        return pdf ? { kind: "pdf", pdf } : raw();
      }
      const content = text(value.content);
      if (content === null) return raw();
      return { kind: "file-read", content, fileBytes: num(value.file_bytes), truncated: value.truncated === true };
    }
    case "apps.list": {
      if (!Array.isArray(value.applications)) return raw();
      const applications = value.applications.flatMap((entry) => {
        if (!isRecord(entry)) return [];
        const appId = text(entry.app_id);
        const displayName = text(entry.display_name);
        return appId !== null && displayName !== null ? [{ appId, displayName }] : [];
      });
      return { kind: "apps", applications, truncated: value.truncated === true };
    }
    case "apps.launch": {
      const appId = text(value.app_id);
      if (appId === null || typeof value.dispatched !== "boolean") return raw();
      return {
        kind: "launch", appId, dispatched: value.dispatched,
        target: targets?.get(result.record.operationId) ?? null,
      };
    }
    default:
      return raw();
  }
}

export function formatNumber(value: number): string {
  if (Number.isInteger(value) && Math.abs(value) < 1e15) return value.toLocaleString("en-US");
  const fixed = Number(value.toPrecision(12));
  return Math.abs(fixed) >= 1e15 || (Math.abs(fixed) < 1e-6 && fixed !== 0)
    ? fixed.toExponential(6)
    : fixed.toLocaleString("en-US", { maximumFractionDigits: 10 });
}

export function validTimeout(seconds: number): boolean {
  return Number.isInteger(seconds) && seconds >= 1 && seconds <= maxTaskTimeoutSeconds;
}

export type { Execution };

export type TimelineEntry = { key: number; at: number; event: AgentEvent };

/** Session-only view of a task started in this window. Nothing here survives restart. */
export type LiveTask = {
  id: string;
  prompt: string;
  submittedAt: number;
  timeoutMs: number | null;
  startedPerf: number | null;
  endedAt: number | null;
  events: TimelineEntry[];
  nativeResults: NativeResult[];
  targets: Record<string, ApprovalTarget>;
  approval: (Extract<AgentEvent, { kind: "approval_required" }> & { expiresAt: number }) | null;
  finished: FinishedEvent | null;
  failure: IpcError | null;
};

export function createLiveTask(id: string, prompt: string, timeoutMs: number, submittedAt: number, perf: number): LiveTask {
  return {
    id, prompt, submittedAt, timeoutMs, startedPerf: perf, endedAt: null, events: [], nativeResults: [],
    targets: {}, approval: null, finished: null, failure: null,
  };
}

/**
 * Apply one host event to a live task. Native observations are kept even when a
 * later model step fails, and terminal results are merged by operation ID.
 */
export function applyAgentEvent(task: LiveTask, event: AgentEvent, entry: { key: number; at: number; perf: number }): LiveTask {
  if (event.taskId !== task.id) return task;
  const next: LiveTask = {
    ...task,
    events: [...task.events.slice(-199), { key: entry.key, at: entry.at, event }],
  };
  switch (event.kind) {
    case "started":
      next.timeoutMs = event.taskTimeoutMs;
      next.startedPerf = entry.perf;
      break;
    case "approval_required":
      next.approval = { ...event, expiresAt: entry.perf + event.expiresInMs };
      if (event.target) next.targets = { ...task.targets, [event.operationId]: event.target };
      break;
    case "execution":
      if (next.approval?.operationId === event.record.operationId) next.approval = null;
      break;
    case "observation":
      next.nativeResults = mergeNativeResults(task.nativeResults, [{ record: event.record, observation: event.observation }]);
      if (next.approval?.operationId === event.record.operationId) next.approval = null;
      break;
    case "finished":
      next.finished = event;
      next.nativeResults = mergeNativeResults(next.nativeResults, event.nativeResults);
      next.approval = null;
      next.endedAt = entry.at;
      break;
    case "failed":
      next.failure = event.error;
      next.approval = null;
      next.endedAt = entry.at;
      break;
    default:
      break;
  }
  return next;
}

export const successfulResults = (results: readonly NativeResult[]) =>
  results.filter((result) => result.observation.status === "success");

/**
 * Visible permission copy. Uses only the sanitized display name, kind, access
 * and state; the revocation reference never appears in labels.
 */
export function scopeSummary(
  scope: { displayName: string; kind: "file" | "directory"; access: string; state: string },
  remainingMs: number,
): string {
  const kind = scope.kind === "directory" ? "Folder" : "File";
  const access = scope.access === "read_only" ? "Read-only" : humanize(scope.access);
  const state = scope.state === "active" ? remainingLabel(remainingMs) : scope.state === "revoked" ? "Revoked" : "Expired";
  return `${scope.displayName} · ${kind} · ${access} · ${state}`;
}
