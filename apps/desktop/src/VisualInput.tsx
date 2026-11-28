// Shared visual input controls (Stage 04-E), used by the contextual panel
// and by Chat: camera progress, camera choice, snapshot preview with an
// optional crop, and the shared OCR language choice. They only present
// state; every action goes through the native host.
import { useRef, useState, type PointerEvent } from "react";
import { CameraIcon, CheckIcon, ChevronDownIcon, CropIcon, LanguagesIcon, Spinner, StopIcon, TrashIcon } from "./icons";
import { languageName } from "./glance";
import {
  cameraStageText, cameraSummary, maxOcrLanguages, selectionRegion, toggleLanguage,
  type CameraNode, type CameraStage, type PendingSnapshot, type Region,
} from "./visual";

/** Visible while a snapshot runs: the camera is never used out of sight. */
export function CameraStatus({ stage, onCancel }: { stage: CameraStage | "waiting"; onCancel: () => void }) {
  const text = stage === "waiting"
    ? "Camera requested: allow access if your desktop asks. The camera turns off as soon as one snapshot is taken."
    : cameraStageText[stage];
  return (
    <div className={`camera-status${stage === "active" ? " is-live" : ""}`} role="status" aria-live="polite">
      <span className="camera-dot" aria-hidden="true" />
      <CameraIcon size={14} />
      <span>{text}</span>
      {stage !== "released" && <button type="button" className="link-button" onClick={onCancel}><StopIcon size={12} />Cancel</button>}
    </div>
  );
}

/** Several cameras: the user picks one; nothing was streamed meanwhile. */
export function CameraPicker({ cameras, disabled, onChoose, onCancel }: {
  cameras: CameraNode[]; disabled: boolean; onChoose: (key: string) => void; onCancel: () => void;
}) {
  const [chosen, setChosen] = useState(cameras[0]?.key ?? "");
  return (
    <fieldset className="camera-picker" disabled={disabled}>
      <legend><CameraIcon size={14} />Choose a camera</legend>
      {cameras.map(camera => (
        <label key={camera.key} className="check-row">
          <input type="radio" name="camera" value={camera.key} checked={chosen === camera.key} onChange={() => setChosen(camera.key)} />
          <span>{camera.name}</span>
        </label>
      ))}
      <div className="camera-actions">
        <button type="button" className="primary-button" disabled={!chosen} onClick={() => onChoose(chosen)}><CameraIcon size={14} />Take snapshot</button>
        <button type="button" className="ghost-button" onClick={onCancel}>Cancel</button>
      </div>
    </fieldset>
  );
}

/**
 * The captured snapshot before anything uses it: the camera is already off.
 * Dragging over the image chooses an area; otherwise the whole snapshot is
 * used. Arrow keys are not needed: Use whole snapshot is always available.
 */
export function SnapshotReview({ snapshot, url, disabled, useLabel, onUse, onDiscard }: {
  snapshot: PendingSnapshot; url: string; disabled: boolean; useLabel: string;
  onUse: (region: Region | null) => void; onDiscard: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [start, setStart] = useState<{ x: number; y: number } | null>(null);
  const [end, setEnd] = useState<{ x: number; y: number } | null>(null);
  // A ref, not state: pointer events can arrive faster than renders.
  const dragging = useRef(false);
  const point = (event: PointerEvent<HTMLDivElement>) => {
    const rect = box.current!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };
  const view = () => {
    const rect = box.current?.getBoundingClientRect();
    return { width: rect?.width ?? 0, height: rect?.height ?? 0 };
  };
  const region = start && end ? selectionRegion(start, end, view(), snapshot) : null;
  const shown = start && end && region ? {
    left: Math.min(start.x, end.x), top: Math.min(start.y, end.y),
    width: Math.abs(end.x - start.x), height: Math.abs(end.y - start.y),
  } : null;
  return (
    <section className="snapshot-review" aria-label="Camera snapshot preview">
      <div className="snapshot-head">
        <strong><CameraIcon size={14} />Camera snapshot</strong>
        <span>{cameraSummary(snapshot.record)}</span>
      </div>
      <div ref={box} className={`snapshot-frame${disabled ? " is-disabled" : ""}`}
        onPointerDown={event => {
          if (disabled) return;
          // Capture keeps the drag going outside the image; it is best effort.
          try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* not capturable */ }
          const p = point(event); setStart(p); setEnd(p); dragging.current = true;
        }}
        onPointerMove={event => { if (dragging.current) setEnd(point(event)); }}
        onPointerUp={event => { if (dragging.current) { setEnd(point(event)); dragging.current = false; } }}
        onPointerCancel={() => { dragging.current = false; }}>
        <img src={url} alt={`Camera snapshot, ${snapshot.width} by ${snapshot.height} pixels`} draggable={false} />
        {shown && <div className="snapshot-selection" style={shown} aria-hidden="true" />}
      </div>
      <p className="hint">The camera is off. Nothing has been processed, shared or saved. Drag over the image to use only part of it.</p>
      <div className="camera-actions">
        <button type="button" className="primary-button" disabled={disabled} onClick={() => onUse(region)}>
          {region ? <CropIcon size={14} /> : <CheckIcon size={14} />}{region ? `${useLabel} (selected ${region.width} × ${region.height})` : `${useLabel} (whole snapshot)`}
        </button>
        {region && <button type="button" className="ghost-button" disabled={disabled} onClick={() => { setStart(null); setEnd(null); }}>Clear selection</button>}
        <button type="button" className="ghost-button" disabled={disabled} onClick={onDiscard}><TrashIcon size={14} />Discard</button>
      </div>
    </section>
  );
}

/** The shared recognition languages for screen, camera and attached images. */
export function OcrLanguages({ available, chosen, disabled, missing, onChange }: {
  available: string[]; chosen: string[]; disabled: boolean; missing: string[]; onChange: (next: string[]) => void;
}) {
  return (
    <details className="ocr-languages">
      <summary><LanguagesIcon size={13} />Text languages: {chosen.map(languageName).join(", ") || "none"}<ChevronDownIcon size={12} /></summary>
      <div className="ocr-language-list">{available.map(code => (
        <label className="check-row" key={code} title={code}>
          <input type="checkbox" disabled={disabled || (!chosen.includes(code) && chosen.length >= maxOcrLanguages)}
            checked={chosen.includes(code)} onChange={event => onChange(toggleLanguage(chosen, code, event.currentTarget.checked))} />
          <span>{languageName(code)}</span>
        </label>
      ))}</div>
      <p className="hint">Used for screen regions, camera snapshots and attached images. Recognition quality differs by language and script; choose up to four.
        {missing.length > 0 && <> More languages: install <code>{missing.join(" ")}</code> with apt. Nothing is installed automatically.</>}</p>
    </details>
  );
}

export function Busy({ text }: { text: string }) {
  return <span className="hint" role="status"><Spinner size={12} />{text}</span>;
}
