import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { commandError } from "./chat";
import { CloseIcon, EditIcon, PlusIcon, SearchIcon, Spinner, TrashIcon } from "./icons";
import {
  createLatest, knowledgeErrorCopy, memoryInputError, memoryMaxBytes, memoryMaxCount, memoryPageSize, memoryQueryValid,
  mergePage, utf8Bytes,
  type MemoryPage, type PersonalMemory,
} from "./knowledge";
import { Callout, Dialog, IconButton, Switch } from "./ui";

const dateLabel = (at: number) => new Date(at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });

function ByteCount({ text }: { text: string }) {
  const bytes = utf8Bytes(text.trim());
  return (
    <span className={`byte-count${bytes > memoryMaxBytes ? " danger-text" : ""}`} aria-live="polite">
      {bytes.toLocaleString()} / {memoryMaxBytes.toLocaleString()} bytes
    </span>
  );
}

function MemoryItem({
  memory, onSaved, onDelete,
}: {
  memory: PersonalMemory;
  onSaved: (memory: PersonalMemory) => void;
  onDelete: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(memory.content);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const invalid = memoryInputError(value);

  async function save() {
    if (invalid) return;
    setSaving(true);
    setError(null);
    try {
      const saved = await invoke<PersonalMemory>("memory_edit", { id: memory.id, content: value.trim() });
      onSaved(saved);
      setEditing(false);
    } catch (failure) {
      const copy = knowledgeErrorCopy(commandError(failure));
      setError(`${copy.title}. ${copy.detail}`);
    } finally {
      setSaving(false);
    }
  }

  if (editing) {
    return (
      <li className="memory-item memory-item-editing">
        <label className="field">
          <span className="sr-only">Edit memory</span>
          <textarea rows={3} value={value} autoFocus onChange={(event) => setValue(event.currentTarget.value)} />
        </label>
        <div className="memory-edit-actions">
          <ByteCount text={value} />
          <span className="spacer" />
          <button type="button" className="ghost-button small" disabled={saving}
            onClick={() => { setEditing(false); setValue(memory.content); setError(null); }}>Cancel</button>
          <button type="button" className="primary-button small" disabled={saving || !!invalid || value.trim() === memory.content}
            onClick={() => void save()}>{saving ? <Spinner size={12} /> : null}Save</button>
        </div>
        {error && <p className="error-text" role="alert">{error}</p>}
      </li>
    );
  }

  return (
    <li className="memory-item">
      {/* Saved memories are shown verbatim, never as Markdown. */}
      <p className="memory-content">{memory.content}</p>
      <div className="memory-meta">
        <span className="hint">{memory.updatedAt !== memory.createdAt ? `Edited ${dateLabel(memory.updatedAt)}` : `Saved ${dateLabel(memory.createdAt)}`}</span>
        <span className="spacer" />
        <IconButton label="Edit memory" className="small" onClick={() => { setValue(memory.content); setEditing(true); }}>
          <EditIcon size={14} />
        </IconButton>
        <IconButton label="Delete memory" className="small" onClick={onDelete}><TrashIcon size={14} /></IconButton>
      </div>
    </li>
  );
}

export default function MemoryView({
  enabled, onEnabledChange,
}: {
  /** Persisted retrieval setting, or null while it loads. */
  enabled: boolean | null;
  onEnabledChange: (value: boolean) => Promise<void>;
}) {
  const [memories, setMemories] = useState<PersonalMemory[]>([]);
  const [total, setTotal] = useState<number | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [toggling, setToggling] = useState(false);
  const [error, setError] = useState<{ title: string; detail: string } | null>(null);
  const [confirm, setConfirm] = useState<{ kind: "one"; memory: PersonalMemory } | { kind: "all" } | null>(null);
  const [removing, setRemoving] = useState(false);
  const gate = useRef(createLatest()).current;
  const trimmedQuery = query.trim();
  const queryValid = memoryQueryValid(query);
  const draftError = draft.trim() ? memoryInputError(draft) : null;

  const fail = (failure: unknown) => setError(knowledgeErrorCopy(commandError(failure)));

  const load = useCallback(async (beforeId?: number) => {
    if (!memoryQueryValid(trimmedQuery)) return;
    const token = gate.next();
    setLoading(true);
    try {
      const page = await invoke<MemoryPage>("memory_page", {
        ...(trimmedQuery ? { query: trimmedQuery } : {}),
        ...(beforeId === undefined ? {} : { beforeId }),
        limit: memoryPageSize,
      });
      if (!gate.isCurrent(token)) return;
      setMemories((current) => (beforeId === undefined ? page.items : mergePage(current, page.items)));
      setTotal(page.total);
      setHasMore(page.hasMore);
    } catch (failure) {
      if (gate.isCurrent(token)) fail(failure);
    } finally {
      if (gate.isCurrent(token)) setLoading(false);
    }
  }, [trimmedQuery, gate]);

  const liveLoad = useRef(load);
  liveLoad.current = load;

  useEffect(() => {
    gate.invalidate();
    const timer = window.setTimeout(() => void load(), trimmedQuery ? 200 : 0);
    return () => { gate.invalidate(); window.clearTimeout(timer); };
  }, [load, trimmedQuery, gate]);

  async function create() {
    if (saving || total === memoryMaxCount || memoryInputError(draft)) return;
    setSaving(true);
    setError(null);
    try {
      await invoke<PersonalMemory>("memory_create", { content: draft.trim() });
      setDraft((current) => current === draft ? "" : current);
      await liveLoad.current();
    } catch (failure) {
      fail(failure);
    } finally {
      setSaving(false);
    }
  }

  async function toggle(value: boolean) {
    setToggling(true);
    setError(null);
    try {
      await onEnabledChange(value);
    } catch (failure) {
      fail(failure);
    } finally {
      setToggling(false);
    }
  }

  async function remove() {
    if (!confirm) return;
    setRemoving(true);
    setError(null);
    try {
      if (confirm.kind === "one") {
        await invoke("memory_delete", { id: confirm.memory.id });
        void liveLoad.current();
      } else {
        await invoke<number>("memory_clear");
        setMemories([]);
        gate.invalidate();
        setHasMore(false);
        setTotal(0);
        setLoading(false);
      }
      setConfirm(null);
    } catch (failure) {
      fail(failure);
      setConfirm(null);
      if (commandError(failure).code === "not_found") void liveLoad.current();
    } finally {
      setRemoving(false);
    }
  }

  return (
    <div className="library-view">
      <header className="library-header">
        <div>
          <h2>Memory</h2>
          <p>Short notes about you that Chat can use as background. You decide what's saved.</p>
        </div>
      </header>

      <section className="embedding-panel" aria-label="Memory retrieval">
        <Switch label="Use memories in Chat" checked={!!enabled} disabled={enabled === null || toggling}
          description="When on, up to three memories that share words with your message are added to new Chat replies and regenerations in every conversation. Turning it off keeps your memories."
          onChange={(value) => void toggle(value)} />
        <ul className="privacy-list">
          <li>Memories are stored on this device in the app's database, which isn't encrypted.</li>
          <li>Nothing is remembered automatically. Memories are added only here.</li>
          <li>Editing or deleting a memory doesn't change earlier answers that already mention it.</li>
        </ul>
      </section>

      {error && (
        <Callout tone="error" role="alert" title={error.title}
          actions={<button type="button" className="link-button" onClick={() => setError(null)}>Dismiss</button>}>{error.detail}</Callout>
      )}

      <form className="memory-new" onSubmit={(event) => { event.preventDefault(); void create(); }}>
        <label className="field" htmlFor="memory-new">
          <span>Add a memory</span>
          <textarea id="memory-new" rows={2} value={draft} placeholder="For example: I prefer metric units."
            onChange={(event) => setDraft(event.currentTarget.value)} />
        </label>
        <div className="memory-edit-actions">
          <ByteCount text={draft} />
          {draftError && <span className="error-text">{draftError}</span>}
          <span className="spacer" />
          <button type="submit" className="primary-button small" disabled={saving || total === memoryMaxCount || !draft.trim() || !!draftError}>
            {saving ? <Spinner size={12} /> : <PlusIcon size={14} />}Save memory
          </button>
        </div>
      </form>

      <section className="library-documents" aria-label="Saved memories">
        <div className="memory-toolbar">
          <div className="search-field">
            <SearchIcon size={15} />
            <label className="sr-only" htmlFor="memory-search">Search memories</label>
            <input id="memory-search" type="search" value={query} placeholder="Search memories…" aria-invalid={!queryValid}
              onChange={(event) => setQuery(event.currentTarget.value)} />
            {query && <button type="button" className="chip-remove" aria-label="Clear search" onClick={() => setQuery("")}><CloseIcon size={12} /></button>}
          </div>
          <button type="button" className="ghost-button small danger-text" disabled={removing || !total}
            onClick={() => setConfirm({ kind: "all" })}>Clear all…</button>
        </div>
        {!queryValid && <p className="error-text">Searches are limited to 256 bytes.</p>}
        {loading && memories.length === 0 && <p className="hint inline-status" role="status"><Spinner size={12} />Loading…</p>}
        {!loading && memories.length === 0 && (
          <p className="empty-line">{trimmedQuery ? "No memories contain that text." : "No memories saved."}</p>
        )}
        {memories.length > 0 && (
          <ul className="memory-list">
            {memories.map((memory) => (
              <MemoryItem key={`${memory.id}-${memory.updatedAt}`} memory={memory}
                onSaved={() => void liveLoad.current()}
                onDelete={() => setConfirm({ kind: "one", memory })} />
            ))}
          </ul>
        )}
        {hasMore && (
          <button type="button" className="secondary-button small load-more" disabled={loading}
            onClick={() => void load(memories.at(-1)?.id)}>{loading ? <Spinner size={12} /> : null}Show more</button>
        )}
        <p className="hint library-footnote">{total === null ? `Up to ${memoryMaxCount} saved memories` : `${total} of ${memoryMaxCount} memories saved`} · {memoryMaxBytes.toLocaleString()} bytes each.</p>
      </section>

      {confirm && (
        <Dialog size="sm" busy={removing} onClose={() => setConfirm(null)}
          title={confirm.kind === "all" ? "Clear all memories?" : "Delete this memory?"}
          description={confirm.kind === "all"
            ? "All saved memories are removed from this app’s local database. The setting to use memories stays as it is."
            : "This memory is removed from the app’s local database."}>
          <div className="dialog-form">
            {confirm.kind === "one" && <p className="memory-content memory-quote">{confirm.memory.content}</p>}
            <p className="hint">Earlier answers that mention {confirm.kind === "all" ? "them" : "it"} are not changed.</p>
            <div className="dialog-actions">
              <button type="button" className="secondary-button" disabled={removing} onClick={() => setConfirm(null)}>Cancel</button>
              <button type="button" className="danger-button" disabled={removing} onClick={() => void remove()}>
                {removing ? <Spinner size={13} /> : <TrashIcon size={14} />}{confirm.kind === "all" ? "Clear all" : "Delete"}
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </div>
  );
}
