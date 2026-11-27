import { visualClassNames, type CameraRecord, type SearchState, type VisualSettings } from "./visual.ts";

/** Only the explicit selections for this run are passed to the native Glance session. */
export const recipes = {
  explain: "Explain", summarize: "Summarize", rewrite: "Rewrite", improve: "Improve writing",
  grammar: "Fix grammar", translate: "Translate", key_points: "Extract key points", custom: "Ask a question",
} as const;

export type Recipe = keyof typeof recipes;

export type GlanceProvider = {
  id: number; name: string; model: string; locality: string; retention: string; revision: string;
  sharing: { documents: boolean; memories: boolean }; imageInput?: boolean; retentionSummary?: string;
};

export type OcrState = {
  available: boolean; engine: string | null; languages: string[]; defaults: string[];
  guidance: string | null; missingRecommended: string[];
};

const languageNames: Record<string, string> = {
  eng: "English", spa: "Spanish", fra: "French", deu: "German", por: "Portuguese", ita: "Italian", rus: "Russian",
  ukr: "Ukrainian", ara: "Arabic", heb: "Hebrew", hin: "Hindi", ben: "Bengali", chi_sim: "Chinese (Simplified)",
  chi_tra: "Chinese (Traditional)", jpn: "Japanese", kor: "Korean", tha: "Thai", vie: "Vietnamese", tur: "Turkish",
  ind: "Indonesian", sin: "Sinhala", tam: "Tamil", nld: "Dutch", pol: "Polish", swe: "Swedish",
};

/** A readable name for Tesseract language data, falling back to its code. */
export function languageName(code: string): string {
  return languageNames[code] ?? code;
}

export type GlanceState = {
  localAvailable: boolean; localOnly: boolean; memoryEnabled: boolean; providers: GlanceProvider[];
  documents: { id: number; name: string }[]; ocr?: OcrState;
  /** The configured search provider for visual-assisted search (Stage 04-E). */
  search?: SearchState;
  /** Shared OCR languages and translation target (Stage 04-E). */
  visual?: VisualSettings;
};

export type Region = { x: number; y: number; width: number; height: number };
export type CaptureRecord = {
  source: string; portalVersion: number; persistMode: number; restoreToken: string; restoreTokenReturned: boolean;
  cursor: string; consentMs: number; sessionClosedMs: number; sessionCloseConfirmed: boolean; monitor: [number, number, number, number] | null;
  stream: { library: string; format: string; width: number; height: number; frames: number; rejectedBuffers: number; settled: string; firstFrameMs: number; waitedMs: number };
};
export type RegionRecord = { region: Region; frameWidth: number; frameHeight: number };
export type CropInfo = {
  id: string; width: number; height: number; bytes: number; imageEligible: boolean;
  region: RegionRecord; source: "screen" | "camera"; capture: CaptureRecord | null; camera: CameraRecord | null;
};
export type OcrRecord = { engine: string; languages: string[]; elapsedMs: number; characters: number; sandbox: string };
export type ScreenReceipt = {
  source: "screen" | "camera"; capture: CaptureRecord | null; camera: CameraRecord | null; region: RegionRecord;
  ocr: OcrRecord | null; imageAttached: boolean; fullFrame: string;
};
export type Estimate = {
  classes: string[]; inputTokens: number; maxOutputTokens: number; costMicros: number | null; currency: string | null; imageTokens: number;
};

export type GlanceInput = {
  text: string; recipe: Recipe; language?: string; question?: string;
  provider?: number; revision?: string; share: boolean; memory: boolean; documents: number[];
  image?: boolean; crop?: string;
};

export type GlanceReceipt = {
  state: string; capability: string; application_retention: string;
  provider: { id: string; locality: string; retention: string } | null;
  used: { kind: string; record?: number | null }[]; withheld: { kind: string; record?: number | null }[];
  external: { origin: string; transmission: string; labels: string[] }[];
  usage_basis?: string;
};
export type GlanceResult = { text: string; receipt: GlanceReceipt | null; error: string | null; screen?: ScreenReceipt | null };

/** Human labels for data classes shown before a provider request. */
export const classNames: Record<string, string> = visualClassNames;

const encoder = new TextEncoder();

export function inputProblem(text: string, recipe: Recipe, language: string, question: string): string | null {
  if (text.includes("\0") || encoder.encode(text).length > 8192) return "Use source text of up to 8,192 UTF-8 bytes without null characters.";
  if (recipe === "translate" && (encoder.encode(language.trim()).length > 100 || /[\u0000-\u001f\u007f-\u009f]/u.test(language.trim()))) {
    return "Use a language name of up to 100 UTF-8 bytes without control characters.";
  }
  if (recipe === "custom" && (question.includes("\0") || encoder.encode(question.trim()).length > 2048)) {
    return "Use a question of up to 2,048 UTF-8 bytes without null characters.";
  }
  return null;
}

export function prepareGlance(
  state: GlanceState, provider: GlanceProvider | undefined, recipe: Recipe, text: string,
  language: string, question: string, memory: boolean, documents: number[], crop?: string,
): GlanceInput | null {
  // An image goes only to a provider that accepts images; text is then optional.
  const image = !!crop && !!provider?.imageInput;
  if (crop && !image) return null;
  if ((!text.trim() && !image) || (!provider && !state.localAvailable) || inputProblem(text, recipe, language, question)) return null;
  if ((recipe === "translate" && !language.trim()) || (recipe === "custom" && !question.trim())) return null;
  return {
    text, recipe, share: false,
    ...(recipe === "translate" && { language: language.trim() }),
    ...(recipe === "custom" && { question: question.trim() }),
    ...(provider && { provider: provider.id, revision: provider.revision }),
    ...(image && { image: true, crop }),
    memory: memory && state.memoryEnabled && (!provider || provider.sharing.memories),
    documents: provider && !provider.sharing.documents ? [] : state.documents.filter(d => documents.includes(d.id)).map(d => d.id),
  };
}
