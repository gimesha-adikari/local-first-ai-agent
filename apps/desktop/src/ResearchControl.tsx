import { useId } from "react";
import { ChevronDownIcon, GlobeIcon } from "./icons";
import {
  maxQueryChars, normalizeQuery, type ResearchDraft, type ResearchPlan, type WebResearchView,
} from "./research";
import { Menu, MenuItem, MenuSeparator } from "./ui";

const researchChoices = [
  { label: "Search and answer", shortLabel: "Search", mode: "search", resultsOnly: false },
  { label: "Search results only", shortLabel: "Results only", mode: "search", resultsOnly: true },
  { label: "Read a webpage", shortLabel: "Read page", mode: "page", resultsOnly: false },
] as const;

export function SearchControl({ draft, onChange, disabled }: {
  draft: ResearchDraft;
  onChange: (next: ResearchDraft) => void;
  disabled: boolean;
}) {
  const active = researchChoices.find(({ mode, resultsOnly }) => draft.enabled
    && draft.mode === mode && draft.resultsOnly === resultsOnly);
  return (
    <Menu label={active ? `Search · ${active.label}` : "Search"} placement="above" disabled={disabled}
      className={`composer-pill-menu${active ? " composer-pill-menu-active" : ""}`}
      trigger={<><GlobeIcon size={14} /><span>{active?.shortLabel ?? "Search"}</span><ChevronDownIcon size={12} /></>}>
      {(close) => (
        <>
          {researchChoices.map((choice) => (
            <MenuItem key={choice.label} checked={active === choice} onSelect={() => {
              close();
              onChange({ ...draft, enabled: true, mode: choice.mode, resultsOnly: choice.resultsOnly,
                share: false, reviewedQuery: null });
            }}>{choice.label}</MenuItem>
          ))}
          <MenuSeparator />
          <MenuItem checked={!active} onSelect={() => {
            close();
            onChange({ ...draft, enabled: false, share: false, reviewedQuery: null });
          }}>Search off</MenuItem>
        </>
      )}
    </Menu>
  );
}

export function ResearchBar({ draft, onChange, plan, view, remoteName, remoteLocality, message, disabled, onOpenSettings }: {
  draft: ResearchDraft;
  onChange: (next: ResearchDraft) => void;
  plan: ResearchPlan;
  view: WebResearchView | null;
  remoteName: string | null;
  remoteLocality: string | null;
  message: string;
  disabled: boolean;
  onOpenSettings: () => void;
}) {
  const inputId = useId();
  const provider = view?.settings.provider ?? null;
  const search = draft.mode === "search";
  const searchLocality = provider === "searxng" ? "Self-hosted" : provider ? "Web service" : null;
  const derived = search && draft.queryAttachments.length > 0;
  const title = search ? (draft.resultsOnly ? "Search results" : "Search and answer") : "Read a webpage";
  const answer = plan.answer === "local" ? "Answer on this device"
    : plan.answer === "remote" ? `Answer by ${remoteName} · ${remoteLocality}`
      : search && draft.resultsOnly ? null : "Sources only";

  return (
    <div className="research-bar" role="group" aria-label={title}>
      <div className="research-row">
        <span className="research-label">
          {search && searchLocality ? `${plan.destination} · ${searchLocality}` : title}
          {answer && ` · ${answer}`}
        </span>
        <span className="spacer" />
        {search && draft.query === null && !plan.setupRequired && (
          <button type="button" className="link-button" disabled={disabled}
            onClick={() => onChange({ ...draft, query: normalizeQuery(message) })}>Edit query</button>
        )}
        {search && plan.setupRequired && (
          <button type="button" className="link-button" onClick={onOpenSettings}>Set up search</button>
        )}
        <details className="research-privacy">
          <summary>Privacy</summary>
          <p className="hint">{search
            ? "The search service receives the query. The app reads up to three public pages without cookies or sign-in."
            : "The app reads the public page directly over HTTPS without cookies, scripts or images."}
            {" "}{plan.answerNote}</p>
        </details>
      </div>
      {search && draft.query !== null && (
        <div className="research-edit">
          <label className="sr-only" htmlFor={inputId}>Search query</label>
          <input id={inputId} type="text" maxLength={maxQueryChars} value={draft.query} disabled={disabled}
            placeholder="Search query" onChange={(event) => onChange({ ...draft, query: event.currentTarget.value, reviewedQuery: null })} />
          <button type="button" className="link-button" disabled={disabled}
            onClick={() => onChange({ ...draft, query: null, queryAttachments: [], reviewedQuery: null })}>Use my message</button>
        </div>
      )}
      {derived && provider && (
        <label className="check-row">
          <input type="checkbox" checked={!plan.reviewRequired} disabled={disabled || !plan.outgoing || plan.setupRequired}
            onChange={(event) => onChange({
              ...draft, reviewedQuery: event.currentTarget.checked && plan.outgoing ? { text: plan.outgoing, provider } : null,
            })} />
          <span>I reviewed the query from the image. Send only the query text to {plan.destination}.</span>
        </label>
      )}
      {!search && (
        <div className="research-edit">
          <label className="sr-only" htmlFor={inputId}>Public webpage address</label>
          <input id={inputId} type="url" inputMode="url" maxLength={2048} value={draft.url} disabled={disabled}
            placeholder="https://example.org/article" onChange={(event) => onChange({ ...draft, url: event.currentTarget.value })} />
        </div>
      )}
      {remoteName && !draft.resultsOnly && (
        <label className="check-row">
          <input type="checkbox" checked={draft.share} disabled={disabled}
            onChange={(event) => onChange({ ...draft, share: event.currentTarget.checked })} />
          <span>Let {remoteName} answer using my question and page excerpts</span>
        </label>
      )}
      {plan.problem && <p className="error-text" role="status">{plan.problem}</p>}
    </div>
  );
}
