/** Only the explicit selections for this run are passed to the native Glance session. */
export const recipes = {
  explain: "Explain", summarize: "Summarize", rewrite: "Rewrite", improve: "Improve writing",
  grammar: "Fix grammar", translate: "Translate", key_points: "Extract key points", custom: "Ask a question",
} as const;

export type Recipe = keyof typeof recipes;

export type GlanceProvider = {
  id: number; name: string; model: string; locality: string; retention: string; revision: string;
  sharing: { documents: boolean; memories: boolean };
};

export type GlanceState = {
  localAvailable: boolean; memoryEnabled: boolean; providers: GlanceProvider[];
  documents: { id: number; name: string }[];
};

export type GlanceInput = {
  text: string; recipe: Recipe; language?: string; question?: string;
  provider?: number; revision?: string; share: boolean; memory: boolean; documents: number[];
};

export type GlanceReceipt = {
  state: string; capability: string; application_retention: string;
  provider: { id: string; locality: string; retention: string } | null;
  used: { kind: string; record?: number | null }[]; withheld: { kind: string; record?: number | null }[];
  external: { origin: string; transmission: string; labels: string[] }[];
  usage_basis?: string;
};
export type GlanceResult = { text: string; receipt: GlanceReceipt | null; error: string | null };

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
  language: string, question: string, memory: boolean, documents: number[],
): GlanceInput | null {
  if (!text.trim() || (!provider && !state.localAvailable) || inputProblem(text, recipe, language, question)) return null;
  if ((recipe === "translate" && !language.trim()) || (recipe === "custom" && !question.trim())) return null;
  return {
    text, recipe, share: false,
    ...(recipe === "translate" && { language: language.trim() }),
    ...(recipe === "custom" && { question: question.trim() }),
    ...(provider && { provider: provider.id, revision: provider.revision }),
    memory: memory && state.memoryEnabled && (!provider || provider.sharing.memories),
    documents: provider && !provider.sharing.documents ? [] : state.documents.filter(d => documents.includes(d.id)).map(d => d.id),
  };
}
