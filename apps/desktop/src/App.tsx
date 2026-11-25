import { Channel, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { defaultTaskTimeoutSeconds, validTimeout } from "./agent";
import { LegacyRecord } from "./AgentView";
import {
  chatRequestOptions, commandError, continuationCandidate, continuationErrorCopy, latestChatAttempt, lowOutputLimit,
  validModelSettings,
  type Bootstrap, type BundledModel, type ChatEvent, type ContinuationStatus, type Conversation, type ConversationSummary, type Message,
  type ModelReference, type ModelSettings, type RecommendedChatSettings,
} from "./chat";
import { type RegenerateRequest } from "./ChatView";
import ConversationView from "./ConversationView";
import Composer, { modelLabel, type ComposerMode } from "./Composer";
import DetailsDrawer from "./DetailsDrawer";
import {
  AccessDialog, EditConversationDialog, ModelsDialog, SettingsDialog, type ModelAction,
} from "./Dialogs";
import {
  AgentIcon, ArrowDownIcon, ChatIcon, EditIcon, MoreIcon, NoteIcon, PanelRightIcon, Spinner, TrashIcon,
} from "./icons";
import { errorMessage, type ModelInfo, type Status } from "./inference";
import {
  advancedModelName, agentUnavailableText, builtInModelName, capabilityGuidance, modelProfile,
} from "./models";
import Sidebar, { type RecentItem, type RecordItem } from "./Sidebar";
import { Callout, IconButton, Menu, MenuItem } from "./ui";
import {
  agentTurnTaskIds, applyChatEventToTurns, canRegenerateLatest,
  type AgentTurnIdentity, type UnifiedConversation,
} from "./unified";
import { useAgent } from "./useAgent";

type Modal = "models" | "settings" | "access" | "rename" | "delete" | "instructions";
type Operation = "opening" | "creating" | "saving" | "generating";
type View = { kind: "empty" } | { kind: "conversation"; id: number } | { kind: "record"; taskId: string };

const toUnified = (conversation: Conversation): UnifiedConversation => ({
  id: conversation.id, title: conversation.title, systemPrompt: conversation.systemPrompt,
  createdAt: conversation.createdAt, updatedAt: conversation.updatedAt, turns: [],
});

const narrowQuery = "(max-width: 860px)";

const chatSuggestions = [
  "Explain Rust ownership with a short example",
  "Write a four-line poem about rain",
  "Summarize the pros and cons of local AI models",
];
const agentSuggestions = [
  "What is 137 × 89? Use the calculator.",
  "How much memory and storage does this device have?",
  "Find the text files in my selected folder.",
  "Which desktop applications are installed?",
];

function useMediaQuery(query: string) {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const update = () => setMatches(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [query]);
  return matches;
}

const recordTitle = (createdAt: number) =>
  `Agent task · ${new Date(createdAt).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`;

const shorten = (text: string, length = 60) => (text.length > length ? `${text.slice(0, length)}…` : text);

export default function App() {
  const [snapshot, setSnapshot] = useState<Bootstrap | null>(null);
  const [summaries, setSummaries] = useState<ConversationSummary[]>([]);
  const [conversation, setConversation] = useState<UnifiedConversation | null>(null);
  const [loadedAt, setLoadedAt] = useState(0);
  const [drawerTurnId, setDrawerTurnId] = useState<string | null>(null);
  const [pendingAgent, setPendingAgent] = useState<{ conversationId: number; prompt: string } | null>(null);
  const [view, setView] = useState<View>({ kind: "empty" });
  const [mode, setMode] = useState<ComposerMode>("chat");
  const [draft, setDraft] = useState("");
  const [thinking, setThinking] = useState(false);
  const [outputOverride, setOutputOverride] = useState("");
  const [taskTimeout, setTaskTimeout] = useState(String(defaultTaskTimeoutSeconds));
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [modal, setModal] = useState<Modal | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const narrow = useMediaQuery(narrowQuery);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => window.matchMedia(narrowQuery).matches);
  const [error, setError] = useState<string | null>(null);
  const [startupError, setStartupError] = useState<string | null>(null);
  const [operation, setOperation] = useState<Operation | null>(null);
  const operationRef = useRef<Operation | null>(null);
  const [stopping, setStopping] = useState(false);
  const [modelAction, setModelAction] = useState<ModelAction | null>(null);
  const [modelError, setModelError] = useState<string | null>(null);
  const [modelNotice, setModelNotice] = useState<string | null>(null);
  const [selectedModelId, setSelectedModelId] = useState<number | null>(null);
  const [dismissedLimit, setDismissedLimit] = useState<number | null>(null);
  const [editError, setEditError] = useState<string | null>(null);
  const [continuation, setContinuation] = useState<ContinuationStatus | null>(null);
  const [continueFailure, setContinueFailure] = useState<{ messageId: number; title: string; detail: string } | null>(null);
  const [recommended, setRecommended] = useState<RecommendedChatSettings | null>(null);
  const [autoLoad, setAutoLoad] = useState<ModelReference | null>(null);
  /** Bundled model verification: hashing the resource on first launch takes a few seconds. */
  const [bundled, setBundled] = useState<BundledModel["state"] | "checking" | "unavailable">("checking");
  const viewRef = useRef<View>({ kind: "empty" });
  const agent = useAgent({ onTurnSettled: (identity) => settleTurn(identity) });
  viewRef.current = view;
  const request = useRef(0);
  const scroll = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const composer = useRef<HTMLTextAreaElement>(null);
  const initialized = snapshot !== null;
  const appearance = snapshot?.preferences.appearance ?? "system";

  useEffect(() => {
    let active = true;
    void Promise.all([
      invoke<Bootstrap>("bootstrap"),
      invoke<ConversationSummary[]>("list_conversations", { query: "" }),
    ]).then(([initial, list]) => {
      if (!active) return;
      setSnapshot(initial);
      setSelectedModelId(initial.activeModelId ?? initial.models[0]?.id ?? null);
      setSummaries(list);
      // Verify and register the bundled model. Existing models, their order
      // and settings are unchanged; it loads only when it is the only model.
      void invoke<BundledModel>("prepare_bundled_model").then((bundled) => {
        if (!active) return;
        setBundled(bundled.state);
        const model = bundled.model;
        if (model) {
          setSnapshot((current) => current && (current.models.some(({ id }) => id === model.id) ? current
            : { ...current, models: [...current.models, model] }));
          setSelectedModelId((current) => current ?? model.id);
          if (bundled.autoLoad && initial.activeModelId === null) setAutoLoad(model);
        }
        // A corrupted resource is explained in the new-thread view and in Models.
      }).catch(() => {
        // Without a resource folder the app works with imported models only.
        if (active) setBundled("unavailable");
      });
    }).catch((failure: unknown) => {
      if (active) setStartupError(errorMessage(failure));
    });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (!initialized) return;
    let active = true;
    const timer = window.setTimeout(() => {
      void invoke<ConversationSummary[]>("list_conversations", { query: query.trim() })
        .then((list) => {
          if (!active) return;
          setSummaries(list);
          setSearchError(null);
        })
        .catch((failure: unknown) => { if (active) setSearchError(errorMessage(failure)); });
    }, query ? 160 : 0);
    return () => { active = false; window.clearTimeout(timer); };
  }, [initialized, query, refresh]);

  useEffect(() => {
    document.documentElement.dataset.appearance = appearance;
  }, [appearance]);

  useEffect(() => {
    if (narrow) setSidebarCollapsed(true);
  }, [narrow]);

  // Escape closes the topmost non-modal panel. Modal dialogs handle Escape themselves.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || document.querySelector("dialog[open]")) return;
      if (drawerOpen) setDrawerOpen(false);
      else if (narrow && !sidebarCollapsed) setSidebarCollapsed(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [drawerOpen, narrow, sidebarCollapsed]);

  // Keep the newest content in view unless the person scrolled up to read.
  const viewKey = view.kind === "conversation" ? `conversation-${view.id}` : view.kind === "record" ? `record-${view.taskId}` : "empty";
  useLayoutEffect(() => {
    follow.current = true;
    setShowJump(false);
  }, [viewKey]);
  useLayoutEffect(() => {
    const node = scroll.current;
    if (node && follow.current) node.scrollTop = node.scrollHeight;
  }, [conversation?.turns, agent.turns, pendingAgent, viewKey, operation]);

  // Late content (journal details, native results) can grow the thread after the
  // first scroll; keep following it while the person hasn't scrolled up.
  useEffect(() => {
    const node = scroll.current;
    const content = node?.firstElementChild;
    if (!node || !content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (follow.current) node.scrollTop = node.scrollHeight;
    });
    observer.observe(content);
    return () => observer.disconnect();
  }, []);

  function onScroll() {
    const node = scroll.current;
    if (!node) return;
    follow.current = node.scrollHeight - node.scrollTop - node.clientHeight < 64;
    setShowJump(!follow.current);
  }

  function jumpToLatest() {
    follow.current = true;
    setShowJump(false);
    scroll.current?.scrollTo({ top: scroll.current.scrollHeight });
  }

  const agentBusy = agent.running || agent.starting;
  const chatBusy = operation !== null;
  const busy = chatBusy || agentBusy || modelAction !== null;
  const generating = operation === "generating";
  const phase = snapshot?.inference.phase ?? "empty";
  const modelInfo = snapshot?.inference.model ?? null;
  const modelReady = phase === "ready";
  const activeReference = snapshot?.models.find((model) => model.id === snapshot.activeModelId) ?? null;
  const supportsThinking = !!modelInfo?.supportsThinking;
  const supportsAgent = !!modelInfo?.supportsAgent;
  const agentUnavailable = !agent.eventReady ? "Connecting to Agent events…"
    : !modelReady ? "Load a model to use Agent mode"
      : !supportsAgent ? agentUnavailableText(modelInfo)
        : null;
  const agentAvailable = agentUnavailable === null;

  // Ask the backend whether the newest Chat reply can continue. It owns the
  // replay seed, model fingerprint and request caps; the UI only offers the action.
  const candidate = conversation && !generating ? latestChatAttempt(conversation.turns) : null;
  const candidateId = continuationCandidate(candidate) ? candidate!.id : null;
  const conversationId = conversation?.id ?? null;
  const activeModelId = snapshot?.activeModelId ?? null;
  useEffect(() => {
    if (candidateId === null || conversationId === null) {
      setContinuation(null);
      return;
    }
    if (!modelReady) {
      setContinuation({
        messageId: candidateId, remaining: null, checking: false,
        blocked: continuationErrorCopy({ code: "no_model", message: "" }),
      });
      return;
    }
    let active = true;
    setContinuation({ messageId: candidateId, remaining: null, checking: true, blocked: null });
    invoke<number>("chat_continuation_remaining", { id: conversationId, messageId: candidateId })
      .then((remaining) => {
        if (active) setContinuation({ messageId: candidateId, remaining, checking: false, blocked: null });
      })
      .catch((failure: unknown) => {
        if (active) {
          setContinuation({
            messageId: candidateId, remaining: null, checking: false, blocked: continuationErrorCopy(commandError(failure)),
          });
        }
      });
    return () => { active = false; };
  }, [conversationId, candidateId, modelReady, activeModelId, loadedAt]);
  const continuationView: ContinuationStatus | null = continuation && continueFailure?.messageId === continuation.messageId
    ? { ...continuation, blocked: continueFailure } : continuation;

  function begin(next: Operation) {
    if (operationRef.current !== null || (agentBusy && next !== "opening")) return false;
    operationRef.current = next;
    setOperation(next);
    setError(null);
    return true;
  }

  function finish() {
    operationRef.current = null;
    setOperation(null);
  }

  const focusComposer = useCallback(() => {
    composer.current?.focus();
    window.requestAnimationFrame(() => composer.current?.focus());
  }, []);

  /** Read the saved unified thread. Never triggers a native operation. */
  async function loadConversation(id: number) {
    const next = await invoke<UnifiedConversation>("get_unified_conversation", { id });
    const current = viewRef.current;
    if (current.kind !== "conversation" || current.id !== id) return next;
    setConversation(next);
    setLoadedAt(Date.now());
    // Saved status, errors and omission counts come with the turn summary. Fetch a
    // journal only when the session still holds native results to show for it;
    // other journals load when their details are opened.
    for (const taskId of agentTurnTaskIds(next, { withNativeResults: true })) {
      if (!agent.details[taskId] || !agent.details[taskId].task.finalized) void agent.loadDetail(taskId);
    }
    return next;
  }

  function settleTurn(identity: AgentTurnIdentity) {
    setRefresh((tick) => tick + 1);
    const current = viewRef.current;
    if (current.kind === "conversation" && current.id === identity.conversationId) {
      void loadConversation(identity.conversationId).catch((failure: unknown) => setError(errorMessage(failure)));
    }
  }

  function newThread() {
    if (operationRef.current !== null || agent.starting) return;
    setView({ kind: "empty" });
    setConversation(null);
    setDrawerTurnId(null);
    setError(null);
    agent.setError(null);
    if (narrow) setSidebarCollapsed(true);
    focusComposer();
  }

  async function openConversation(id: number) {
    if (operationRef.current !== null || agent.starting) return;
    if (narrow) setSidebarCollapsed(true);
    setError(null);
    if (view.kind === "conversation" && view.id === id) return;
    if (!begin("opening")) return;
    const target: View = { kind: "conversation", id };
    viewRef.current = target;
    setView(target);
    setConversation(null);
    setDrawerTurnId(null);
    try {
      await loadConversation(id);
      focusComposer();
    } catch (failure) {
      setError(errorMessage(failure));
      setView({ kind: "empty" });
    } finally {
      finish();
    }
  }

  function openRecord(record: RecordItem) {
    if (operationRef.current !== null || agent.starting) return;
    if (narrow) setSidebarCollapsed(true);
    setError(null);
    setConversation(null);
    setDrawerTurnId(null);
    setView({ kind: "record", taskId: record.id });
    void agent.loadDetail(record.id);
  }

  const parsedOverride = (() => {
    if (!outputOverride.trim()) return null;
    const value = Number(outputOverride);
    const limit = modelInfo?.config.contextLength ?? 4097;
    return Number.isInteger(value) && value >= 1 && value <= 4096 && value < limit ? value : NaN;
  })();

  /** Use the open conversation, or create one for the first message of a new thread. */
  async function ensureConversation(): Promise<{ conversation: UnifiedConversation; created: boolean } | null> {
    if (conversation) return { conversation, created: false };
    try {
      const created = toUnified(await invoke<Conversation>("create_conversation"));
      const target: View = { kind: "conversation", id: created.id };
      viewRef.current = target;
      setView(target);
      setConversation(created);
      setLoadedAt(Date.now());
      setRefresh((tick) => tick + 1);
      return { conversation: created, created: true };
    } catch (failure) {
      setError(errorMessage(failure));
      return null;
    }
  }

  async function generate(kind: "send" | "regenerate" | "continue", regenerate: RegenerateRequest = {}, messageId?: number) {
    if (!snapshot || !modelReady) return;
    const content = draft;
    if (kind === "send" && !content.trim()) return;
    if (kind === "regenerate" && !canRegenerateLatest(conversation)) return;
    if (kind === "continue" && (!conversation || messageId === undefined
      || latestChatAttempt(conversation.turns)?.id !== messageId)) return;
    if (Number.isNaN(parsedOverride)) return;
    if (!begin("generating")) return;
    setContinueFailure(null);

    const ensured = await ensureConversation();
    if (!ensured) {
      finish();
      return;
    }
    const id = ensured.conversation.id;
    const before = new Set(ensured.conversation.turns.map((turn) => turn.id));
    const modelName = activeReference?.fileName ?? null;
    const options = chatRequestOptions({
      thinking, supportsThinking,
      maxTokens: regenerate.maxTokens ?? (parsedOverride === null ? null : parsedOverride),
    });
    const generation = ++request.current;
    follow.current = true;
    const events = new Channel<ChatEvent>((event) => {
      if (request.current !== generation) return;
      if (event.kind === "started") {
        if (event.user && kind === "send") setDraft("");
        setOutputOverride("");
      }
      setConversation((current) => current?.id === id
        ? { ...current, turns: applyChatEventToTurns(current.turns, event, modelName) }
        : current);
    });
    let failureMessage: string | null = null;
    let sent = false;
    try {
      if (kind === "continue") {
        // Appends to the same assistant message. Sampling and Thinking come from the
        // original attempt; an optional ceiling applies to new tokens only.
        await invoke<Message>("continue_chat", {
          id, messageId, events, ...(parsedOverride === null ? {} : { maxTokens: parsedOverride }),
        });
      } else {
        await invoke<Message>(kind === "send" ? "send_message" : "regenerate", kind === "send"
          ? { id, content, options, events }
          : { id, options, events });
      }
      sent = kind === "send";
    } catch (failure) {
      if (kind === "continue" && messageId !== undefined) {
        setContinueFailure({ messageId, ...continuationErrorCopy(commandError(failure)) });
      } else {
        failureMessage = errorMessage(failure);
      }
    } finally {
      request.current++;
      // A rejected request may still have saved the user message and a partial reply.
      try {
        const saved = await loadConversation(id);
        if (kind === "send" && (sent || saved.turns.some((turn) =>
          turn.user.content === content && !before.has(turn.id)))) {
          setDraft((current) => (current === content ? "" : current));
        }
      } catch (failure) {
        if (!failureMessage) failureMessage = errorMessage(failure);
      }
      if (failureMessage) setError(failureMessage);
      setRefresh((tick) => tick + 1);
      setStopping(false);
      finish();
      focusComposer();
    }
  }

  async function stopChat() {
    setStopping(true);
    try {
      if (!await invoke<boolean>("stop_generation")) setStopping(false);
    } catch (failure) {
      setError(errorMessage(failure));
      setStopping(false);
    }
  }

  /** Start a linked Agent turn in the open conversation (or a new one). */
  async function startAgentTurn() {
    const text = draft.trim();
    const seconds = Number(taskTimeout);
    if (!text || !agentAvailable || busy || !validTimeout(seconds)) return;
    agent.setError(null);
    setError(null);
    if (!begin("creating")) return;
    const ensured = await ensureConversation();
    finish();
    if (!ensured) return;
    const conversationId = ensured.conversation.id;
    follow.current = true;
    setPendingAgent({ conversationId, prompt: text });
    const identity = await agent.start(conversationId, text, seconds);
    setPendingAgent(null);
    if (identity) {
      setDraft((current) => (current.trim() === text ? "" : current));
      setRefresh((tick) => tick + 1);
      try {
        await loadConversation(identity.conversationId);
      } catch (failure) {
        setError(errorMessage(failure));
      }
    } else if (ensured.created) {
      // Don't leave an empty conversation behind when the first turn couldn't start.
      try {
        await invoke("delete_conversation", { id: conversationId });
      } catch {
        // The empty conversation stays; it can be deleted from its menu.
      }
      setConversation(null);
      setView({ kind: "empty" });
      setRefresh((tick) => tick + 1);
    }
    focusComposer();
  }

  function submit() {
    if (mode === "agent") void startAgentTurn();
    else void generate("send");
  }

  async function revokeScope(scope: Parameters<typeof agent.revoke>[0]) {
    if (await agent.revoke(scope) && conversation) {
      // Revocation can remove session results tied to that resource.
      void loadConversation(conversation.id).catch(() => {});
    }
  }

  async function refreshStatus() {
    try {
      const status = await invoke<Status>("inference_status");
      setSnapshot((current) => current && { ...current, inference: status, activeModelId: status.model ? current.activeModelId : null });
    } catch {
      // Keep the last known state; the action's own error is shown.
    }
  }

  async function modelOperation(kind: ModelAction, work: () => Promise<void>) {
    if (busy) return false;
    setModelAction(kind);
    setModelError(null);
    setModelNotice(null);
    try {
      await work();
      return true;
    } catch (failure) {
      const message = errorMessage(failure);
      setModelError(message);
      if (modal !== "models") setError(message);
      return false;
    } finally {
      setModelAction(null);
    }
  }

  async function loadModel(reference: ModelReference, settings: ModelSettings) {
    if (!snapshot || !validModelSettings(settings)) return;
    const switching = snapshot.activeModelId !== null && snapshot.activeModelId !== reference.id && phase === "ready";
    setSelectedModelId(reference.id);
    await modelOperation(switching ? "switch" : "load", async () => {
      try {
        if (switching) {
          await invoke("unload_model");
          setSnapshot((current) => current && { ...current, activeModelId: null, inference: { phase: "empty", model: null } });
        }
        setSnapshot((current) => current && { ...current, inference: { phase: "loading", model: null } });
        const model = await invoke<ModelInfo>("load_model", { modelId: reference.id, settings });
        setSnapshot((current) => current && {
          ...current,
          models: [{ ...reference, settings }, ...current.models.filter(({ id }) => id !== reference.id)],
          activeModelId: reference.id,
          inference: { phase: "ready", model },
        });
        setSelectedModelId(reference.id);
        setModelNotice(`${modelLabel(reference.fileName)} is ready.`);
      } catch (failure) {
        await refreshStatus();
        throw failure;
      }
    });
  }

  // First-launch default: load the verified bundled model when it is the only one.
  useEffect(() => {
    if (!autoLoad || !snapshot || busy) return;
    setAutoLoad(null);
    if (snapshot.activeModelId === null && phase === "empty") void loadModel(autoLoad, autoLoad.settings);
    // loadModel reads current state; this effect only reacts to a new request.
  },[autoLoad, snapshot, busy, phase]);

  function selectModelFromComposer(id: number) {
    const reference = snapshot?.models.find((model) => model.id === id);
    if (reference) void loadModel(reference, reference.settings);
  }

  async function unloadModel() {
    await modelOperation("unload", async () => {
      try {
        await invoke("unload_model");
        setSnapshot((current) => current && { ...current, activeModelId: null, inference: { phase: "empty", model: null } });
        setModelNotice("Model unloaded.");
      } catch (failure) {
        await refreshStatus();
        throw failure;
      }
    });
  }

  async function importModel() {
    await modelOperation("import", async () => {
      const reference = await invoke<ModelReference | null>("choose_model");
      if (reference) {
        setSnapshot((current) => current && { ...current, models: [reference, ...current.models.filter(({ id }) => id !== reference.id)] });
        setSelectedModelId(reference.id);
        setModelNotice(`Imported ${modelLabel(reference.fileName)}.`);
      }
    });
  }

  async function saveModelSettings(reference: ModelReference, settings: ModelSettings) {
    return modelOperation("save", async () => {
      await invoke("save_model_settings", { modelId: reference.id, settings });
      setSnapshot((current) => current && {
        ...current,
        models: current.models.map((model) => (model.id === reference.id ? { ...model, settings } : model)),
      });
      setModelNotice("Settings saved.");
    });
  }

  /**
   * Reload the active model with new settings. The host only accepts context and
   * thread changes for an unloaded model, so unload, save, then load.
   */
  async function reloadWithSettings(reference: ModelReference, settings: ModelSettings) {
    if (!validModelSettings(settings) || snapshot?.activeModelId !== reference.id) return;
    await modelOperation("switch", async () => {
      try {
        await invoke("unload_model");
        setSnapshot((current) => current && { ...current, activeModelId: null, inference: { phase: "empty", model: null } });
        await invoke("save_model_settings", { modelId: reference.id, settings });
        setSnapshot((current) => current && {
          ...current,
          models: current.models.map((model) => (model.id === reference.id ? { ...model, settings } : model)),
          inference: { phase: "loading", model: null },
        });
        const model = await invoke<ModelInfo>("load_model", { modelId: reference.id, settings });
        setSnapshot((current) => current && {
          ...current,
          models: current.models.map((item) => (item.id === reference.id ? { ...item, settings } : item)),
          activeModelId: reference.id,
          inference: { phase: "ready", model },
        });
        setModelNotice(`Settings saved. ${modelLabel(reference.fileName)} reloaded with ${model.config.contextLength.toLocaleString()} tokens of context.`);
      } catch (failure) {
        await refreshStatus();
        throw failure;
      }
    });
  }

  async function checkRecommended() {
    try {
      setRecommended(await invoke<RecommendedChatSettings>("recommended_chat_settings"));
    } catch {
      setRecommended(null);
    }
  }

  async function editConversation(value: string) {
    if (!conversation || !modal || !begin("saving")) return;
    const id = conversation.id;
    setEditError(null);
    try {
      if (modal === "delete") {
        await invoke("delete_conversation", { id });
        agent.forgetConversation(id);
        setConversation(null);
        setDrawerTurnId(null);
        setView({ kind: "empty" });
        void agent.refreshTasks();
      } else {
        if (modal === "rename") await invoke("rename_conversation", { id, title: value.trim() });
        if (modal === "instructions") await invoke("set_system_prompt", { id, systemPrompt: value });
        await loadConversation(id);
      }
      setModal(null);
      setRefresh((tick) => tick + 1);
    } catch (failure) {
      setEditError(errorMessage(failure));
    } finally {
      finish();
    }
  }

  function openModal(next: Modal) {
    setModelError(null);
    setModelNotice(null);
    setEditError(null);
    if (next === "models") {
      setSelectedModelId(snapshot?.activeModelId ?? selectedModelId ?? snapshot?.models[0]?.id ?? null);
      void checkRecommended();
    }
    setModal(next);
  }

  // One sidebar entry per conversation; journal records stay in their own section.
  const recent = useMemo<RecentItem[]>(() => summaries.map((summary) => ({
    key: `conversation-${summary.id}`, id: summary.id, title: summary.title, updatedAt: summary.updatedAt,
    running: agent.active?.conversationId === summary.id,
  })), [summaries, agent.active]);

  const records = useMemo<RecordItem[]>(() => agent.tasks
    .filter((task) => task.conversationId === null)
    .map((task) => ({ key: `record-${task.id}`, id: task.id, title: recordTitle(task.createdAt), updatedAt: task.updatedAt, state: task.state })),
  [agent.tasks]);

  const currentKey = view.kind === "conversation" ? `conversation-${view.id}` : view.kind === "record" ? `record-${view.taskId}` : null;
  const activeHere = !!agent.active && view.kind === "conversation" && view.id === agent.active.conversationId;
  const returnToAgent = !!agent.active && !activeHere;
  const activeTitle = agent.active ? summaries.find((item) => item.id === agent.active?.conversationId)?.title ?? "another conversation" : null;
  const editing = modal === "rename" || modal === "delete" || modal === "instructions";
  const contextLength = modelInfo?.config.contextLength ?? null;

  const title = view.kind === "conversation" ? conversation?.title ?? "Conversation"
    : view.kind === "record" ? recordTitle(agent.tasks.find((task) => task.id === view.taskId)?.createdAt ?? Date.now())
      : "New thread";

  // Details drawer: the chosen Agent turn, or the latest one in this conversation.
  const drawerTurn = view.kind === "conversation" && conversation
    ? conversation.turns.find((turn) => turn.id === drawerTurnId && turn.kind === "agent")
      ?? [...conversation.turns].reverse().find((turn) => turn.kind === "agent")
    : undefined;
  const drawerTask = view.kind === "record"
    ? { live: null, summary: agent.tasks.find((task) => task.id === view.taskId) ?? null, detail: agent.details[view.taskId] ?? null, label: "Execution record" }
    : drawerTurn?.taskId
      ? { live: agent.turns[drawerTurn.id] ?? null, summary: null, detail: agent.details[drawerTurn.taskId] ?? null, label: `Agent turn · ${shorten(drawerTurn.user.content, 48)}` }
      : null;

  // Detailed journals load on demand when their details are opened.
  const drawerTaskId = drawerOpen && view.kind === "conversation" ? drawerTurn?.taskId ?? null : null;
  const drawerDetailLoaded = !!drawerTaskId && !!agent.details[drawerTaskId];
  useEffect(() => {
    if (drawerTaskId && !drawerDetailLoaded) void agent.loadDetail(drawerTaskId);
  }, [drawerTaskId, drawerDetailLoaded]);

  const statusText = modelAction === "load" || modelAction === "switch" || phase === "loading" ? "Loading model…"
    : modelAction === "unload" || phase === "unloading" ? "Unloading…"
      : modelInfo ? modelLabel(modelInfo.fileName) : "No model loaded";

  const showLimitBanner = mode === "chat" && modelReady && !!activeReference && lowOutputLimit(activeReference.settings)
    && dismissedLimit !== activeReference.id && view.kind !== "record";

  const canSend = !busy && !!draft.trim() && (mode === "agent"
    ? agentAvailable && validTimeout(Number(taskTimeout))
    : !!snapshot && modelReady && !Number.isNaN(parsedOverride));

  const composerStatus = !snapshot ? (startupError ? "The app couldn't start." : "Starting…")
    : generating ? (stopping ? "Stopping…" : "Generating locally…")
      : agent.running ? (activeHere
        ? (agent.activeApproval ? "Waiting for your approval. Approve, deny or cancel above." : "Agent is working. You can cancel at any time.")
        : `An Agent turn is running in “${shorten(activeTitle ?? "another conversation", 40)}”.`)
        : modelAction ? statusText
          : !modelReady ? (bundled === "checking" && !snapshot.models.length ? "Checking the built-in assistant…"
            : phase === "loading" ? "Loading model…" : "Load a model to start · Everything runs on this device")
            : mode === "agent" ? (agentUnavailable ?? "Agent uses only tools and files you allow · Local only")
              : `Local inference · ${snapshot.preferences.enterToSend ? "Enter to send, Shift+Enter for a new line" : "Use the send button to send"}`;

  const inThread = view.kind === "conversation" && !!conversation?.turns.length;
  const placeholder = mode === "agent"
    ? (inThread ? "Ask the agent a follow-up…" : "Describe a task for the agent…")
    : (inThread ? "Reply…" : "Ask anything…");

  const loadingReference = snapshot?.models.find((model) => model.id === selectedModelId) ?? null;
  const loadingBuiltIn = !!(autoLoad || (loadingReference && modelProfile(loadingReference.fileName)?.builtIn));
  const chatGuidance = modelInfo && modelProfile(modelInfo.fileName)?.builtIn && !supportsAgent
    ? `You're using ${builtInModelName}, the built-in lightweight assistant, for Chat. For Agent tasks and Thinking, select or import ${advancedModelName}.`
    : null;

  const emptyState = (
    <section className="empty-workspace" aria-label="New thread">
      <h2>{mode === "agent" ? "What should the agent do?" : "What can I help with?"}</h2>
      <p>
        {mode === "agent"
          ? "The agent can calculate, describe this device, search and read files you select, and find or open applications after you approve."
          : "Chat runs entirely on this computer with your local model."}
      </p>
      {!snapshot && !startupError && <p className="hint" role="status">Starting…</p>}
      {startupError && <Callout tone="error" title="The app couldn't start" role="alert">{startupError}</Callout>}
      {snapshot && (phase === "loading" || modelAction === "load" || modelAction === "switch") && (
        <p className="hint inline-status" role="status">
          <Spinner size={13} />{loadingBuiltIn ? `Starting ${builtInModelName}, the built-in assistant…` : "Loading the model…"}
        </p>
      )}
      {snapshot && !modelReady && phase !== "loading" && !modelAction && (bundled === "checking" && !snapshot.models.length ? (
        <p className="hint inline-status" role="status"><Spinner size={13} />Checking the built-in assistant…</p>
      ) : bundled === "corrupted" ? (
        <Callout tone="error" role="alert" title="The built-in assistant couldn't be verified"
          actions={<button type="button" className="primary-button small" onClick={() => openModal("models")}>
            {snapshot.models.length ? "Choose a model" : "Import a model"}
          </button>}>
          Its model file is damaged or was changed, so it wasn't loaded. Reinstall the app to restore it
          {snapshot.models.length ? ", or choose one of your models." : ", or import a GGUF model from this computer."}
        </Callout>
      ) : (
        <Callout tone="accent" title={snapshot.models.length ? "Choose a model to begin" : "Import a model to begin"}
          actions={<button type="button" className="primary-button small" onClick={() => openModal("models")}>
            {snapshot.models.length ? "Choose a model" : "Import a model"}
          </button>}>
          {snapshot.models.length
            ? `Your models are listed in Models. ${builtInModelName} handles everyday Chat; ${advancedModelName} adds Agent tasks and Thinking.`
            : `This copy of the app doesn't include the built-in assistant. Select a GGUF model file from this computer, such as ${advancedModelName}.`}
        </Callout>
      ))}
      {snapshot && modelReady && mode === "agent" && !supportsAgent && (
        <Callout tone="accent" title={`Agent needs ${advancedModelName}`}
          actions={<button type="button" className="secondary-button small" onClick={() => openModal("models")}>Open Models</button>}>
          {capabilityGuidance(modelInfo)}
        </Callout>
      )}
      {snapshot && modelReady && (mode === "chat" || supportsAgent) && (
        <ul className="suggestions" aria-label="Suggestions">
          {(mode === "agent" ? agentSuggestions : chatSuggestions).map((text) => (
            <li key={text}>
              <button type="button" className="suggestion" onClick={() => { setDraft(text); focusComposer(); }}>
                {mode === "agent" ? <AgentIcon size={14} /> : <ChatIcon size={14} />}<span>{text}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {snapshot && modelReady && mode === "chat" && chatGuidance && (
        <p className="hint capability-note">
          {chatGuidance}{" "}
          <button type="button" className="link-button" onClick={() => openModal("models")}>Models</button>
        </p>
      )}
    </section>
  );

  const overlay = (narrow && !sidebarCollapsed) || drawerOpen;

  return (
    <div className={`app${sidebarCollapsed ? " app-sidebar-collapsed" : ""}${drawerOpen ? " app-drawer-open" : ""}${narrow ? " app-narrow" : ""}`}>
      <Sidebar collapsed={sidebarCollapsed} onToggle={() => setSidebarCollapsed((value) => !value)}
        onNewThread={newThread} newDisabled={generating} searching={searching} onSearching={setSearching}
        query={query} onQuery={setQuery} searchError={searchError} items={recent} records={records} currentKey={currentKey}
        navigationDisabled={generating || operation === "opening"} onOpen={(item) => void openConversation(item.id)}
        onOpenRecord={openRecord}
        onModels={() => openModal("models")} onSettings={() => openModal("settings")} loading={!snapshot} />

      {overlay && (
        <button type="button" className={`scrim${drawerOpen && !(narrow && !sidebarCollapsed) ? " scrim-drawer" : ""}`}
          aria-label="Close panel" tabIndex={-1}
          onClick={() => { if (narrow) setSidebarCollapsed(true); setDrawerOpen(false); }} />
      )}

      <main className="workspace" aria-label={title}>
        <header className="topbar">
          <h1 className="topbar-title" title={title}>{title}</h1>
          <span className="spacer" />
          <button type="button" className="status-pill" onClick={() => openModal("models")}
            title="Models" aria-label={`Model status: ${statusText}. Open models.`}>
            <span className={`status-dot status-${modelAction ? "loading" : phase}`} aria-hidden="true" />
            <span className="status-pill-label">Local</span>
            <span className="status-pill-model">{statusText}</span>
          </button>
          {view.kind === "conversation" && conversation && (
            <Menu label="Conversation options" align="end" trigger={<MoreIcon size={18} />} className="topbar-menu" disabled={busy}>
              {(close) => (
                <>
                  <MenuItem icon={<EditIcon size={15} />} onSelect={() => { close(); openModal("rename"); }}>Rename</MenuItem>
                  <MenuItem icon={<NoteIcon size={15} />} onSelect={() => { close(); openModal("instructions"); }}>Instructions</MenuItem>
                  <MenuItem danger icon={<TrashIcon size={15} />} onSelect={() => { close(); openModal("delete"); }}>Delete</MenuItem>
                </>
              )}
            </Menu>
          )}
          <IconButton label={drawerOpen ? "Hide details" : "Show details"} aria-pressed={drawerOpen}
            onClick={() => setDrawerOpen((value) => !value)}><PanelRightIcon size={18} /></IconButton>
        </header>

        <div className="thread-scroll" ref={scroll} onScroll={onScroll} tabIndex={-1}>
          <div className="thread" aria-live={view.kind === "empty" ? undefined : "polite"} aria-busy={generating || agent.running}>
            {operation === "opening" ? <p className="empty-line" role="status">Opening conversation…</p>
              : view.kind === "empty" ? emptyState
                : view.kind === "record" ? (
                  <LegacyRecord summary={agent.tasks.find((task) => task.id === view.taskId) ?? null}
                    detail={agent.details[view.taskId] ?? null} onDetails={() => setDrawerOpen(true)} />
                ) : conversation && conversation.turns.length === 0 && !pendingAgent ? emptyState
                  : conversation && (
                    <ConversationView conversation={conversation} loadedAt={loadedAt} agent={agent}
                      busy={busy || !modelReady} generating={generating} contextLength={contextLength}
                      pendingAgentPrompt={pendingAgent?.conversationId === conversation.id ? pendingAgent.prompt : null}
                      chatActions={{
                        onRegenerate: (regenerate) => void generate("regenerate", regenerate),
                        onContinue: (messageId) => void generate("continue", {}, messageId),
                        onOpenLimits: () => openModal("models"),
                      }}
                      continuation={continuationView}
                      onDetails={(turnId) => { setDrawerTurnId(turnId); setDrawerOpen(true); }} />
                  )}
          </div>
        </div>

        <div className="composer-dock">
          {returnToAgent && agent.active && (
            <Callout tone={agent.activeApproval ? "warning" : "neutral"}
              title={agent.activeApproval ? "An Agent turn is waiting for your approval" : "An Agent turn is running"}
              actions={<>
                <button type="button" className="link-button" disabled={operation !== null}
                  onClick={() => { follow.current = true; void openConversation(agent.active!.conversationId); }}>
                  Return to “{shorten(activeTitle ?? "conversation", 32)}”
                </button>
                <button type="button" className="link-button danger-text" disabled={agent.cancelling}
                  onClick={() => void agent.cancel()}>{agent.cancelling ? "Cancelling…" : "Cancel turn"}</button>
              </>} />
          )}
          {showJump && (
            <button type="button" className="jump-button" onClick={jumpToLatest}>
              <ArrowDownIcon size={14} />Jump to latest
            </button>
          )}
          {error && !editing && (
            <Callout tone="error" role="alert" title="Something went wrong"
              actions={<button type="button" className="link-button" onClick={() => setError(null)}>Dismiss</button>}>
              {error}
            </Callout>
          )}
          {mode === "agent" && agent.error && !agent.activeApproval && (
            <Callout tone="error" role="alert" title="Agent"
              actions={<button type="button" className="link-button" onClick={() => agent.setError(null)}>Dismiss</button>}>
              {agent.error}
            </Callout>
          )}
          {showLimitBanner && activeReference && (
            <Callout tone="warning" title={`Replies are limited to ${activeReference.settings.maxTokens} tokens`}
              actions={<>
                <button type="button" className="secondary-button small" disabled={busy}
                  onClick={() => openModal("models")}>Review recommended settings</button>
                <button type="button" className="link-button" onClick={() => setDismissedLimit(activeReference.id)}>
                  Keep {activeReference.settings.maxTokens}
                </button>
              </>}>
              This model's saved output limit is low, so longer answers, and answers with Thinking, may be cut off.
              Models shows the recommended settings, which you can review before applying.
            </Callout>
          )}
          <Composer inputRef={composer} mode={mode} onModeChange={setMode}
            agentAvailable={agentAvailable} agentUnavailable={agentUnavailable}
            draft={draft} onDraftChange={setDraft} onSubmit={submit}
            onStop={() => (agent.running ? void agent.cancel() : void stopChat())}
            canSend={canSend} inputDisabled={!snapshot}
            showStop={generating || agent.running}
            stopLabel={agent.running ? "Cancel Agent turn" : "Stop generating"}
            canStop={agent.running ? !agent.cancelling
              : generating && conversation?.turns.at(-1)?.assistants.at(-1)?.message.state === "streaming" && !stopping}
            stopping={agent.running ? agent.cancelling : stopping}
            enterToSend={snapshot?.preferences.enterToSend ?? true}
            models={snapshot?.models ?? []} activeModelId={modelReady || phase === "generating" ? snapshot?.activeModelId ?? null : null}
            modelBusy={modelAction === "load" || modelAction === "switch" ? "Loading…" : modelAction === "unload" ? "Unloading…" : null}
            modelLocked={chatBusy || agentBusy}
            onSelectModel={selectModelFromComposer} onManageModels={() => openModal("models")}
            thinkingSupported={supportsThinking} thinking={thinking} onThinkingChange={setThinking}
            savedMaxTokens={activeReference?.settings.maxTokens ?? null}
            outputOverride={outputOverride} onOutputOverride={setOutputOverride} contextLength={contextLength}
            onOpenModelSettings={() => openModal("models")}
            timeout={taskTimeout} onTimeoutChange={setTaskTimeout}
            scopes={agent.scopes} scopeRemaining={agent.scopeRemaining} scopeAction={agent.scopeAction}
            scopeError={agent.scopeError} onGrant={(kind) => void agent.grant(kind)}
            onRevoke={(scope) => void revokeScope(scope)} onManageAccess={() => openModal("access")}
            grantDisabled={busy} lockMenus={!!agent.activeApproval} placeholder={placeholder} status={composerStatus} />
        </div>
      </main>

      <DetailsDrawer open={drawerOpen} onClose={() => setDrawerOpen(false)}
        view={view.kind} conversation={view.kind === "conversation" ? conversation : null} model={modelInfo}
        onEditConversation={(kind) => openModal(kind)} agentTask={drawerTask}
        scopes={agent.scopes} scopeRemaining={agent.scopeRemaining} onManageAccess={() => openModal("access")} />

      {snapshot && modal === "settings" && (
        <SettingsDialog snapshot={snapshot} onSave={(preferences) => setSnapshot({ ...snapshot, preferences })}
          onClose={() => setModal(null)} onManageModels={() => openModal("models")} />
      )}
      {snapshot && modal === "models" && (
        <ModelsDialog snapshot={snapshot} selectedId={selectedModelId} onSelect={setSelectedModelId}
          action={modelAction} operationLocked={chatBusy || agentBusy} error={modelError} notice={modelNotice}
          onImport={() => void importModel()} onLoad={(reference, settings) => void loadModel(reference, settings)}
          onUnload={() => void unloadModel()} onSaveSettings={(reference, settings) => void saveModelSettings(reference, settings)}
          recommended={recommended} onReload={(reference, settings) => void reloadWithSettings(reference, settings)}
          bundled={bundled}
          onClose={() => setModal(null)} />
      )}
      {modal === "access" && (
        <AccessDialog scopes={agent.scopes} remaining={agent.scopeRemaining} action={agent.scopeAction}
          error={agent.scopeError} disabled={busy} onGrant={(kind) => void agent.grant(kind)}
          onRevoke={(scope) => void revokeScope(scope)} onClose={() => setModal(null)} />
      )}
      {conversation && editing && (
        <EditConversationDialog kind={modal} conversation={conversation} error={editError} busy={chatBusy}
          onSubmit={(value) => void editConversation(value)} onClose={() => { setEditError(null); setModal(null); }} />
      )}
    </div>
  );
}
