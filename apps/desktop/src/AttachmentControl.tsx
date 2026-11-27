import { CloseIcon, FileIcon, ImageIcon, LockIcon, PaperclipIcon, Spinner } from "./icons";
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

function AttachmentCard({ view, included, disabled, onToggle, onCancel, onRemove, onRetention }: {
  view: AttachmentView;
  included: boolean;
  disabled: boolean;
  onToggle: () => void;
  onCancel: () => void;
  onRemove: () => void;
  onRetention: (keep: boolean) => void;
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
        <p className="attachment-name" title={`${view.name} · ${formatBytes(view.fileBytes)}`}>{view.name}</p>
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
  onToggle, onCancel, onRemove, onRetention, onDismissError,
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
}) {
  if (views.length === 0 && !error && rejected.length === 0) return null;
  return (
    <div className="attachment-tray" role="group" aria-label="Attachments">
      {views.length > 0 && (
        <ul className="attachment-list">
          {views.map((view) => (
            <AttachmentCard key={view.id} view={view} included={included(view.id)} disabled={disabled}
              onToggle={() => onToggle(view.id)} onCancel={() => onCancel(view.id)} onRemove={() => onRemove(view.id)}
              onRetention={(keep) => onRetention(view.id, keep)} />
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
      {plan.needed && remoteName && (
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
      {views.length > 0 && (
        <p className="hint attachment-footnote">
          Attachments are read on this device and aren't added to your library or memory. Temporary ones last for this
          session; saved answers can still contain information from them.
        </p>
      )}
    </div>
  );
}
