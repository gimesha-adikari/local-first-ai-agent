// DEVELOPMENT ONLY — SYNTHETIC DATA. Loaded by main.tsx when Vite serves the UI
// in an ordinary browser (no Tauri host). It imitates the Task 02-A unified IPC
// with deterministic, keyword-driven behavior so difficult states can be
// inspected without a model. Production builds never include this file.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";

type Json = Record<string, unknown>;
type Message = { id: number; position: number; role: string; state: string; content: string; createdAt: number; stats: Json | null; context?: Json | null };
type NativeResult = { record: Json; observation: Json };
type Turn = {
  id: string; kind: "chat" | "agent"; user: Message; assistants: { message: Message; modelName: string | null }[];
  taskId: string | null; taskTimeoutMs: number | null; cache: NativeResult[];
};
type Conversation = { id: number; title: string; createdAt: number; updatedAt: number; systemPrompt: string; turns: Turn[] };

const settings = (maxTokens: number, contextLength = 2048) => ({
  contextLength, threads: 4, temperature: 0.7, maxTokens, topP: 0.8, topK: 20, enableThinking: false,
});
const models = [
  { id: 2, fileName: "Qwen3.5-4B-Q4_K_M.gguf", settings: settings(768, 4096) },
  { id: 1, fileName: "Qwen3-0.6B-Q4_K_M.gguf", settings: settings(256) },
];
const qwen35 = {
  fileName: "Qwen3.5-4B-Q4_K_M.gguf", name: "Qwen3.5 4B", architecture: "qwen35",
  parameters: 4_210_000_000, fileBytes: 2_740_000_000, trainedContext: 262144, supportsThinking: true, supportsAgent: true,
  config: { contextLength: 4096, threads: 4 }, loadMs: 3120,
};
const qwen3 = {
  fileName: "Qwen3-0.6B-Q4_K_M.gguf", name: "Qwen3 0.6B", architecture: "qwen3",
  parameters: 596_000_000, fileBytes: 484_000_000, trainedContext: 40960, supportsThinking: true, supportsAgent: true,
  config: { contextLength: 2048, threads: 4 }, loadMs: 820,
};

const lfm = { id: 3, fileName: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf", settings: settings(768, 4096) };
const lfmInfo = {
  fileName: lfm.fileName, name: "8ed288026e23958ad9dfa92d53ed773a8eee7125", architecture: "lfm2",
  parameters: 1_170_000_000, fileBytes: 730_895_168, trainedContext: 128000, supportsThinking: false, supportsAgent: false,
  config: { contextLength: 4096, threads: 4 }, loadMs: 640,
};
// Bundled-model scenarios: "firstlaunch" (only the verified bundled model),
// "corrupted" (failed integrity check, no models) and "nobundle" (missing resource).
const bundledScenarios = new Set(["firstlaunch", "corrupted", "nobundle"]);

const MODEL = "Qwen3.5-4B-Q4_K_M.gguf";
const now = Date.now();
const minute = 60_000;
let nextMessage = 200;
let nextTask = 60;
let scenario = "default";
// Provider scenarios: "providers" (a ready cloud provider used for Chat and a
// self-hosted server) and "onboarding" (no models and no providers).
let providers: { id: number; settings: Json; credential: string }[] = [];
let nextProvider = 3;
let intelligence = { localOnly: true, activeProvider: null as number | null };
const providerViews = () => providers.map((item) => {
  const settings = item.settings as Json & { locality: string; endpoint: string; requiresKey: boolean; retention: string; imageInput: boolean };
  const local = /\/\/(localhost|127\.)/.test(settings.endpoint);
  const credential = settings.requiresKey ? item.credential : null;
  return {
    id: item.id, settings, origin: settings.endpoint, network: settings.locality === "cloud" ? "public" : local ? "loopback" : "local_network",
    encrypted: settings.endpoint.startsWith("https://"), credential, period: "2026-11",
    usage: item.id === 1
      ? { requests: 14, reportedTokens: 48_210, estimatedTokens: 1_536, reservedTokens: 0, costMicros: 182_400, unpricedRequests: 0 }
      : { requests: 0, reportedTokens: 0, estimatedTokens: 0, reservedTokens: 0, costMicros: 0, unpricedRequests: 0 },
    modalities: settings.imageInput ? ["text", "image"] : ["text"],
    readiness: intelligence.localOnly ? "local_only" : settings.retention === "unknown" ? "needs_retention"
      : credential === "missing" ? "needs_credential" : "ready",
    active: !intelligence.localOnly && intelligence.activeProvider === item.id,
  };
});
let inference: Json = { phase: "ready", model: qwen35 };
let activeModelId: number | null = 2;
let active: { conversationId: number; turnId: string; taskId: string; approval: Json | null;
  tool: string | null; startedAt: number; taskTimeoutMs: number } | null = null;
let pendingApproval: ((approve: boolean) => void) | null = null;
let cancelTask: (() => void) | null = null;

const msg = (role: string, content: string, state = "complete", at = now): Message =>
  ({ id: nextMessage++, position: 0, role, state, content, createdAt: at, stats: null });
const record = (operationId: string, tool: string, status = "succeeded") => ({ operationId, tool, status, error: null });
const result = (operationId: string, tool: string, value: Json): NativeResult =>
  ({ record: record(operationId, tool), observation: { tool, status: "success", result: value } });

const searchResult = (op: string, truncated = false) => result(op, "files.search",
  { paths: ["alpha_ledger.txt", "budget_ledger.txt", "archive/ledger 2025.txt"], truncated });
const readResult = (op: string) => result(op, "files.read",
  { content: "Juniper Orchard ledger\n\nRainwater from the north barn is collected in two tanks and used to irrigate the young apple trees during dry weeks.", file_bytes: 132, truncated: false });

const tasks: Json[] = [];
const details: Record<string, Json> = {};

function journal(taskId: string, state: string, at: number, executions: Json[], error: string | null, usage: Json, metadata: Json = {}) {
  const index = tasks.findIndex((item) => item.id === taskId);
  const task = { conversationId: null, turnId: null, excludedTurns: null, errorMessage: null,
    ...tasks[index], id: taskId, state, createdAt: at, updatedAt: at + 40_000, finalized: true, usage, error, ...metadata };
  if (index === -1) tasks.unshift(task); else tasks[index] = task;
  details[taskId] = {
    task, events: [{ sequence: 1, createdAt: at, event: { kind: "transition", transition: { from: "created", to: "planning" } } }],
    executions: executions.map((item) => ({ ...item, createdAt: at, updatedAt: at + 20_000 })),
    audit: executions.length ? [{ namespace: "fixture", sequence: 1, createdAt: at, record: { action: "authorized", tool: executions[0].tool } }] : [],
    nativeResults: [],
  };
}

function agentTurn(conversation: Conversation, prompt: string, answer: string, results: NativeResult[], at: number, state = "complete"): Turn {
  const taskId = String(nextTask++);
  const user = msg("user", prompt, "complete", at);
  const assistant = msg("assistant", answer, state, at + 30_000);
  const turn: Turn = { id: String(user.id), kind: "agent", user, assistants: [{ message: assistant, modelName: MODEL }], taskId, taskTimeoutMs: 300_000, cache: results };
  journal(taskId, state === "interrupted" ? "interrupted" : "completed", at, results.map((item) => item.record), null,
    { toolCalls: results.length, modelCalls: results.length ? 2 : 1, retries: 0, generatedTokens: 61 },
    { conversationId: conversation.id, turnId: turn.id, excludedTurns: 0 });
  conversation.turns.push(turn);
  conversation.updatedAt = at + 30_000;
  return turn;
}

function chatTurn(conversation: Conversation, prompt: string, answer: string, at: number) {
  const user = msg("user", prompt, "complete", at);
  const assistant = { ...msg("assistant", answer, "complete", at + 5_000), stats: stats(48) };
  conversation.turns.push({ id: String(user.id), kind: "chat", user, assistants: [{ message: assistant, modelName: MODEL }], taskId: null, taskTimeoutMs: null, cache: [] });
  conversation.updatedAt = at + 5_000;
}

const stats = (generatedTokens: number, reason = "end_of_generation") => ({
  reason, promptTokens: 38, promptTokensPerSecond: 182.4, generatedTokens, timeToFirstTokenMs: 410,
  elapsedMs: 9_800, tokensPerSecond: 21.6, excludedTurns: 0,
});

const conversations: Conversation[] = [];

function seed() {
  const ledger: Conversation = { id: 5, title: "Find ledger files in the selected folder", createdAt: now - 30 * minute, updatedAt: now, systemPrompt: "", turns: [] };
  agentTurn(ledger, "Find ledger files in the selected folder.", "I found three ledger files: `alpha_ledger.txt`, `budget_ledger.txt` and `archive/ledger 2025.txt`.", [searchResult("op_a1")], now - 30 * minute);
  agentTurn(ledger, "Read the first matching file.", "`alpha_ledger.txt` describes the Juniper Orchard ledger. Rainwater from the north barn is collected in two tanks and used to irrigate young apple trees in dry weeks.", [readResult("op_a2")], now - 25 * minute);
  agentTurn(ledger, "Summarize its main point in one sentence.", "Juniper Orchard irrigates its young apple trees with rainwater collected from the north barn.", [], now - 20 * minute);
  const mixed: Conversation = { id: 6, title: "Calculator and follow-ups", createdAt: now - 90 * minute, updatedAt: now - 60 * minute, systemPrompt: "", turns: [] };
  chatTurn(mixed, "I need to check an invoice total. Can you help?", "Of course. Tell me the quantities and prices, and switch to Agent if you'd like the local calculator to verify the arithmetic.", now - 90 * minute);
  agentTurn(mixed, "Calculate 137 times 89 with the calculator.", "137 × 89 = **12,193**.", [result("op_m1", "math.calculate", { value: 12193 })], now - 88 * minute);
  agentTurn(mixed, "What number did the calculator just return?", "The calculator returned **12,193**.", [], now - 86 * minute);
  chatTurn(mixed, "Thanks. Is that above 12,000?", "Yes. 12,193 is 193 more than 12,000.", now - 60 * minute);
  const poem: Conversation = { id: 3, title: "Write a poem", createdAt: now - 26 * 60 * minute, updatedAt: now - 26 * 60 * minute, systemPrompt: "", turns: [] };
  chatTurn(poem, "Write a four-line poem about rain.", "Soft rain on the window,\nquiet streets below,\nthe garden drinks slowly,\nand the rivers grow.", now - 26 * 60 * minute);
  conversations.push(ledger, mixed, poem);
  // A standalone journal record from before conversations held Agent turns.
  journal("12", "completed", now - 3 * 24 * 60 * minute, [record("op_old", "device.info")], null, { toolCalls: 1, modelCalls: 2, retries: 0, generatedTokens: 40 });
  if (scenario === "restart") {
    for (const conversation of conversations) for (const turn of conversation.turns) turn.cache = [];
    const last = ledger.turns.at(-1)!;
    last.assistants[0].message = { ...last.assistants[0].message, state: "interrupted", content: "" };
    journal(last.taskId!, "interrupted", now - 20 * minute, [], null, { toolCalls: 0, modelCalls: 1, retries: 0, generatedTokens: 0 });
  }
  enforceCache();
}

/** The backend keeps native observations for at most eight result-bearing turns. */
function enforceCache() {
  const bearing = conversations.flatMap((conversation) => conversation.turns).filter((turn) => turn.cache.length)
    .sort((a, b) => b.user.createdAt - a.user.createdAt);
  for (const turn of bearing.slice(8)) turn.cache = [];
}

const scopes: Json[] = [];

function unified(conversation: Conversation) {
  return {
    id: conversation.id, title: conversation.title, systemPrompt: conversation.systemPrompt,
    createdAt: conversation.createdAt, updatedAt: conversation.updatedAt,
    turns: conversation.turns.map((turn) => {
      const task = tasks.find(({ id }) => id === turn.taskId);
      return {
        id: turn.id, kind: turn.kind, user: turn.user, assistants: turn.assistants, taskId: turn.taskId,
        taskTimeoutMs: turn.taskTimeoutMs, taskState: task?.state ?? null, taskError: task?.error ?? null,
        taskErrorMessage: task?.errorMessage ?? null, excludedTurns: task?.excludedTurns ?? null,
        nativeResultsAvailable: turn.cache.length > 0,
      };
    }),
  };
}

function deliver(channel: unknown, index: number, message: unknown) {
  const id = (channel as { id: number }).id;
  (window as unknown as { __TAURI_INTERNALS__: { runCallback: (id: number, data: unknown) => void } })
    .__TAURI_INTERNALS__.runCallback(id, { index, message });
}

const wait = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms));
const find = (id: unknown) => {
  const conversation = conversations.find((item) => item.id === Number(id));
  if (!conversation) throw { code: "not_found", message: "Conversation not found." };
  return conversation;
};

const chatAnswer = `Ownership is how Rust manages memory without a garbage collector. Every value has a single **owner**, and the value is dropped when its owner goes out of scope.

\`\`\`rust
fn main() {
    let greeting = String::from("hello");
    let moved = greeting; // ownership moves here
    println!("{moved}");
}
\`\`\``;

// Synthetic web research: settings, transient source views and progress.
let webSettings = {
  provider: "brave" as "brave" | "searxng" | "purili" | null, braveSaveResults: false, searxngEndpoint: "",
  searxngSaveResults: false, puriliPersonalUse: false,
};
let braveKey: "stored" | "missing" = "stored";
const researchViews = new Map<number, unknown>();

function webView() {
  return {
    settings: structuredClone(webSettings), braveKey,
    searxngOrigin: webSettings.searxngEndpoint || null,
    searxngNetwork: webSettings.searxngEndpoint ? "loopback" : null,
    searxngError: null, localOnly: intelligence.localOnly,
  };
}

function researchFixture(research: Json, question: string) {
  const page = research.mode === "page";
  const synthesis = research.resultsOnly ? "none" : "local";
  const sources = page
    ? [{ citationId: "S1", evidence: "page", supplied: synthesis !== "none", cited: synthesis !== "none", title: "Rainwater harvesting basics", url: String(research.url), host: new URL(String(research.url)).hostname, published: "2026-10-02", excerpt: "Collected rainwater can irrigate gardens during dry weeks. Tanks should be covered and cleaned yearly.", saved: true, openable: true }]
    : [
      { citationId: "S1", evidence: "page", supplied: synthesis !== "none", cited: synthesis !== "none", title: "Fernhill bridge opens to traffic", url: "https://news.example.org/fernhill-bridge?ref=search", host: "news.example.org", published: "2026-03-02", excerpt: "The Fernhill bridge opened to traffic in March 2026 after four years of construction.", saved: false, openable: true },
      { citationId: "S2", evidence: "page", supplied: synthesis !== "none", cited: synthesis !== "none", title: "Engineering notes: Fernhill crossing", url: "https://blog.example.net/fernhill", host: "blog.example.net", published: null, excerpt: "The crossing spans 120 metres and carries two traffic lanes and a cycle path.", saved: false, openable: true },
      { citationId: "S3", evidence: "snippet", supplied: synthesis !== "none", cited: false, title: "Members only: bridge report", url: "https://paywall.example.com/story", host: "paywall.example.com", published: null, excerpt: "The bridge opened in March 2026, members report.", saved: false, openable: true },
    ];
  const record = {
    version: 1, mode: page ? "page" : "search", ...(page ? {} : { provider: webSettings.provider ?? "brave" }), synthesis,
    discovered: page ? 0 : 5, retrieved: page ? 1 : 2, unavailable: page ? 0 : 1,
    sources: sources.map((source) => ({ evidence: source.evidence, saved: source.saved })), transientResults: !page,
  };
  const view = {
    record, sources,
    discovered: page ? [] : [
      { title: "Fernhill bridge timeline", url: "https://city.example.gov/projects/fernhill", host: "city.example.gov", snippet: "Project milestones from 2022 to 2026.", published: null, attempted: false },
      { title: "Bridge photos", url: "https://photos.example.com/fernhill", host: "photos.example.com", snippet: "A gallery of the opening day.", published: null, attempted: false },
    ],
    limitations: page ? [] : ["paywall.example.com requires sign-in or blocks automated reading (access restricted)."],
    query: page ? null : String(research.query ?? question), transient: true,
  };
  const text = synthesis === "none"
    ? page ? "The webpage was read. You asked for results only. The sources are listed below."
      : `Brave Search found 5 results. You asked for results only. The sources are listed below.`
    : page
      ? "The page explains that collected rainwater can irrigate gardens during dry weeks, and that tanks should be covered and cleaned every year [S1]."
      : "The Fernhill bridge opened to traffic in **March 2026**, after four years of construction [S1]. It spans 120 metres with two lanes and a cycle path [S2].";
  return { record, view, text, supplied: synthesis === "none" ? 0 : sources.length };
}

async function streamChat(args: Json, regenerate: boolean): Promise<Message> {
  const conversation = find(args.id);
  const options = (args.options ?? {}) as Json;
  let index = 0;
  let turn: Turn;
  let user: Message | null = null;
  if (regenerate) {
    const last = conversation.turns.at(-1);
    if (!last || last.kind !== "chat") throw { code: "invalid_request", message: "Regenerate applies to the latest Chat turn." };
    turn = last;
  } else {
    user = msg("user", String(args.content));
    turn = { id: String(user.id), kind: "chat", user, assistants: [], taskId: null, taskTimeoutMs: null, cache: [] };
    conversation.turns.push(turn);
    if (conversation.title === "New conversation") conversation.title = String(args.content).slice(0, 48);
  }
  const assistant = msg("assistant", "", "streaming");
  turn.assistants.push({ message: assistant, modelName: activeModelId === 1 ? "Qwen3-0.6B-Q4_K_M.gguf" : MODEL });
  const research = options.research as Json | undefined;
  if (research) {
    if (intelligence.localOnly) throw { code: "policy_denied", message: "Local-only mode is on. Web research needs internet access, even with a local model." };
    const fixture = researchFixture(research, turn.user.content);
    assistant.context = { coverage: null, memoryEnabled: null, fitted: null, research: fixture.record };
    deliver(args.events, index++, { kind: "started", user, assistant: { ...assistant } });
    for (const stage of research.mode === "page" ? ["reading", "ranking"] : ["searching", "reading", "ranking"]) {
      deliver(args.events, index++, { kind: "research", id: assistant.id, stage });
      await wait(700);
    }
    if (fixture.record.synthesis !== "none") deliver(args.events, index++, { kind: "research", id: assistant.id, stage: "answering" });
    for (let offset = 0; offset < fixture.text.length; offset += 16) {
      const chunk = fixture.text.slice(offset, offset + 16);
      assistant.content += chunk;
      deliver(args.events, index++, { kind: "delta", id: assistant.id, text: chunk });
      await wait(16);
    }
    assistant.context = { ...(assistant.context as Json), fitted: { sources: fixture.supplied, memories: 0 } };
    researchViews.set(assistant.id, fixture.view);
    assistant.state = "complete";
    assistant.stats = fixture.record.synthesis === "none" ? null : stats(80);
    deliver(args.events, index++, { kind: "research", id: assistant.id, stage: "done" });
    conversation.updatedAt = Date.now();
    return assistant;
  }
  deliver(args.events, index++, { kind: "started", user, assistant: { ...assistant } });
  const prompt = turn.user.content.toLowerCase();
  const body = /12,?193|calculator|that number/.test(prompt)
    ? "Earlier in this conversation the calculator returned **12,193** for 137 × 89. That value came from a native tool in an Agent turn; I'm only repeating the saved answer."
    : chatAnswer;
  const truncate = prompt.includes("truncate");
  const full = (options.enableThinking ? "<think>\nUse the saved conversation context.\n</think>\n\n" : "") + body;
  const text = truncate ? full.slice(0, 120) : full;
  for (let offset = 0; offset < text.length; offset += 16) {
    const chunk = text.slice(offset, offset + 16);
    assistant.content += chunk;
    deliver(args.events, index++, { kind: "delta", id: assistant.id, text: chunk });
    await wait(16);
  }
  assistant.state = truncate ? "truncated" : "complete";
  assistant.stats = truncate
    ? { ...stats(48, "token_limit"), outputBudget: { requested: 4096, effective: 48, contextLength: 4096 } }
    : stats(140);
  if (truncate) continuations.set(assistant.id, 3);
  conversation.updatedAt = Date.now();
  return assistant;
}

// Synthetic continuation: same message ID, appended suffix, at most three requests.
const continuations = new Map<number, number>();

async function continueChat(args: Json): Promise<Message> {
  const conversation = find(args.id);
  const attempt = conversation.turns.flatMap((turn) => turn.assistants).find((item) => item.message.id === Number(args.messageId));
  const left = continuations.get(Number(args.messageId)) ?? 0;
  if (!attempt || attempt.message.state !== "truncated" || left <= 0) {
    throw { code: "invalid_config", message: "This response can't be continued" };
  }
  continuations.set(attempt.message.id, left - 1);
  const assistant = attempt.message;
  assistant.state = "streaming";
  deliver(args.events, 0, { kind: "started", user: null, assistant: { ...assistant } });
  const suffix = left > 1 ? " Continued with more synthetic text that still stops early" : " and it finished here.";
  for (let offset = 0, index = 1; offset < suffix.length; offset += 12, index++) {
    const chunk = suffix.slice(offset, offset + 12);
    assistant.content += chunk;
    deliver(args.events, index, { kind: "delta", id: assistant.id, text: chunk });
    await wait(40);
  }
  assistant.state = left > 1 ? "truncated" : "complete";
  assistant.stats = { ...stats(48 * (5 - left), left > 1 ? "token_limit" : "end_of_generation"),
    outputBudget: { requested: 4096, effective: 1536, contextLength: 4096 } };
  return assistant;
}

async function turnEvent(identity: { conversationId: number; turnId: string; taskId: string }, event: Json, excludedTurns = 0) {
  const payload = { taskId: identity.taskId, ...event };
  await emit("agent-turn-event", { conversationId: identity.conversationId, turnId: identity.turnId, excludedTurns, event: payload });
  // The host also emits unchanged compatibility events; the UI must ignore them.
  await emit("agent-event", payload);
}

async function runTurn(conversation: Conversation, turn: Turn) {
  const identity = { conversationId: conversation.id, turnId: turn.id, taskId: turn.taskId! };
  const excluded = scenario === "omission" ? 3 : 0;
  const assistant = turn.assistants[0].message;
  let cancelled = false;
  cancelTask = () => { cancelled = true; };
  const step = async (ms: number) => {
    for (let waited = 0; waited < ms; waited += 100) {
      if (cancelled) throw new Error("cancelled");
      await wait(Math.min(100, ms - waited));
    }
    if (cancelled) throw new Error("cancelled");
  };
  const state = (value: string) => {
    const task = tasks.find(({ id }) => id === identity.taskId);
    if (task) task.state = value;
    if (active?.taskId === identity.taskId && value === "planning") active.tool = null;
    return turnEvent(identity, { kind: "state_changed", state: value }, excluded);
  };
  const prompt = turn.user.content.toLowerCase();
  const results: NativeResult[] = [];
  const executions: Json[] = [];
  const priorRead = conversation.turns.some((item) => item.cache.some((cached) => cached.observation.tool === "files.read"));

  const finish = async (finalState: string, answer: string | null, error: Json | null) => {
    assistant.state = finalState === "completed" ? "complete" : finalState === "cancelled" ? "cancelled" : finalState === "interrupted" ? "interrupted" : "failed";
    assistant.content = answer ?? "";
    turn.cache = results;
    enforceCache();
    conversation.updatedAt = Date.now();
    const usage = { toolCalls: executions.length, modelCalls: executions.length ? 2 : 1, retries: 0, generatedTokens: 74 };
    journal(identity.taskId, finalState, turn.user.createdAt, executions, error ? String(error.code) : null, usage,
      { excludedTurns: excluded, errorMessage: error?.message ?? null });
    details[identity.taskId].nativeResults = results;
    active = null;
    cancelTask = null;
    await turnEvent(identity, {
      kind: "finished", state: finalState, usage, answer, error, executions, nativeResults: results, journalIncomplete: false,
    }, excluded);
  };
  const observe = async (native: NativeResult) => {
    executions.push(native.record);
    await turnEvent(identity, { kind: "execution", record: native.record }, excluded);
    results.push(native);
    await turnEvent(identity, { kind: "observation", ...native }, excluded);
  };
  const dispatch = async (operationId: string, tool: string, native: NativeResult, ms = 800) => {
    if (active?.taskId === identity.taskId) active.tool = tool;
    await turnEvent(identity, { kind: "tool_proposed", tool }, excluded);
    await state("validating");
    await state("awaiting_authorization");
    await state("authorized");
    await state("executing");
    await turnEvent(identity, { kind: "dispatch_started", operationId, tool }, excluded);
    await step(ms);
    await observe(native);
    await state("observing");
  };

  try {
    await state("planning");
    await step(scenario === "slow" ? 2500 : 1000);
    if (/open|launch/.test(prompt)) {
      await dispatch("op_l1", "apps.list", result("op_l1", "apps.list", { applications: [
        { app_id: "app_1", display_name: "KCalc" }, { app_id: "app_2", display_name: "Text Editor" }, { app_id: "app_3", display_name: "Files" },
      ], truncated: false }), 600);
      await state("planning");
      await step(700);
      await turnEvent(identity, { kind: "tool_proposed", tool: "apps.launch" }, excluded);
      await state("awaiting_authorization");
      const target = { displayName: "KCalc", desktopFile: "/usr/share/applications/org.kde.kcalc.desktop", executable: "/usr/bin/kcalc" };
      const expiresInMs = scenario === "expiry" ? 6_000 : 60_000;
      const outcome = await new Promise<"approve" | "deny" | "expired">((resolve) => {
        pendingApproval = (approve) => resolve(approve ? "approve" : "deny");
        if (active?.taskId === identity.taskId) {
          active.tool = "apps.launch";
          active.approval = { operationId: "op_l2", expiresInMs, target, tool: "apps.launch" };
        }
        void turnEvent(identity, { kind: "approval_required", operationId: "op_l2", tool: "apps.launch", expiresInMs, target }, excluded);
        window.setTimeout(() => resolve("expired"), expiresInMs);
      });
      pendingApproval = null;
      if (active) active.approval = null;
      if (cancelled) throw new Error("cancelled");
      if (outcome === "deny") return finish("denied", null, { code: "approval_declined", message: "The application launch was declined." });
      if (outcome === "expired") return finish("timed_out", null, { code: "approval_expired", message: "The approval window expired." });
      await state("authorized");
      await dispatch("op_l2", "apps.launch", result("op_l2", "apps.launch", { app_id: "app_1", dispatched: true }), 500);
      await state("planning");
      await step(600);
      return finish("completed", "I asked the system to open **KCalc**, and the launch request was accepted.", null);
    }
    if (scenario === "mismatch" && /selected file|read/.test(prompt)) {
      await turnEvent(identity, { kind: "tool_proposed", tool: "files.read" }, excluded);
      await state("validating");
      await state("awaiting_authorization");
      return finish("denied", null, { code: "scope_kind_mismatch", message: "The request did not match the selected resource." });
    }
    if (/summar|main point|just return|previous result|what number|that tool/.test(prompt)) {
      await step(900);
      return finish("completed", priorRead
        ? "Juniper Orchard irrigates its young apple trees with rainwater collected from the north barn."
        : "I used the earlier saved answer in this conversation; no tool was needed.", null);
    }
    let tool = "math.calculate";
    let native: NativeResult;
    if (/read|first match|open the first/.test(prompt)) { tool = "files.read"; native = readResult("op_r1"); }
    else if (/find|search|ledger|folder/.test(prompt)) { tool = "files.search"; native = searchResult("op_s1", scenario === "synthfail"); }
    else if (/device|memory|operating system/.test(prompt)) {
      tool = "device.info";
      native = result("op_d1", tool, { os: "Linux (synthetic)", architecture: "x86_64", cpu: "Synthetic 6-core CPU", logical_cpus: 6, memory_total_bytes: 15_032_385_536, memory_available_bytes: 6_012_954_214, storage_total_bytes: null, gpu: null });
    } else native = result("op_c1", tool, { value: 12193 });
    await dispatch(native.record.operationId as string, tool, native, scenario === "slow" ? 60_000 : 900);
    await state("planning");
    await step(1000);
    if (scenario === "synthfail") return finish("failed", null, { code: "model_context_limit", message: "The task context exceeded the loaded model context." });
    const answers: Record<string, string> = {
      "math.calculate": "137 × 89 = **12,193**.",
      "files.search": "I found three ledger files: `alpha_ledger.txt`, `budget_ledger.txt` and `archive/ledger 2025.txt`.",
      "files.read": "`alpha_ledger.txt` says rainwater from the north barn is collected in two tanks and used to irrigate young apple trees.",
      "device.info": "This device runs Linux with 6 logical CPUs.",
    };
    return finish("completed", answers[tool], null);
  } catch {
    return finish("cancelled", null, { code: "cancelled", message: "The task was cancelled." });
  }
}

async function startTurn(conversationId: number, prompt: string, timeoutSeconds: number) {
  if (active) throw { code: "busy", message: "Another operation is in progress." };
  const conversation = find(conversationId);
  const timeoutMs = timeoutSeconds * 1000;
  const user = msg("user", prompt, "complete", Date.now());
  const assistant = msg("assistant", "", "streaming", Date.now());
  const taskId = String(nextTask++);
  const turn: Turn = { id: String(user.id), kind: "agent", user, assistants: [{ message: assistant, modelName: MODEL }], taskId, taskTimeoutMs: timeoutMs, cache: [] };
  conversation.turns.push(turn);
  if (conversation.title === "New conversation") conversation.title = prompt.slice(0, 48);
  conversation.updatedAt = Date.now();
  tasks.unshift({ id: taskId, state: "planning", createdAt: Date.now(), updatedAt: Date.now(), finalized: false, usage: null, error: null,
    errorMessage: null, conversationId: conversation.id, turnId: turn.id, excludedTurns: scenario === "omission" ? 3 : 0 });
  details[taskId] = { task: tasks[0], events: [], executions: [], audit: [], nativeResults: [] };
  const identity = { conversationId: conversation.id, turnId: turn.id, taskId };
  active = { ...identity, approval: null, tool: null, startedAt: Date.now(), taskTimeoutMs: timeoutMs };
  // Emitted before the command returns, as the host may do.
  await turnEvent(identity, { kind: "started", taskTimeoutMs: timeoutMs }, scenario === "omission" ? 3 : 0);
  window.setTimeout(() => void runTurn(conversation, turn), 30);
  return identity;
}

export async function installFixtures(name: string) {
  scenario = name;
  if (name === "research" || name === "researchnomodel") intelligence.localOnly = false;
  if (name === "researchnomodel") { inference = { phase: "empty", model: null }; activeModelId = null; }
  if (name === "nomodel" || bundledScenarios.has(name)) { inference = { phase: "empty", model: null }; activeModelId = null; }
  if (bundledScenarios.has(name)) models.splice(0, models.length);
  if (name === "onboarding") { inference = { phase: "empty", model: null }; activeModelId = null; models.splice(0, models.length); }
  if (name === "providers") {
    inference = { phase: "empty", model: null }; activeModelId = null; models.splice(0, models.length);
    const base = { contextTokens: 128000, maxOutputTokens: 2048, temperature: 0.7, streamUsage: true, imageInput: true };
    providers = [
      { id: 1, credential: "stored", settings: { ...base, name: "Example Cloud", endpoint: "https://api.example.com/v1", locality: "cloud",
        model: "example-large", requiresKey: true, retention: "provider_policy",
        retentionSummary: "Kept up to 30 days for abuse monitoring; not used for training.",
        sharing: { history: true, memories: false, documents: false },
        pricing: { currency: "USD", inputMicrosPerMillion: 2_500_000, outputMicrosPerMillion: 10_000_000 },
        budget: { monthlyTokens: 1_000_000, monthlyCostMicros: 5_000_000 } } },
      { id: 2, credential: "missing", settings: { ...base, imageInput: false, contextTokens: 8192, maxOutputTokens: 1024,
        name: "Home server", endpoint: "http://192.168.1.20:8000/v1", locality: "self_hosted", model: "llama-3-8b",
        requiresKey: false, retention: "none", retentionSummary: "", sharing: { history: true, memories: true, documents: true },
        pricing: null, budget: { monthlyTokens: null, monthlyCostMicros: null } } },
    ];
    intelligence = { localOnly: false, activeProvider: 1 };
  }
  if (name === "light") document.documentElement.dataset.appearance = "light";
  seed();
  if (name !== "nomodel" && name !== "restart" && !bundledScenarios.has(name)) {
    scopes.push(
      { id: "grant-a-1", displayName: "Ledgers", kind: "directory", access: "read_only", state: "active", remainingMs: 52 * minute },
      { id: "grant-a-2", displayName: "alpha_ledger.txt", kind: "file", access: "read_only", state: "active", remainingMs: 47 * minute },
    );
  }

  mockWindows("main");

  mockIPC(async (cmd, payload) => {
    const args = (payload ?? {}) as Json;
    switch (cmd) {
      case "bootstrap":
        return {
          preferences: { appearance: name === "light" ? "light" : "system", defaults: settings(768), enterToSend: true },
          plaintextBackup: null,
          databasePath: "/home/user/.local/share/io.github.gimesha-adikari.localfirstaiagent/chat/conversations.sqlite3",
          models, activeModelId, inference,
          intelligence: structuredClone(intelligence),
        };
      case "provider_list": return structuredClone(providerViews());
      case "database_recovery_key": return undefined;
      case "intelligence_settings": return structuredClone(intelligence);
      case "set_intelligence_settings":
        intelligence = { ...(args.settings as typeof intelligence) };
        return null;
      case "provider_save": {
        const saved = { id: Number(args.id ?? nextProvider++), settings: args.settings as Json, credential: "missing" };
        providers = [...providers.filter((item) => item.id !== saved.id), saved];
        return providerViews().find((item) => item.id === saved.id);
      }
      case "provider_delete":
        providers = providers.filter((item) => item.id !== Number(args.id));
        return null;
      case "provider_enter_credential":
        providers = providers.map((item) => item.id === Number(args.id) ? { ...item, credential: "stored" } : item);
        return "stored";
      case "provider_delete_credential":
        providers = providers.map((item) => item.id === Number(args.id) ? { ...item, credential: "missing" } : item);
        return "missing";
      case "web_research_settings": return webView();
      case "web_research_save": {
        const next = args.settings as typeof webSettings;
        if (next.searxngEndpoint && !/^https?:\/\//.test(next.searxngEndpoint)) throw { code: "invalid_config", message: "Enter a valid http or https URL." };
        if (next.provider === "purili" && !next.puriliPersonalUse) throw { code: "invalid_config", message: "Purili allows only personal, non-commercial use. Confirm that below to select it." };
        webSettings = { ...next, searxngEndpoint: next.searxngEndpoint.replace(/\/$/, "") };
        return webView();
      }
      case "web_research_enter_key": braveKey = "stored"; return "stored";
      case "web_research_delete_key": braveKey = "missing"; return null;
      case "chat_web_research": return structuredClone(researchViews.get(Number(args.messageId)) ?? null);
      case "web_research_open": return null;
      case "provider_test": return { status: 200, models: ["fixture-model", "other-model"], modelListed: true, elapsedMs: 182 };
      case "list_conversations": {
        const query = String(args.query ?? "").toLowerCase();
        return conversations
          .filter((item) => !query || item.title.toLowerCase().includes(query) || item.turns.some((turn) =>
            turn.user.content.toLowerCase().includes(query) || turn.assistants.some((a) => a.message.content.toLowerCase().includes(query))))
          .sort((a, b) => b.updatedAt - a.updatedAt)
          .map(({ id, title, updatedAt }) => ({ id, title, updatedAt }));
      }
      case "get_unified_conversation": return structuredClone(unified(find(args.id)));
      case "create_conversation": {
        const conversation: Conversation = { id: 10 + conversations.length, title: "New conversation", createdAt: Date.now(), updatedAt: Date.now(), systemPrompt: "", turns: [] };
        conversations.unshift(conversation);
        return { id: conversation.id, title: conversation.title, updatedAt: conversation.updatedAt, createdAt: conversation.createdAt, systemPrompt: "", messages: [] };
      }
      case "rename_conversation": find(args.id).title = String(args.title); return null;
      case "set_system_prompt": find(args.id).systemPrompt = String(args.systemPrompt); return null;
      case "delete_conversation": {
        if (active?.conversationId === Number(args.id)) throw { code: "busy", message: "Another operation is in progress." };
        const conversation = find(args.id);
        for (const turn of conversation.turns) if (turn.taskId) {
          delete details[turn.taskId];
          tasks.splice(tasks.findIndex((task) => task.id === turn.taskId), 1);
        }
        conversations.splice(conversations.indexOf(conversation), 1);
        return null;
      }
      case "send_message": return streamChat(args, false);
      case "regenerate": return streamChat(args, true);
      case "stop_generation": return true;
      case "continue_chat": return continueChat(args);
      case "chat_continuation_remaining": {
        const left = continuations.get(Number(args.messageId)) ?? 0;
        if (left <= 0) throw { code: "invalid_config", message: "This response can't be continued" };
        return left;
      }
      case "recommended_chat_settings": {
        const trained = (inference as Json).model ? Number(((inference as Json).model as Json).trainedContext) : null;
        const context = trained === null ? 8192 : Math.min(8192, trained);
        return { settings: { ...settings(Math.min(4096, context / 2)), contextLength: context, threads: 4 }, trainedContext: trained };
      }
      case "inference_status": return inference;
      case "prepare_bundled_model": {
        await wait(name === "firstlaunch" ? 1200 : 0);
        if (name === "corrupted") return { state: "corrupted", model: null, autoLoad: false };
        if (name === "nobundle" || name === "onboarding" || name === "providers") return { state: "missing", model: null, autoLoad: false };
        if (!models.some((item) => item.id === lfm.id)) models.push(lfm);
        return { state: "ready", model: lfm, autoLoad: models.length === 1 };
      }
      case "choose_model": return null;
      case "load_model": {
        await wait(900);
        activeModelId = Number(args.modelId);
        inference = { phase: "ready", model: activeModelId === 1 ? qwen3 : activeModelId === 3 ? lfmInfo : qwen35 };
        return (inference as Json).model;
      }
      case "unload_model": await wait(400); inference = { phase: "empty", model: null }; activeModelId = null; return null;
      case "save_model_settings": {
        const model = models.find((item) => item.id === args.modelId);
        if (model) model.settings = args.settings as typeof model.settings;
        return null;
      }
      case "save_preferences": return null;
      case "start_agent_task": throw { code: "unsupported", message: "The fixture expects start_agent_turn." };
      case "start_agent_turn": return startTurn(Number(args.conversationId), String(args.prompt), Number(args.taskTimeoutSeconds ?? 300));
      case "get_active_agent_turn": return active ? structuredClone(active) : null;
      case "cancel_agent_task": {
        if (!active || active.taskId !== String(args.taskId)) return false;
        cancelTask?.();
        pendingApproval?.(false);
        return true;
      }
      case "respond_agent_approval": {
        if (!active || active.taskId !== String(args.taskId) || (active.approval as Json | null)?.operationId !== args.operationId) {
          throw { code: "invalid_request", message: "That approval is no longer active." };
        }
        pendingApproval?.(Boolean(args.approve));
        return null;
      }
      case "list_agent_tasks": return structuredClone(tasks);
      case "get_agent_task": {
        const detail = details[String(args.taskId)];
        if (!detail) throw { code: "not_found", message: "Task not found." };
        const turn = conversations.flatMap((item) => item.turns).find((item) => item.taskId === String(args.taskId));
        return structuredClone({ ...detail, nativeResults: turn ? turn.cache : detail.nativeResults });
      }
      case "choose_agent_directory": {
        const scope = { id: `grant-a-${scopes.length + 1}`, displayName: "Reports", kind: "directory", access: "read_only", state: "active", remainingMs: 60 * minute };
        scopes.push(scope);
        return scope;
      }
      case "choose_agent_file": {
        const scope = { id: `grant-a-${scopes.length + 1}`, displayName: "budget.txt", kind: "file", access: "read_only", state: "active", remainingMs: 60 * minute };
        scopes.push(scope);
        return scope;
      }
      case "list_agent_scopes": return structuredClone(scopes);
      case "revoke_agent_scope": {
        const scope = scopes.find((item) => item.id === args.scopeId);
        if (!scope) return false;
        scope.state = "revoked";
        scope.remainingMs = 0;
        // Results tied to a revoked resource stop being available.
        for (const turn of conversations.flatMap((item) => item.turns)) {
          turn.cache = turn.cache.filter((cached) => !String(cached.observation.tool).startsWith("files."));
        }
        return true;
      }
      default:
        throw { code: "unknown_command", message: `Fixture has no handler for ${cmd}.` };
    }
  }, { shouldMockEvents: true });
  if (name === "recover") {
    // A turn already running in the host before this window mounted. Its early
    // events, including the approval request, are missed and must be recovered.
    await startTurn(5, "Open KCalc so I can check the ledger totals.", 300);
    for (let waited = 0; waited < 8000 && !(active?.approval); waited += 100) await wait(100);
  }
}
