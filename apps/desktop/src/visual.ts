// Visual input workflows (Stage 04-E): camera snapshots, crop selection,
// shared OCR languages and visual-assisted search. Types mirror the native
// host; the host and engine re-check every rule here.

export type Region = { x: number; y: number; width: number; height: number };

export type StreamRecord = {
  library: string; format: string; width: number; height: number; frames: number;
  rejectedBuffers: number; settled: string; firstFrameMs: number; waitedMs: number;
};

export type CameraRecord = {
  source: string; portalVersion: number; camera: string; camerasOffered: number; consentMs: number;
  stream: StreamRecord; releasedMs: number; authorization: string; storage: string;
};

export type CameraNode = { key: string; name: string };
export type PendingSnapshot = { id: string; width: number; height: number; record: CameraRecord };
export type Taken =
  | { kind: "snapshot"; snapshot: PendingSnapshot }
  | { kind: "choose"; cameras: CameraNode[] };

/** Camera progress the main window receives while a snapshot runs. */
export type CameraStage = "requesting" | "active" | "released";

export type VisualSettings = { ocrLanguages: string[]; translationTarget: string };

export type SearchProvider = "brave" | "searxng" | "purili";
export type SearchState = { provider: SearchProvider | null; name: string | null; personalUseOnly: boolean };
export type EgressRecord = { destination: string; origin?: string; transmission: string; labels: string[]; bodyBytes?: number };
export type VisualSearchResult = { title: string; url: string; host: string; snippet: string; published: string | null };
export type VisualSearch = {
  provider: SearchProvider; providerName: string; query: string; results: VisualSearchResult[]; egress: EgressRecord[];
};

export const maxOcrLanguages = 4;
export const maxQueryChars = 400;
export const maxQueryWords = 50;

/** The selection drawn over a preview of `view` size, in frame pixels.
 * Mirrors the native mapping: edges round outward, then clamp; at least
 * 4 × 4 pixels. `null` means no usable selection (use the whole image). */
export function selectionRegion(
  start: { x: number; y: number }, end: { x: number; y: number },
  view: { width: number; height: number }, frame: { width: number; height: number },
): Region | null {
  if (!(view.width > 0 && view.height > 0) || ![start.x, start.y, end.x, end.y].every(Number.isFinite)) return null;
  const sx = frame.width / view.width;
  const sy = frame.height / view.height;
  const clamp = (value: number, limit: number) => Math.min(Math.max(value, 0), limit);
  const left = Math.floor(clamp(Math.min(start.x, end.x) * sx, frame.width));
  const top = Math.floor(clamp(Math.min(start.y, end.y) * sy, frame.height));
  const right = Math.ceil(clamp(Math.max(start.x, end.x) * sx, frame.width));
  const bottom = Math.ceil(clamp(Math.max(start.y, end.y) * sy, frame.height));
  const region = { x: left, y: top, width: right - left, height: bottom - top };
  return region.width >= 4 && region.height >= 4 ? region : null;
}

/** A starting search query from recognized or selected text: whitespace
 * collapsed and cut to the provider-neutral limits. The user edits it. */
export function suggestQuery(text: string): string {
  const words = text.split(/\s+/u).filter(Boolean).slice(0, maxQueryWords);
  let query = "";
  for (const word of words) {
    const next = query ? `${query} ${word}` : word;
    if (Array.from(next).length > maxQueryChars) break;
    query = next;
  }
  return query;
}

/** Why a query cannot be sent yet, or `null`. */
export function queryProblem(query: string): string | null {
  const text = query.split(/\s+/u).filter(Boolean).join(" ");
  if (!text) return "Enter something to search for.";
  if (Array.from(text).length > maxQueryChars || text.split(" ").length > maxQueryWords
    || /[\u0000-\u001f\u007f-\u009f]/u.test(text)) {
    return `Shorten the search to at most ${maxQueryChars} characters and ${maxQueryWords} words.`;
  }
  return null;
}

/** Why visual-assisted search is unavailable, or `null`. */
export function searchUnavailable(search: SearchState | undefined): string | null {
  if (!search?.provider) return "Choose a search provider in the main window under Settings → Web search to search the web.";
  return null;
}

/** The disclosure shown before a visual-assisted search is authorized. */
export function searchDisclosure(search: SearchState, described: boolean): string {
  const name = search.name ?? "the search provider";
  return `Only the query text above is sent to ${name}. No screenshot, camera image, document, memory or conversation is sent.`
    + (described ? " The query includes text a model wrote about the image." : "")
    + " This is a text search using visual clues, not reverse image matching."
    + (search.personalUseOnly ? " Purili is for personal, non-commercial use, and its results are not saved." : "");
}

/** Toggle one OCR language, keeping at most four in the chosen order. */
export function toggleLanguage(current: string[], code: string, on: boolean): string[] {
  if (!on) return current.filter(item => item !== code);
  if (current.includes(code) || current.length >= maxOcrLanguages) return current;
  return [...current, code];
}

/** A short receipt line for a camera snapshot. */
export function cameraSummary(record: CameraRecord): string {
  const camera = record.camera || "camera";
  const offered = record.camerasOffered > 1 ? ` (one of ${record.camerasOffered})` : "";
  return `${camera}${offered} · ${record.stream.width} × ${record.stream.height} ${record.stream.format} · camera released after ${record.releasedMs} ms`;
}

export const cameraStageText: Record<CameraStage, string> = {
  requesting: "Asking for camera access. Your desktop may show a permission dialog.",
  active: "Camera on: taking one snapshot.",
  released: "Camera off.",
};

/** Human labels for data classes in visual receipts. */
export const visualClassNames: Record<string, string> = {
  "core.user-input/1": "Your text",
  "core.query/1": "Search query",
  "core.clipboard-text/1": "Clipboard text",
  "core.screen-image/1": "Screen image",
  "core.screen-text/1": "Text recognized from the screen",
  "core.camera-image/1": "Camera image",
  "core.camera-text/1": "Text recognized from a camera snapshot",
  "core.attachment/1": "Attachment",
  "core.attachment-text/1": "Attachment text",
  "core.attachment-image/1": "Attached image",
  "core.model-output/1": "Model output",
  "core.memory/1": "Saved memories",
  "core.library/1": "Knowledge documents",
};

export const previewUrl = (id: string) => `camera-preview://localhost/${id}.png`;
