import { useId } from "react";
import { GlobeIcon } from "./icons";
import {
  maxQueryChars, normalizeQuery, providerLabel, type ResearchDraft, type ResearchPlan, type WebResearchView,
} from "./research";
import { Segmented } from "./ui";

/** Compact composer toggle. Search is enabled for one message at a time. */
export function SearchToggle({ draft, onChange, disabled }: {
  draft: ResearchDraft;
  onChange: (next: ResearchDraft) => void;
  disabled: boolean;
}) {
  return (
    <button type="button" className="composer-pill" aria-pressed={draft.enabled} disabled={disabled}
      title={draft.enabled ? "Web research is on for your next message" : "Search the web or read a page for your next message"}
      onClick={() => onChange({ ...draft, enabled: !draft.enabled })}>
      <GlobeIcon size={14} /><span>Search</span>
    </button>
  );
}

/**
 * What the next research request sends and where, shown before sending.
 * Nothing from the conversation, memory or documents is sent to search.
 */
export function ResearchBar({ draft, onChange, plan, view, remoteName, message, disabled, onOpenSettings }: {
  draft: ResearchDraft;
  onChange: (next: ResearchDraft) => void;
  plan: ResearchPlan;
  view: WebResearchView | null;
  remoteName: string | null;
  message: string;
  disabled: boolean;
  onOpenSettings: () => void;
}) {
  const queryId = useId();
  const urlId = useId();
  const provider = view?.settings.provider ?? null;
  const editing = draft.query !== null;
  return (
    <div className="research-bar" role="group" aria-label="Web research for the next message">
      <div className="research-row">
        <Segmented label="Research mode" value={draft.mode} disabled={disabled}
          options={[{ value: "search", label: "Search the web" }, { value: "page", label: "Read a page" }]}
          onChange={(mode) => onChange({ ...draft, mode })} />
        <button type="button" className="link-button" onClick={onOpenSettings}>Web search settings…</button>
      </div>
      {draft.mode === "search" ? (
        <div className="research-disclosure">
          <p>
            <span className="research-label">Sent to {providerLabel(provider)}</span>
            {!editing && (
              <span className="research-query" title="The exact search query">
                {plan.outgoing ? `“${plan.outgoing}”` : "Your message, when you type it"}
              </span>
            )}
            {!editing && (
              <button type="button" className="link-button" disabled={disabled}
                onClick={() => onChange({ ...draft, query: normalizeQuery(message) })}>Edit query</button>
            )}
          </p>
          {editing && (
            <div className="research-edit">
              <label className="sr-only" htmlFor={queryId}>Search query</label>
              <input id={queryId} type="text" maxLength={maxQueryChars} value={draft.query ?? ""} disabled={disabled}
                placeholder="Search query" onChange={(event) => onChange({ ...draft, query: event.currentTarget.value })} />
              <button type="button" className="link-button" disabled={disabled}
                onClick={() => onChange({ ...draft, query: null, queryAttachments: [] })}>Use my message</button>
            </div>
          )}
          <p className="hint">
            Only this query is sent. Your conversation, memories and documents are not. Up to 3 public pages are then read
            directly from their sites, without cookies or sign-in.
          </p>
          {editing && draft.queryAttachments.length > 0 && (
            <p className="hint">
              This query starts from text recognized in an attached image. Review it: only the query text is sent, never
              the image. This is a text search using visual clues, not reverse image matching.
            </p>
          )}
          {provider === "purili" && (
            <p className="hint">Experimental provider, for personal and non-commercial use only.</p>
          )}
        </div>
      ) : (
        <div className="research-disclosure">
          <label className="research-label" htmlFor={urlId}>Public webpage (https)</label>
          <input id={urlId} type="url" inputMode="url" maxLength={2048} value={draft.url} disabled={disabled}
            placeholder="https://example.org/article" onChange={(event) => onChange({ ...draft, url: event.currentTarget.value })} />
          <p className="hint">The page is read directly from its site, without cookies, scripts or images. No search provider is used.</p>
        </div>
      )}
      <div className="research-answer">
        {remoteName && !draft.resultsOnly && (
          <label className="check-row">
            <input type="checkbox" checked={draft.share} disabled={disabled}
              onChange={(event) => onChange({ ...draft, share: event.currentTarget.checked })} />
            <span>Send my question and selected page excerpts to {remoteName} to write the answer</span>
          </label>
        )}
        <label className="check-row">
          <input type="checkbox" checked={draft.resultsOnly} disabled={disabled}
            onChange={(event) => onChange({ ...draft, resultsOnly: event.currentTarget.checked })} />
          <span>Results only, without a written answer</span>
        </label>
        <p className="hint">{plan.answerNote}</p>
      </div>
      {plan.problem && <p className="error-text" role="status">{plan.problem}</p>}
    </div>
  );
}
