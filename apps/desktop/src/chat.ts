import type { GenerationStats, ModelConfig, Status } from "./inference";

export type Appearance = "system" | "light" | "dark";

export type ModelSettings = ModelConfig & {
  temperature: number;
  maxTokens: number;
  topP: number;
  topK: number;
  enableThinking: boolean;
};

export type Preferences = {
  appearance: Appearance;
  defaults: ModelSettings;
  enterToSend: boolean;
};

export type ModelReference = {
  id: number;
  fileName: string;
  settings: ModelSettings;
};

export type Bootstrap = {
  preferences: Preferences;
  databasePath: string;
  models: ModelReference[];
  activeModelId: number | null;
  inference: Status;
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
};

export type Conversation = ConversationSummary & {
  systemPrompt: string;
  createdAt: number;
  messages: Message[];
};

export type ChatEvent =
  | { kind: "started"; user: Message | null; assistant: Message }
  | { kind: "delta"; id: number; text: string };

function upsert(messages: Message[], message: Message): Message[] {
  const next = messages.filter((current) => current.id !== message.id);
  next.push(message);
  return next.sort((a, b) => a.position - b.position || a.id - b.id);
}

export function applyChatEvent(messages: Message[], event: ChatEvent): Message[] {
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
