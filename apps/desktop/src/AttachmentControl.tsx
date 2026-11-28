import { useState } from "react";
import { CameraIcon, CloseIcon, FileIcon, ImageIcon, LanguagesIcon, LockIcon, PaperclipIcon, QuoteIcon, ScanTextIcon, SearchIcon, Spinner } from "./icons";
import { errorMessage } from "./inference";
import {
  accepted, formatBytes, isImage, maxAttachments, phaseLabel, preparing, previewUrl,
  type AttachmentView, type SharePlan, type ShareDraft,
} from "./attachments";

/** Composer button that opens the native multi-file picker. */
export function AttachButton({ disabled, busy, count, onAttach }: {
  disabled: boolean;
  busy: boolean;
  count: number;
  onAttach: () => void;
}) {
  const full = count >= maxAttachments;
  return (
    <button type="button" className="composer-pill attach-pill" disabled={disabled || busy || full}
      aria-label="Attach files" onClick={onAttach}
      title={full ? `At most ${maxAttachments} attachments at a time` : `Attach files: ${accepted}`}>
      {busy ? <Spinner size={12} /> : <PaperclipIcon size={14} />}
      <span>Attach</span>
    </button>
  );
}

/** Shared visual actions for ready images: the same operations the panel
 * offers, carried out by Chat's own attachment and research pipelines. */
export type VisualActions = {
  loadText: (id: string) => Promise<string>;
  useText: (view: AttachmentView, text: string) => void;
  translate: (view: AttachmentView, target: string) => void;
  ask: (view: AttachmentView) => void;
  search: (view: AttachmentView, text: string) => void;
  target: string;
};

function ImageActions({ view, disabled, actions }: { view: AttachmentView; disabled: boolean; actions: VisualActions }) {
  const [text, setText] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [translating, setTranslating] = useState(false);
  const [target, setTarget] = useState(actions.target);
  const [problem, setProblem] = useState<string | null>(null);
  const [selected, setSelected] = useState("");
  const request = () => {
    if (!target.trim()) return;
    actions.translate(view, target.trim());
    setTranslating(false);
  };
  async function load() {
    if (text !== null) return text;
    try {
      const value = await actions.loadText(view.id);
      setText(value);
      return value;
    } catch (failure) {
      setProblem(errorMessage(failure));
      return null;
    }
  }
  return (
    <div className="image-actions">
      <div className="attachment-actions" role="group" aria-label={`Actions for ${view.name}`}>
        <button type="button" className="link-button" aria-expanded={open} disabled={disabled}
          onClick={() => { setOpen(!open); void load(); }}><ScanTextIcon size={11} />Text</button>
        <button type="button" className="link-button" aria-expanded={translating} disabled={disabled || view.passages === 0}
          title={view.passages === 0 ? "No text was recognized in this image" : "Translate the recognized text"}
          onClick={() => setTranslating(!translating)}><LanguagesIcon size={11} />Translate</button>
        <button type="button" className="link-button" disabled={disabled} onClick={() => actions.ask(view)}><QuoteIcon size={11} />Ask</button>
        <button type="button" className="link-button" disabled={disabled}
          onClick={() => void load().then((value) => { if (value !== null) actions.search(view, selected || value); })}><SearchIcon size={11} />Search</button>
      </div>
      {translating && (
        // Not a <form>: the card already sits inside the composer's form.
        <div className="image-translate" role="group" aria-label="Translation target">
          <label>Translate into <input value={target} maxLength={100} placeholder="e.g. French"
            onChange={(event) => setTarget(event.currentTarget.value)}
            onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); request(); } }} /></label>
          <button type="button" className="link-button" disabled={!target.trim()} onClick={request}>Write request</button>
        </div>
      )}
      {open && (
        <div className="image-text">
          {text === null ? <span className="hint"><Spinner size={11} />Loading recognized text…</span>
            : text ? <>
              <textarea readOnly dir="auto" value={text} rows={4} aria-label={`Text recognized in ${view.name}`}
                onSelect={(event) => setSelected(event.currentTarget.value.slice(event.currentTarget.selectionStart, event.currentTarget.selectionEnd))} />
              <div className="attachment-actions">
                <button type="button" className="link-button" disabled={disabled} onClick={() => actions.useText(view, selected || text)}>Use {selected ? "selection" : "text"} in message</button>
                <span className="hint">{view.recognition ? `Recognized on this device: ${view.recognition}` : "Recognized on this device"}</span>
              </div>
            </> : <span className="hint">No text was recognized in this image.</span>}
        </div>
      )}
      {problem && <p className="attachment-note error-text">{problem}</p>}
    </div>
  );
}

function AttachmentCard({ view, included, disabled, onToggle, onCancel, onRemove, onRetention, visual }: {
  view: AttachmentView;
  included: boolean;
  disabled: boolean;
  onToggle: () => void;
  onCancel: () => void;
  onRemove: () => void;
  onRetention: (keep: boolean) => void;
  visual?: VisualActions;
}) {
  const busy = preparing(view);
  const problem = view.phase === "failed" || view.phase === "expired" || view.phase === "cancelled";
  const ready = view.phase === "ready";
  const kept = view.retention === "conversation";
  return (
    <li className={`attachment-card${problem ? " is-problem" : ""}${ready && !included ? " is-paused" : ""}`}
      aria-label={`${view.name}: ${phaseLabel(view)}`}>
      <div className="attachment-thumb" aria-hidden="true">
        {ready && view.image ? <img src={previewUrl(view.id)} alt="" />
          : isImage(view.kind) ? <ImageIcon size={18} /> : <FileIcon size={18} />}
      </div>
      <div className="attachment-body">
        <p className="attachment-name" title={`${view.name} · ${formatBytes(view.fileBytes)}`}>
          {view.origin === "camera" && <CameraIcon size={11} aria-label="Camera snapshot" />}{view.name}</p>
        <p className={`attachment-status${problem ? " error-text" : ""}`} role={busy ? "status" : undefined}>
          {busy && <Spinner size={11} />}
          <span>{phaseLabel(view)}</span>
        </p>
        {ready && view.notes.map((note) => <p key={note} className="attachment-note">{note}</p>)}
        {ready && (
          <div className="attachment-actions">
            <button type="button" className="link-button" aria-pressed={included} disabled={disabled} onClick={onToggle}
              title={included ? "Used in your next message" : "Not used until you turn it back on"}>
              {included ? "Using" : "Paused"}
            </button>
            <button type="button" className={`link-button attachment-keep${kept ? " is-on" : ""}`} aria-pressed={kept}
              disabled={disabled} onClick={() => onRetention(!kept)}
              title={kept
                ? "Kept with this conversation in the encrypted profile. Turn off to make it temporary."
                : "Temporary: kept in memory for this session only. Turn on to keep it with this conversation."}>
              <LockIcon size={11} />{kept ? (view.saved ? "Kept" : "Keep after sending") : "Temporary"}
            </button>
          </div>
        )}
        {ready && view.image && visual && <ImageActions view={view} disabled={disabled} actions={visual} />}
      </div>
      {busy ? (
        <button type="button" className="chip-remove" aria-label={`Cancel preparing ${view.name}`} title="Cancel"
          onClick={onCancel}><CloseIcon size={12} /></button>
      ) : (
        <button type="button" className="chip-remove" disabled={disabled} aria-label={`Remove ${view.name}`}
          title={view.saved ? "Remove and delete the kept copy" : "Remove"} onClick={onRemove}><CloseIcon size={12} /></button>
      )}
    </li>
  );
}

/**
 * Attachments of the open draft or conversation, with status, previews and
 * the sharing notice when a remote provider would receive their content.
 */
export function AttachmentTray({
  views, included, disabled, error, rejected, plan, share, remoteName, onShareChange,
  onToggle, onCancel, onRemove, onRetention, onDismissError, showSharing, onCancelReview, visual,
}: {
  views: AttachmentView[];
  included: (id: string) => boolean;
  disabled: boolean;
  error: string | null;
  rejected: { name: string; message: string }[];
  plan: SharePlan;
  share: ShareDraft;
  remoteName: string | null;
  onShareChange: (next: ShareDraft) => void;
  onToggle: (id: string) => void;
  onCancel: (id: string) => void;
  onRemove: (id: string) => void;
  onRetention: (id: string, keep: boolean) => void;
  onDismissError: () => void;
  showSharing: boolean;
  onCancelReview?: () => void;
  visual?: VisualActions;
}) {
  const sharing = showSharing && plan.needed && remoteName;
  if (views.length === 0 && !error && rejected.length === 0 && !sharing && !onCancelReview) return null;
  return (
    <div className="attachment-tray" role="group" aria-label="Attachments">
      {views.length > 0 && (
        <ul className="attachment-list">
          {views.map((view) => (
            <AttachmentCard key={view.id} view={view} included={included(view.id)} disabled={disabled}
              onToggle={() => onToggle(view.id)} onCancel={() => onCancel(view.id)} onRemove={() => onRemove(view.id)}
              onRetention={(keep) => onRetention(view.id, keep)} visual={visual} />
          ))}
        </ul>
      )}
      {(error || rejected.length > 0) && (
        <div className="attachment-errors" role="alert">
          {error && <p className="error-text">{error}</p>}
          {rejected.map((item) => <p key={item.name} className="error-text">{item.name}: {item.message}</p>)}
          <button type="button" className="link-button" onClick={onDismissError}>Dismiss</button>
        </div>
      )}
      {sharing && (
        <div className="attachment-share">
          <p className="research-label">Sent to {remoteName} with this message</p>
          <ul className="attachment-share-list">
            {plan.lines.map((line) => (
              <li key={line.id}>
                <span>{line.text}</span>
                {line.imageChoice === "available" && (
                  <label className="check-row">
                    <input type="checkbox" checked={share.images.includes(line.id)} disabled={disabled}
                      onChange={(event) => onShareChange({
                        confirmed: false,
                        images: event.currentTarget.checked
                          ? [...share.images, line.id] : share.images.filter((id) => id !== line.id),
                      })} />
                    <span>Send the image itself for visual understanding</span>
                  </label>
                )}
                {line.imageChoice === "unsupported" && <span className="hint"> {remoteName} isn't set up for images; only recognized text can be sent.</span>}
                {line.imageChoice === "too_large" && <span className="hint"> The image is over 3 MB, so only recognized text can be sent.</span>}
              </li>
            ))}
          </ul>
          <label className="check-row">
            <input type="checkbox" checked={share.confirmed} disabled={disabled}
              onChange={(event) => onShareChange({ ...share, confirmed: event.currentTarget.checked })} />
            <span>Send this attached content to {remoteName} for this message only</span>
          </label>
          <p className="hint">Original files are never sent. The provider's own retention applies to what it receives.</p>
        </div>
      )}
      {onCancelReview && <div className="attachment-actions">
        {sharing && <span className="hint">After confirming, choose Regenerate again.</span>}
        <button type="button" className="link-button" onClick={onCancelReview}>Cancel review</button>
      </div>}
      {views.length > 0 && (
        <p className="hint attachment-footnote">
          Attachments are read on this device and aren't added to your library or memory. Temporary ones last for this
          session; saved answers can still contain information from them.
        </p>
      )}
    </div>
  );
}
