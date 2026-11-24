import { Channel, invoke } from "@tauri-apps/api/core";
import {
  useEffect, useLayoutEffect, useMemo, useRef, useState,
  type FormEvent, type KeyboardEvent,
} from "react";
import {
  applyChatEvent, earlierReplies, splitReasoning,
  type Bootstrap, type ChatEvent, type Conversation, type ConversationSummary, type Message,
} from "./chat";
import { Dialog, ModelDialog, SettingsDialog } from "./Dialogs";
import { errorMessage } from "./inference";
import MarkdownMessage, { CopyButton } from "./MarkdownMessage";
import AgentPanel from "./Agent";

type Modal = "model" | "settings" | "rename" | "delete" | "instructions";
type Operation = "opening" | "creating" | "saving" | "generating";
type WorkspaceMode = "chat" | "agent";

const messageStatus: Record<Message["state"], string> = {
  streaming: "Generating…",
  complete: "",
  cancelled: "Stopped",
  interrupted: "Interrupted",
  failed: "Generation failed",
  truncated: "Token limit reached",
};

function ChatMessage({ message, earlier }: { message: Message; earlier: boolean }) {
  const { answer, reasoning, reasoningComplete } = message.role === "assistant"
    ? splitReasoning(message.content, message.state === "streaming")
    : { answer: message.content, reasoning: null, reasoningComplete: false };
  const stats = message.stats;
  const name = message.role === "user" ? "You" : message.role === "system" ? "System" : "Assistant";

  return (
    <article className={`message message-${message.role}${earlier ? " earlier-message" : ""}`} aria-label={name}>
      <header className="message-heading">
        <div>
          <strong>{name}</strong>
          {earlier && <span className="message-label" role="note">Earlier reply</span>}
          {messageStatus[message.state] && <span className="message-label" role="status">{messageStatus[message.state]}</span>}
        </div>
        {message.content && <CopyButton label="Copy message" text={message.content} />}
      </header>
      {reasoning !== null && (
        <details className="reasoning">
          <summary>
            {reasoningComplete ? "Reasoning" : message.state === "streaming" ? "Thinking…" : "Incomplete reasoning"}
          </summary>
          <div className="reasoning-body">
            {reasoning ? <MarkdownMessage text={reasoning} /> : <p className="hint">Waiting for reasoning text.</p>}
          </div>
        </details>
      )}
      {answer && (message.role === "assistant"
        ? <MarkdownMessage text={answer} />
        : <p className="user-content">{answer}</p>)}
      {!answer && reasoning === null && message.role === "assistant" && message.state === "streaming" && (
        <p className="hint">Waiting for the first token…</p>
      )}
      {stats && (
        <div className="generation-stats">
          <span>{stats.generatedTokens} tokens</span>
          <span>{stats.tokensPerSecond.toFixed(1)} tokens/s</span>
          {stats.timeToFirstTokenMs !== null && <span>First token {stats.timeToFirstTokenMs} ms</span>}
          {stats.promptTokensPerSecond !== null && <span>Prompt {stats.promptTokensPerSecond.toFixed(1)} tokens/s</span>}
          {stats.excludedTurns > 0 && <span role="status">{stats.excludedTurns} earlier {stats.excludedTurns === 1 ? "turn" : "turns"} omitted from context</span>}
        </div>
      )}
    </article>
  );
}

function EditConversationDialog({
  modal, conversation, onSubmit, onClose, busy, error,
}: {
  modal: "rename" | "delete" | "instructions";
  conversation: Conversation;
  onSubmit: (value: string) => void;
  onClose: () => void;
  busy: boolean;
  error: string | null;
}) {
  const [value, setValue] = useState(modal === "rename" ? conversation.title : conversation.systemPrompt);
  const title = modal === "rename" ? "Rename conversation" : modal === "delete" ? "Delete conversation" : "Conversation instructions";

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!busy && (modal !== "rename" || value.trim())) onSubmit(value);
  }

  return (
    <Dialog title={title} onClose={onClose} busy={busy}>
      <form className="dialog-form" onSubmit={submit}>
        {modal === "rename" && (
          <label className="field">
            <span>Conversation title</span>
            <input value={value} maxLength={100} required disabled={busy}
              onChange={(event) => setValue(event.currentTarget.value)} />
          </label>
        )}
        {modal === "instructions" && (
          <label className="field">
            <span>System instructions</span>
            <textarea rows={5} maxLength={8192} value={value} disabled={busy}
              onChange={(event) => setValue(event.currentTarget.value)} />
            <span className="hint">Applies to future replies in this conversation.</span>
          </label>
        )}
        {modal === "delete" && (
          <p>Delete <strong>{conversation.title}</strong> and its saved messages?</p>
        )}
        {error && <p className="error" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button className={modal === "delete" ? "danger" : "primary"}
            type="submit" disabled={busy || (modal === "rename" && !value.trim())}>
            {modal === "rename" ? "Save title" : modal === "delete" ? "Confirm deletion" : "Save instructions"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

export default function App() {
  const [snapshot, setSnapshot] = useState<Bootstrap | null>(null);
  const [mode, setMode] = useState<WorkspaceMode>("chat");
  const [agentRunning, setAgentRunning] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [summaries, setSummaries] = useState<ConversationSummary[]>([]);
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [query, setQuery] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [draft, setDraft] = useState("");
  const [modal, setModal] = useState<Modal | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [operation, setOperation] = useState<Operation | null>(null);
  const operationRef = useRef<Operation | null>(null);
  const [selectedModelId, setSelectedModelId] = useState<number | null>(null);
  const [stopping, setStopping] = useState(false);
  const request = useRef(0);
  const scroll = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const composer = useRef<HTMLTextAreaElement>(null);
  const focusComposer = useRef(false);
  const selectedId = conversation?.id;
  const initialized = snapshot !== null;
  const appearance = snapshot?.preferences.appearance ?? "system";

  useEffect(() => {
    let active = true;
    const start = async () => {
      try {
        const [initial, list] = await Promise.all([
          invoke<Bootstrap>("bootstrap"),
          invoke<ConversationSummary[]>("list_conversations", { query: "" }),
        ]);
        const latest = list[0]
          ? await invoke<Conversation>("get_conversation", { id: list[0].id })
          : null;
        if (!active) return;
        setSnapshot(initial);
        setSelectedModelId(initial.activeModelId ?? initial.models[0]?.id ?? null);
        setSummaries(list);
        setConversation(latest);
      } catch (failure) {
        if (active) setError(errorMessage(failure));
      }
    };
    void start();
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
        .catch((failure: unknown) => {
          if (active) setSearchError(errorMessage(failure));
        });
    }, query ? 160 : 0);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [initialized, query, refresh]);

  useEffect(() => {
    document.documentElement.dataset.appearance = appearance;
  }, [appearance]);

  useEffect(() => {
    if (operation === null && focusComposer.current) {
      focusComposer.current = false;
      composer.current?.focus();
    }
  }, [operation]);

  useLayoutEffect(() => {
    follow.current = true;
    setShowJump(false);
  }, [selectedId]);

  useLayoutEffect(() => {
    const node = scroll.current;
    if (node && follow.current) node.scrollTop = node.scrollHeight;
  }, [conversation?.messages, selectedId]);

  function onScroll() {
    const node = scroll.current;
    if (!node) return;
    follow.current = node.scrollHeight - node.scrollTop - node.clientHeight < 64;
    setShowJump(!follow.current);
  }

  function jumpToLatest() {
    follow.current = true;
    setShowJump(false);
    if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight;
  }

  function begin(next: Operation) {
    if (operationRef.current !== null || agentRunning) return false;
    operationRef.current = next;
    setOperation(next);
    setError(null);
    return true;
  }

  function finish() {
    operationRef.current = null;
    setOperation(null);
  }

  function select(next: Conversation | null) {
    focusComposer.current = next !== null;
    setConversation(next);
    setDraft("");
    setSidebarOpen(false);
  }

  async function openConversation(id: number) {
    if (id === conversation?.id) {
      setSidebarOpen(false);
      return;
    }
    if (!begin("opening")) return;
    try {
      select(await invoke<Conversation>("get_conversation", { id }));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      finish();
    }
  }

  async function newConversation() {
    if (!snapshot || !begin("creating")) return;
    try {
      select(await invoke<Conversation>("create_conversation"));
      setQuery("");
      setRefresh((tick) => tick + 1);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      finish();
    }
  }

  async function editConversation(value: string) {
    if (!conversation || !begin("saving")) return;
    const id = conversation.id;

    try {
      if (modal === "delete") {
        await invoke("delete_conversation", { id });
        select(null);
      } else {
        if (modal === "rename") await invoke("rename_conversation", { id, title: value.trim() });
        if (modal === "instructions") await invoke("set_system_prompt", { id, systemPrompt: value });
        setConversation(await invoke<Conversation>("get_conversation", { id }));
      }
      setModal(null);
      setRefresh((tick) => tick + 1);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      finish();
    }
  }

  async function generate(kind: "send" | "regenerate") {
    if (!snapshot || snapshot.inference.phase !== "ready" || !conversation) return;
    const content = draft;
    if (kind === "send" && !content.trim()) return;
    if (!begin("generating")) return;
    const id = conversation.id;
    const generation = ++request.current;
    const events = new Channel<ChatEvent>((event) => {
      if (request.current !== generation) return;
      if (event.kind === "started" && event.user && kind === "send") setDraft("");
      setConversation((current) => current?.id === id
        ? { ...current, messages: applyChatEvent(current.messages, event) }
        : current);
    });
    let failureMessage: string | null = null;
    let sent = false;
    try {
      await invoke<Message>(kind === "send" ? "send_message" : "regenerate", kind === "send"
        ? { id, content, events }
        : { id, events });
      sent = kind === "send";
    } catch (failure) {
      failureMessage = errorMessage(failure);
    } finally {
      request.current++;
      // A rejected request may still have saved the user message and partial reply.
      try {
        const saved = await invoke<Conversation>("get_conversation", { id });
        setConversation(saved);
        if (kind === "send" && (sent || saved.messages.some((message) =>
          message.role === "user" && message.content === content
          && !conversation.messages.some((previous) => previous.id === message.id),
        ))) setDraft("");
      } catch (failure) {
        if (!failureMessage) failureMessage = errorMessage(failure);
      }
      if (failureMessage) setError(failureMessage);
      setRefresh((tick) => tick + 1);
      setStopping(false);
      focusComposer.current = true;
      finish();
    }
  }

  async function stop() {
    setStopping(true);
    try {
      if (!await invoke<boolean>("stop_generation")) setStopping(false);
    } catch (failure) {
      setError(errorMessage(failure));
      setStopping(false);
    }
  }

  function onComposerKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (!snapshot?.preferences.enterToSend || event.nativeEvent.isComposing
      || event.key !== "Enter" || event.shiftKey || event.ctrlKey || event.altKey || event.metaKey) return;
    event.preventDefault();
    void generate("send");
  }

  function openModal(next: Modal) {
    setError(null);
    setModal(next);
  }

  const busy = operation !== null || agentRunning;
  const generating = operation === "generating";
  const canStop = generating && conversation?.messages.at(-1)?.state === "streaming" && !stopping;
  const modelReady = snapshot?.inference.phase === "ready";
  const sendEnabled = !!conversation && !!modelReady && !busy && !!draft.trim();
  const canRegenerate = !!conversation?.messages.some((message) => message.role === "user")
    && !!modelReady && !busy;
  const earlier = useMemo(() => earlierReplies(conversation?.messages ?? []), [conversation?.messages]);
  const editing = modal === "rename" || modal === "delete" || modal === "instructions";

  return (
    <div className="workspace">
      <header className="workspace-modebar">
        <div className="workspace-brand"><strong>Local First AI</strong><span>Offline desktop</span></div>
        <nav className="mode-switch" aria-label="Workspace mode">
          <button type="button" aria-pressed={mode === "chat"} className={mode === "chat" ? "mode-active" : ""}
            onClick={() => setMode("chat")}>Chat</button>
          <button type="button" aria-pressed={mode === "agent"} className={mode === "agent" ? "mode-active" : ""}
            onClick={() => setMode("agent")}>Agent</button>
        </nav>
        <span className="workspace-local">Local processing · no cloud requests</span>
      </header>
      {sidebarOpen && <button type="button" className="mobile-dismiss" aria-label="Close conversations"
        onClick={() => setSidebarOpen(false)} />}
      <aside className={`sidebar${sidebarOpen ? " sidebar-open" : ""}`} aria-label="Conversations">
        <header className="sidebar-heading">
          <h1>Conversations</h1>
          <button className="quiet mobile-only" type="button" aria-label="Hide conversations"
            onClick={() => setSidebarOpen(false)}>Close</button>
        </header>
        <button className="primary new-conversation" type="button" onClick={() => void newConversation()}
          disabled={!snapshot || busy}>New conversation</button>
        <label className="field search-field">
          <span className="sr-only">Search conversations</span>
          <input type="search" placeholder="Search conversations" maxLength={200} value={query}
            onChange={(event) => setQuery(event.currentTarget.value)} />
        </label>
        <nav className="conversation-list" aria-label="Saved conversations">
          {searchError && <p role="alert" className="error">{searchError}</p>}
          {!summaries.length && !searchError && (
            <p className="hint">{query.trim() ? "No matching conversations." : "No conversations yet."}</p>
          )}
          {summaries.map(({ id, title }) => (
            <button type="button" key={id} className="conversation-link"
              aria-current={id === conversation?.id ? "page" : undefined} disabled={busy}
              onClick={() => void openConversation(id)} title={title}>{title}</button>
          ))}
        </nav>
        <footer className="sidebar-footer">
          <button type="button" className="quiet" disabled={!snapshot || busy}
            onClick={() => openModal("settings")}>Settings</button>
        </footer>
      </aside>

      <main className={"chat" + (mode !== "chat" ? " view-hidden" : "")} aria-label="Chat">
        <header className="chat-header">
          <div className="chat-title">
            <button type="button" className="quiet mobile-only" aria-label="Show conversations"
              aria-expanded={sidebarOpen} onClick={() => setSidebarOpen(true)}>Conversations</button>
            <h2>{conversation?.title ?? "Chat"}</h2>
            <span className="local-indicator"><span aria-hidden="true" />Local inference</span>
          </div>
          <div className="header-actions">
            <button type="button" disabled={!snapshot || busy} onClick={() => openModal("model")}>
              Model controls
            </button>
            {conversation && (
              <>
                <button type="button" className="quiet" disabled={busy}
                  onClick={() => openModal("instructions")}>Conversation instructions</button>
                <button type="button" className="quiet" disabled={busy}
                  onClick={() => openModal("rename")}>Rename conversation</button>
                <button type="button" className="quiet" disabled={busy}
                  onClick={() => openModal("delete")}>Delete conversation</button>
              </>
            )}
          </div>
        </header>

        <div className="model-line" role="status">
          {generating ? "Generating…" : snapshot?.inference.model
            ? `Model ready · ${snapshot.inference.model.name}`
            : !snapshot && !error ? "Starting…" : "No model loaded"}
        </div>

        <div className="thread-scroll" ref={scroll} onScroll={onScroll} tabIndex={0} aria-label="Conversation messages">
          <div className="thread">
            {operation === "opening" ? <p className="empty-state" role="status">Opening conversation…</p>
              : !snapshot && !error ? <p className="empty-state" role="status">Loading local conversations…</p>
                : !conversation ? (
                  <section className="empty-state">
                    <h3>Start a conversation</h3>
                    <p>Choose a conversation in the sidebar or create a new one.</p>
                  </section>
                ) : !conversation.messages.length ? (
                  <section className="empty-state">
                    <h3>Ready to chat</h3>
                    <p>{modelReady ? "Send a message to begin." : "Load a local GGUF model, then send a message."}</p>
                  </section>
                ) : conversation.messages.map((message) => (
                  <ChatMessage key={message.id} message={message} earlier={earlier.has(message.id)} />
                ))}
          </div>
        </div>

        <div className="composer-wrap">
          {showJump && <button type="button" className="jump quiet" onClick={jumpToLatest}>Jump to latest message</button>}
          {error && !editing && <p className="error" role="alert">{error}</p>}
          <form className="composer" onSubmit={(event) => { event.preventDefault(); void generate("send"); }}>
            <label className="sr-only" htmlFor="composer">Message</label>
            <textarea id="composer" ref={composer} rows={3} maxLength={65536} value={draft}
              disabled={!conversation || busy}
              placeholder={!conversation ? "Start a conversation to write a message" : "Write a message"}
              onChange={(event) => setDraft(event.currentTarget.value)} onKeyDown={onComposerKey} />
            <div className="composer-actions">
              <p className="hint">{snapshot?.preferences.enterToSend ? "Enter to send · Shift+Enter for a new line" : "Enter adds a new line"}</p>
              <div>
                <button type="button" className="quiet" disabled={!canRegenerate}
                  onClick={() => void generate("regenerate")}>Regenerate</button>
                {generating ? (
                  <button type="button" className="primary" disabled={!canStop}
                    onClick={() => void stop()}>{stopping ? "Stopping…" : "Stop"}</button>
                ) : <button type="submit" className="primary" disabled={!sendEnabled}>Send</button>}
              </div>
            </div>
          </form>
        </div>
      </main>

      <AgentPanel hidden={mode !== "agent"} modelReady={!!modelReady}
        supportsAgent={!!snapshot?.inference.model?.supportsThinking}
        modelName={snapshot?.inference.model?.name ?? null} blocked={operation !== null}
        onOpenModel={() => openModal("model")} onRunningChange={setAgentRunning} />

      {snapshot && modal === "settings" && (
        <SettingsDialog snapshot={snapshot} onSave={(preferences) => setSnapshot({ ...snapshot, preferences })}
          onClose={() => setModal(null)} />
      )}
      {snapshot && modal === "model" && (
        <ModelDialog snapshot={snapshot} selectedModelId={selectedModelId} onSelect={setSelectedModelId}
          onModelChange={setSnapshot} onClose={() => setModal(null)} />
      )}
      {conversation && editing && (
        <EditConversationDialog modal={modal} conversation={conversation} error={error} busy={busy}
          onSubmit={(value) => void editConversation(value)} onClose={() => { setError(null); setModal(null); }} />
      )}
    </div>
  );
}
