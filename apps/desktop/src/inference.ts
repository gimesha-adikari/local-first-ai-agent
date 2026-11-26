export type ModelConfig = { contextLength: number; threads: number };

export type ModelInfo = {
  fileName: string;
  name: string;
  architecture: string;
  parameters: number;
  fileBytes: number;
  trainedContext: number;
  supportsThinking: boolean;
  /** Validated for the Agent decision protocol; independent of Thinking. */
  supportsAgent: boolean;
  config: ModelConfig;
  loadMs: number;
};

export type Phase = "empty" | "loading" | "ready" | "generating" | "unloading";
export type Status = { phase: Phase; model: ModelInfo | null };

export type GenerationStats = {
  reason: "end_of_generation" | "token_limit" | "cancelled";
  promptTokens: number;
  promptTokensPerSecond: number | null;
  generatedTokens: number;
  timeToFirstTokenMs: number | null;
  elapsedMs: number;
  tokensPerSecond: number;
  excludedTurns: number;
  /** Requested ceiling and effective output for the latest segment (Task 03-A). */
  outputBudget?: OutputBudget | null;
  /** Sources and memories that fit in the prompt of a contextual attempt (v0.2.0 Task 03). */
  contextUsage?: { sources: number; memories: number } | null;
};

export type OutputBudget = { requested: number; effective: number; contextLength: number };

export type TextDelta = { text: string };
export type Output = { id: number; text: string };

export function appendDelta(output: Output, id: number, delta: TextDelta): Output {
  if (output.id !== id || typeof delta.text !== "string" || !delta.text) {
    return output;
  }
  return { id, text: output.text + delta.text };
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
    return error.message;
  }
  return "The inference operation failed.";
}
