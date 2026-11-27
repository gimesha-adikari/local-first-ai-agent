import type { AttemptContext } from "./knowledge";
import type { GenerationStats, ModelConfig, Status } from "./inference";
import type { KnowledgeCoverage, RetrievalMode } from "./knowledge";

export type Appearance = "system" | "light" | "dark";

export type ModelSettings = ModelConfig & {
  temperature: number;
  maxTokens: number;
  topP: number;
  topK: number;
  enableThinking: boolean;
};

/** Read-only recommendation from `recommended_chat_settings` (Task 03-A). */
export type RecommendedChatSettings = { settings: ModelSettings; trainedContext: number | null };

export type SettingChange = { key: keyof ModelSettings; label: string; from: string; to: string; reload: boolean };

const settingLabels: { key: keyof ModelSettings; label: string; reload: boolean }[] = [
  { key: "contextLength", label: "Context", reload: true },
  { key: "maxTokens", label: "Output limit", reload: false },
  { key: "threads", label: "CPU threads", reload: true },
  { key: "enableThinking", label: "Think by default", reload: false },
  { key: "temperature", label: "Temperature", reload: false },
  { key: "topP", label: "Top-p", reload: false },
  { key: "topK", label: "Top-k", reload: false },
];

const formatSetting = (value: unknown) => (typeof value === "boolean" ? (value ? "On" : "Off")
  : typeof value === "number" ? value.toLocaleString("en-US") : String(value));

/** Differences between saved settings and a recommendation, in display order. */
export function settingChanges(current: ModelSettings, next: ModelSettings): SettingChange[] {
  return settingLabels.filter(({ key }) => current[key] !== next[key]).map(({ key, label, reload }) => ({
    key, label, reload, from: formatSetting(current[key]), to: formatSetting(next[key]),
  }));
}

export type Preferences = {
  appearance: Appearance;
  defaults: ModelSettings;
  enterToSend: boolean;
};

export type ModelReference = {
  id: number;
  fileName: string;
  settings: ModelSettings;
  /** Registered from the verified app resource rather than imported (Task 06-A). */
  bundled: boolean;
  /** Settings follow the recommendation on every load until custom settings are saved. */
  automaticSettings: boolean;
  /** Curated manifest entry whose size and SHA-256 the file matched; null for an unverified import. */
  artifact?: string | null;
  managed?: boolean;
  available?: boolean;
};

export type Bootstrap = {
  preferences: Preferences;
  databasePath: string;
  plaintextBackup: string | null;
  models: ModelReference[];
  activeModelId: number | null;
  inference: Status;
  /** Local-only mode and the provider Chat uses. */
  intelligence: { localOnly: boolean; activeProvider: number | null };
};

/** Result of verifying the model resource shipped with the app (Task 04-A). */
export type BundledModel = {
  state: "ready" | "missing" | "corrupted";
  model: ModelReference | null;
  /** No user-imported model exists, so the UI may load the bundled one. */
  autoLoad: boolean;
  /** The last Chat model loaded successfully; absent after an explicit unload (Task 06-C). */
  restore?: { model: ModelReference; available: boolean } | null;
};

export type ConversationSummary = {
  id: number;
  title: string;
  updatedAt: number;
};

export type Message = {
  id: number;
  position: number;
  role: "user" | "assistant" | "system";
  state: "streaming" | "complete" | "cancelled" | "interrupted" | "failed" | "truncated";
  content: string;
  createdAt: number;
  stats: GenerationStats | null;
  context?: AttemptContext | null;
  /** Routing coverage of an ordinary attempt with selected documents (Task 06-C). */
  routeCoverage?: import("./knowledge").KnowledgeCoverage | null;
};

export type Conversation = ConversationSummary & {
  systemPrompt: string;
  createdAt: number;
  messages: Message[];
};

export type ChatEvent =
  | { kind: "started"; user: Message | null; assistant: Message }
  | { kind: "knowledge"; coverage: KnowledgeCoverage }
  | { kind: "delta"; id: number; text: string }
  | { kind: "research"; id: number; stage: import("./research").ResearchStage };

function upsert(messages: Message[], message: Message): Message[] {
  const next = messages.filter((current) => current.id !== message.id);
  next.push(message);
  return next.sort((a, b) => a.position - b.position || a.id - b.id);
}

export function applyChatEvent(messages: Message[], event: ChatEvent): Message[] {
  if (event.kind === "knowledge" || event.kind === "research") return messages;
  if (event.kind === "started") {
    const withUser = event.user ? upsert(messages, event.user) : messages;
    return upsert(withUser, event.assistant);
  }

  const index = messages.findIndex((message) =>
    message.id === event.id && message.role === "assistant" && message.state === "streaming",
  );
  if (index === -1 || typeof event.text !== "string" || !event.text) return messages;

  const next = [...messages];
  const current = next[index];
  next[index] = { ...current, content: current.content + event.text };
  return next;
}

export function earlierReplies(messages: Message[]): Set<number> {
  const earlier = new Set<number>();
  let seenAssistant = false;
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (message.role === "user") seenAssistant = false;
    if (message.role !== "assistant") continue;
    if (seenAssistant) earlier.add(message.id);
    seenAssistant = true;
  }
  return earlier;
}

const thinkingStart = "<think>";
const thinkingEnd = "</think>";

export type Reasoning = {
  answer: string;
  reasoning: string | null;
  reasoningComplete: boolean;
};

export function splitReasoning(content: string, streaming: boolean): Reasoning {
  const trimmed = content.trimStart();
  if (!trimmed.startsWith(thinkingStart)) {
    if (streaming && trimmed && thinkingStart.startsWith(trimmed)) {
      return { answer: "", reasoning: "", reasoningComplete: false };
    }
    return { answer: content, reasoning: null, reasoningComplete: false };
  }

  const body = trimmed.slice(thinkingStart.length);
  const marker = body.indexOf(thinkingEnd);
  if (marker !== -1) {
    return {
      answer: body.slice(marker + thinkingEnd.length).replace(/^(?:[ \t]*\r?\n)+/, ""),
      reasoning: body.slice(0, marker).trim(),
      reasoningComplete: true,
    };
  }

  // A checkpoint or streaming chunk may end inside the closing delimiter.
  // Keep that prefix out of the reasoning text until its meaning is known.
  let end = body.length;
  for (let length = thinkingEnd.length - 1; length > 0; length--) {
    if (body.endsWith(thinkingEnd.slice(0, length))) {
      end -= length;
      break;
    }
  }
  return { answer: "", reasoning: body.slice(0, end).trimStart(), reasoningComplete: false };
}

export function validModelSettings(settings: ModelSettings): boolean {
  const { contextLength, threads, maxTokens, temperature, topP, topK } = settings;
  return Number.isInteger(contextLength) && contextLength >= 128 && contextLength <= 32768
    && Number.isInteger(threads) && threads >= 1 && threads <= 32
    && Number.isInteger(maxTokens) && maxTokens >= 1 && maxTokens <= 4096 && maxTokens < contextLength
    && temperature >= 0 && temperature <= 2
    && topP > 0 && topP <= 1
    && Number.isInteger(topK) && topK >= 1 && topK <= 200;
}

/** Per-request chat options accepted by `send_message` and `regenerate`. */
export type ChatOptions = {
  enableThinking?: boolean;
  maxTokens?: number;
  /** Omitted or null for ordinary Chat. */
  knowledge?: { mode: RetrievalMode; documentIds: number[]; grounding?: "auto" | "documents" } | null;
  /** One explicitly authorized web research request (Stage 04-C). */
  research?: ReturnType<typeof import("./research").researchOptions>;
};

/** Sampling defaults of the backend (agent-core GenerationRequest). */
export const samplingDefaults = { temperature: 0.7, topP: 0.8, topK: 20 } as const;

/** Largest context the app recommends, and the backend's thread default. */
const recommendedContextCeiling = 8192;
const recommendedThreads = 4;

/**
 * The one recommendation the app uses. Mirrors the backend's
 * `recommended_chat_settings`: context is the model's trained context capped at
 * 8,192 (8,192 when unknown), output is half of it up to 4,096, Thinking is
 * off and sampling uses the backend defaults. The Models dialog uses the
 * backend's own result for the loaded model; this covers the defaults for
 * newly imported models, where no model is loaded.
 */
export function recommendedChatSettings(trainedContext: number | null = null): RecommendedChatSettings {
  const contextLength = trainedContext === null ? recommendedContextCeiling : Math.min(trainedContext, recommendedContextCeiling);
  return {
    trainedContext,
    settings: {
      contextLength,
      threads: recommendedThreads,
      maxTokens: Math.min(Math.floor(contextLength / 2), 4096),
      ...samplingDefaults,
      enableThinking: false,
    },
  };
}

export function matchesRecommended(settings: ModelSettings, recommended: ModelSettings): boolean {
  return settingChanges(settings, recommended).length === 0;
}

/**
 * Saved output limits below this often cut replies short. It is a warning
 * threshold, not a recommendation; the recommendation is model-aware.
 */
export const shortOutputLimit = 768;

export function lowOutputLimit(settings: Pick<ModelSettings, "maxTokens">): boolean {
  return settings.maxTokens < shortOutputLimit;
}

/**
 * Build explicit per-turn options. Thinking is sent only when the person chose
 * it for this turn and the loaded model supports it; otherwise the turn is direct.
 */
export function chatRequestOptions({ thinking, supportsThinking, maxTokens, knowledge = null }: {
  thinking: boolean;
  supportsThinking: boolean;
  maxTokens?: number | null;
  knowledge?: ChatOptions["knowledge"];
}): ChatOptions {
  const options: ChatOptions = { enableThinking: thinking && supportsThinking };
  if (typeof maxTokens === "number" && Number.isInteger(maxTokens) && maxTokens >= 1 && maxTokens <= 4096) {
    options.maxTokens = maxTokens;
  }
  if (knowledge) {
    options.knowledge = { mode: knowledge.mode, documentIds: [...knowledge.documentIds] };
    if (knowledge.grounding === "documents") options.knowledge.grounding = "documents";
  }
  return options;
}

/** A one-turn override that is valid for the loaded context, or null when none helps. */
export function suggestedOutputLimit(current: number, contextLength: number): number | null {
  const ceiling = Math.min(4096, contextLength - 512);
  const next = Math.min(ceiling, Math.max(shortOutputLimit, current * 2));
  return Number.isInteger(next) && next > current ? next : null;
}

export type ResponseNotice = {
  tone: "warning" | "error" | "neutral";
  title: string;
  detail: string;
  suggestLimit: boolean;
};

/** Typed IPC error shape returned by Tauri commands. */
export type CommandError = { code: string; message: string };

export function commandError(failure: unknown): CommandError {
  if (failure && typeof failure === "object") {
    const { code, message } = failure as { code?: unknown; message?: unknown };
    return { code: typeof code === "string" ? code : "unknown", message: typeof message === "string" ? message : "" };
  }
  return { code: "unknown", message: typeof failure === "string" ? failure : "" };
}

/**
 * A Chat reply the backend may be able to continue: cut off by the token limit.
 * The backend still decides (replay seed, model, request caps) through
 * `chat_continuation_remaining`.
 */
export function continuationCandidate(message: Message | null | undefined): boolean {
  return !!message && message.role === "assistant" && message.state === "truncated"
    && message.stats?.reason === "token_limit";
}

/** The latest assistant attempt of a conversation whose newest turn is Chat. */
export function latestChatAttempt(turns: readonly { kind: string; assistants: readonly { message: Message }[] }[]): Message | null {
  const last = turns.at(-1);
  if (!last || last.kind !== "chat") return null;
  return last.assistants.at(-1)?.message ?? null;
}

export type ContinuationStatus = {
  messageId: number;
  /** Remaining continuation requests (1–3) once checked; null while checking or unavailable. */
  remaining: number | null;
  checking: boolean;
  /** Why the reply can't be continued now, from the eligibility check or the last attempt. */
  blocked: { title: string; detail: string } | null;
};

/** Specific copy for continuation errors, keyed by typed IPC code. */
export function continuationErrorCopy(error: CommandError): { title: string; detail: string } {
  const message = error.message.trim();
  switch (error.code) {
    case "context_limit":
      return {
        title: "Not enough context left to continue",
        detail: "The original prompt and the reply so far fill the loaded context window. Regenerate with a shorter request, "
          + "or reload the model with a larger context length.",
      };
    case "no_model":
      return {
        title: "Load the original model to continue",
        detail: message || "Continuation needs the same model that wrote this reply.",
      };
    case "invalid_config":
      return {
        title: "This reply can't be continued",
        detail: `${message ? `${message.replace(/\.?$/, ".")} ` : ""}Replies that used documents or memory, replies saved before `
          + "continuation support, replies written by a different model, and replies that used every continuation can be regenerated instead.",
      };
    case "contextual":
      return {
        title: "Answers that used documents or memory can't be continued",
        detail: "Regenerate searches again with your current knowledge settings and keeps this answer as an earlier attempt.",
      };
    case "busy":
      return { title: "Another operation is running", detail: message || "Wait for it to finish, then try again." };
    case "unsupported_template":
    case "invalid_model":
      return { title: "The loaded model can't continue this reply", detail: message || "Load the model that wrote it." };
    default:
      return { title: "Couldn't continue the response", detail: message || "Try again, or regenerate the response." };
  }
}

/**
 * Explain an effective output budget below the requested ceiling. Thinking and
 * the answer share the effective budget.
 */
export function outputBudgetNote(stats: GenerationStats | null | undefined): string | null {
  const budget = stats?.outputBudget;
  if (!budget || !(budget.effective > 0) || budget.effective >= budget.requested) return null;
  return `${budget.effective.toLocaleString("en-US")} of ${budget.requested.toLocaleString("en-US")} output tokens fit `
    + `after the prompt and history in the ${budget.contextLength.toLocaleString("en-US")}-token context`;
}

/** Explain a non-complete assistant turn and what the person can do next. */
export function responseNotice(message: Message, limit?: number, continuable = false): ResponseNotice | null {
  if (message.role !== "assistant") return null;
  const { answer, reasoning } = splitReasoning(message.content, message.state === "streaming");
  const hasAnswer = !!answer.trim();
  const budget = limit ? `${limit}-token ` : "";
  switch (message.state) {
    case "truncated":
      return hasAnswer
        ? {
          tone: "warning", title: "Response cut off at the output limit",
          detail: continuable
            ? `The reply reached its ${budget}output limit before finishing. Continue generating to append the rest to this reply, or regenerate it with a larger limit.`
            : `The reply reached its ${budget}output limit before finishing. Regenerate with a larger limit to get the full answer.`,
          suggestLimit: true,
        }
        : {
          tone: "warning", title: "No answer before the output limit",
          detail: reasoning !== null
            ? `Thinking used the whole ${budget}output budget before an answer was written. ${continuable ? "Continue generating to let it finish, regenerate" : "Regenerate"} with a larger limit, or turn thinking off.`
            : `The reply reached its ${budget}output limit before an answer was written. ${continuable ? "Continue generating, or regenerate" : "Regenerate"} with a larger limit.`,
          suggestLimit: true,
        };
    case "failed":
      if (message.stats?.reason === "end_of_generation" && !hasAnswer) {
        return {
          tone: "warning", title: "The model finished without an answer",
          detail: reasoning !== null
            ? "It stopped after thinking without writing a final answer. Regenerate, or try again with thinking off."
            : "It stopped without writing a final answer. Regenerate to try again.",
          suggestLimit: false,
        };
      }
      return {
        tone: "error", title: "Generation failed",
        detail: "This response didn't complete. Regenerate to try again.",
        suggestLimit: false,
      };
    case "cancelled":
      return {
        tone: "neutral", title: "Stopped",
        detail: hasAnswer ? "You stopped this response before it finished." : "You stopped this response before an answer was written.",
        suggestLimit: false,
      };
    case "interrupted":
      return {
        tone: "neutral", title: "Interrupted",
        detail: "The app closed or the model stopped before this response finished.",
        suggestLimit: false,
      };
    default:
      return null;
  }
}
