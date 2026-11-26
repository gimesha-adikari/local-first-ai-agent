import type { ModelInfo } from "./inference";

/** What the app knows about a model before it is loaded, matched by file name. */
export type ModelProfile = {
  name: string;
  role: string;
  /** Shipped with the app as a verified resource. */
  builtIn: boolean;
  /** Expected capabilities. The loaded model's flags always take precedence. */
  thinking: boolean;
  agent: boolean;
};

const profiles: { pattern: RegExp; profile: ModelProfile }[] = [
  {
    pattern: /^lfm2\.5[-_.]?1\.2b[-_.]instruct/i,
    profile: { name: "LFM2.5 1.2B", role: "Built-in lightweight assistant", builtIn: true, thinking: false, agent: false },
  },
  {
    pattern: /^qwen3\.5[-_.]?4b/i,
    profile: { name: "Qwen3.5 4B", role: "Advanced: Agent and Thinking", builtIn: false, thinking: true, agent: true },
  },
];

export const builtInModelName = "LFM2.5 1.2B";
export const advancedModelName = "Qwen3.5-4B";

export function modelProfile(fileName: string): ModelProfile | null {
  return profiles.find(({ pattern }) => pattern.test(fileName))?.profile ?? null;
}

/**
 * Profile of a listed model. Only the row registered from the verified app
 * resource is built in; an imported copy of the same file is labeled imported.
 */
export function referenceProfile(reference: { fileName: string; bundled: boolean }): ModelProfile | null {
  const profile = modelProfile(reference.fileName);
  if (!profile || profile.builtIn === reference.bundled) return profile;
  return profile.builtIn
    ? { ...profile, builtIn: false, role: "Imported lightweight assistant" }
    : { ...profile, builtIn: true };
}

/**
 * Friendly display name. GGUF `general.name` can be an internal identifier
 * (the bundled LFM2.5 file stores a commit hash), so the file name is used.
 */
export function modelLabel(fileName: string): string {
  return modelProfile(fileName)?.name ?? fileName.replace(/\.gguf$/i, "");
}

export type Capability = { key: "chat" | "thinking" | "agent"; label: string; available: boolean };

/**
 * Capabilities for display. Loaded models report `supportsThinking` and
 * `supportsAgent`; unloaded models use their known profile, or null when
 * nothing is known until the model loads.
 */
export function modelCapabilities(fileName: string, info: ModelInfo | null): { capabilities: Capability[]; checked: boolean } | null {
  const profile = modelProfile(fileName);
  if (!info && !profile) return null;
  const thinking = info ? info.supportsThinking : profile!.thinking;
  const agent = info ? info.supportsAgent : profile!.agent;
  return {
    checked: !!info,
    capabilities: [
      { key: "chat", label: "Chat", available: true },
      { key: "thinking", label: "Thinking", available: thinking },
      { key: "agent", label: "Agent", available: agent },
    ],
  };
}

/** Guidance when the loaded model lacks Agent or Thinking support, or null. */
export function capabilityGuidance(info: Pick<ModelInfo, "fileName" | "supportsAgent" | "supportsThinking"> | null): string | null {
  if (!info || (info.supportsAgent && info.supportsThinking)) return null;
  const missing = !info.supportsAgent && !info.supportsThinking ? "Agent tasks and Thinking"
    : !info.supportsAgent ? "Agent tasks" : "Thinking";
  const name = modelLabel(info.fileName);
  const reason = modelProfile(info.fileName)?.builtIn && !info.supportsAgent
    ? `${name} is a lightweight Chat model. Its reliability with multi-step tool use hasn't been established, so Agent is off.`
    : `${name} doesn't support ${missing}.`;
  return `${reason} For ${missing}, select or import ${advancedModelName}.`;
}

/** Composer text for an Agent mode that the loaded model can't run. */
export function agentUnavailableText(info: Pick<ModelInfo, "fileName"> | null): string {
  return info ? `${modelLabel(info.fileName)} supports Chat only. Agent needs ${advancedModelName}.` : "The loaded model can't run Agent tasks";
}
