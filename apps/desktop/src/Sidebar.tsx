import { useEffect, useId, useRef, useState } from "react";
import { stateLabel } from "./agent";
import type { TaskState } from "./agentTypes";
import {
  AgentIcon, BookIcon, ChatIcon, ChevronRightIcon, ComposeIcon, CubeIcon, GearIcon, MemoryIcon, PanelLeftIcon, QuoteIcon, SearchIcon, Spinner,
} from "./icons";
import { IconButton } from "./ui";

/** One saved conversation (Chat and Agent turns together). */
export type RecentItem = { key: string; id: number; title: string; updatedAt: number; running: boolean };

/** A read-only Agent journal entry, kept apart from conversations. */
export type RecordItem = { key: string; id: string; title: string; updatedAt: number; state: TaskState };

export default function Sidebar({
  collapsed, onToggle, onNewThread, newDisabled, searching, onSearching, query, onQuery, searchError,
  items, records, currentKey, navigationDisabled, onOpen, onOpenRecord, onModels, onSettings, loading,
  onKnowledge, onMemory, onGlance,
}: {
  onKnowledge: () => void;
  onMemory: () => void;
  onGlance: () => void;
  collapsed: boolean;
  onToggle: () => void;
  onNewThread: () => void;
  newDisabled: boolean;
  searching: boolean;
  onSearching: (value: boolean) => void;
  query: string;
  onQuery: (value: string) => void;
  searchError: string | null;
  items: RecentItem[];
  currentKey: string | null;
  navigationDisabled: boolean;
  onOpen: (item: RecentItem) => void;
  records: RecordItem[];
  onOpenRecord: (item: RecordItem) => void;
  onModels: () => void;
  onSettings: () => void;
  loading: boolean;
}) {
  const search = useRef<HTMLInputElement>(null);
  const [recordsOpen, setRecordsOpen] = useState(false);
  const recordsId = useId();

  useEffect(() => {
    if (searching && !collapsed) search.current?.focus();
  }, [searching, collapsed]);

  if (collapsed) {
    return (
      <nav className="sidebar sidebar-collapsed" aria-label="Main">
        <IconButton label="Show sidebar" onClick={onToggle}><PanelLeftIcon size={18} /></IconButton>
        <IconButton label="New thread" onClick={onNewThread} disabled={newDisabled}><ComposeIcon size={18} /></IconButton>
        <IconButton label="Search" onClick={() => { onToggle(); onSearching(true); }}><SearchIcon size={18} /></IconButton>
        <IconButton label="Knowledge" onClick={onKnowledge} disabled={navigationDisabled}
          aria-current={currentKey === "knowledge" ? "page" : undefined}><BookIcon size={18} /></IconButton>
        <IconButton label="Memory" onClick={onMemory} disabled={navigationDisabled}
          aria-current={currentKey === "memory" ? "page" : undefined}><MemoryIcon size={18} /></IconButton>
        <IconButton label="Contextual panel" onClick={onGlance}><QuoteIcon size={18} /></IconButton>
        <IconButton label="Models" onClick={onModels}><CubeIcon size={18} /></IconButton>
        <span className="spacer" />
        <IconButton label="Settings" onClick={onSettings}><GearIcon size={18} /></IconButton>
      </nav>
    );
  }

  return (
    <nav className="sidebar" aria-label="Main">
      <div className="sidebar-top">
        <IconButton label="Hide sidebar" onClick={onToggle}><PanelLeftIcon size={18} /></IconButton>
      </div>
      <div className="sidebar-actions">
        <button type="button" className="nav-item" onClick={onNewThread} disabled={newDisabled}>
          <ComposeIcon size={16} /><span>New thread</span>
        </button>
        {searching ? (
          <div className="nav-search">
            <SearchIcon size={15} />
            <label className="sr-only" htmlFor="sidebar-search">Search conversations</label>
            <input id="sidebar-search" ref={search} type="search" placeholder="Search" maxLength={200} value={query}
              onChange={(event) => onQuery(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") { onQuery(""); onSearching(false); }
              }}
              onBlur={() => { if (!query) onSearching(false); }} />
          </div>
        ) : (
          <button type="button" className="nav-item" onClick={() => onSearching(true)}>
            <SearchIcon size={16} /><span>Search</span>
          </button>
        )}
        <button type="button" className="nav-item" onClick={onKnowledge} disabled={navigationDisabled}
          aria-current={currentKey === "knowledge" ? "page" : undefined}>
          <BookIcon size={16} /><span>Knowledge</span>
        </button>
        <button type="button" className="nav-item" onClick={onMemory} disabled={navigationDisabled}
          aria-current={currentKey === "memory" ? "page" : undefined}>
          <MemoryIcon size={16} /><span>Memory</span>
        </button>
        <button type="button" className="nav-item" onClick={onModels}>
          <CubeIcon size={16} /><span>Models</span>
        </button>
      </div>

      <div className="sidebar-section">
        <h2 className="sidebar-label">{query.trim() ? "Results" : "Recent"}</h2>
        {searchError && <p className="error-text sidebar-note" role="alert">{searchError}</p>}
        {!items.length && !searchError && (
          <p className="sidebar-note">
            {loading ? "Loading…" : query.trim() ? "No saved conversation text matches your search." : "Your conversations appear here."}
          </p>
        )}
        <ul className="recent-list">
          {items.map((item) => (
            <li key={item.key}>
              <button type="button" className="recent-item" aria-current={item.key === currentKey ? "page" : undefined}
                disabled={navigationDisabled && item.key !== currentKey} onClick={() => onOpen(item)} title={item.title}>
                {item.running ? <Spinner size={13} /> : <ChatIcon size={14} />}
                <span className="recent-title">{item.title}</span>
                {item.running && <span className="recent-state">Agent running</span>}
              </button>
            </li>
          ))}
        </ul>
        {records.length > 0 && !query.trim() && (
          <div className="records">
            <button type="button" className="records-toggle" aria-expanded={recordsOpen} aria-controls={recordsId}
              onClick={() => setRecordsOpen((value) => !value)}>
              <ChevronRightIcon size={13} className={recordsOpen ? "chevron-open" : undefined} />
              <span>Execution records</span>
              <span className="recent-state">{records.length}</span>
            </button>
            {recordsOpen && (
              <div id={recordsId}>
                <p className="sidebar-note">Journal entries, including older standalone tasks. Agent turns also appear inside their conversations.</p>
                <ul className="recent-list">
                  {records.map((record) => (
                    <li key={record.key}>
                      <button type="button" className="recent-item recent-record" aria-current={record.key === currentKey ? "page" : undefined}
                        disabled={navigationDisabled} onClick={() => onOpenRecord(record)} title={record.title}>
                        <AgentIcon size={14} />
                        <span className="recent-title">{record.title}</span>
                        {record.state !== "completed" && <span className="recent-state">{stateLabel(record.state)}</span>}
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}
      </div>

      <div className="sidebar-bottom">
        <button type="button" className="nav-item" onClick={onGlance}>
          <QuoteIcon size={16} /><span>Contextual panel</span>
        </button>
        <button type="button" className="nav-item" onClick={onSettings}>
          <GearIcon size={16} /><span>Settings</span>
        </button>
      </div>
    </nav>
  );
}
