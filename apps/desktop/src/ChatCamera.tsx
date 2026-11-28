// Camera snapshots for Chat (Stage 04-E): an explicit click, visible camera
// progress, an optional camera choice, then preview and crop before the
// snapshot becomes a temporary attachment. The camera is off before the
// preview appears; nothing is attached until the user confirms.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { CameraIcon, Spinner } from "./icons";
import { errorMessage } from "./inference";
import { maxAttachments } from "./attachments";
import { CameraPicker, CameraStatus, OcrLanguages, SnapshotReview } from "./VisualInput";
import {
  previewUrl, type CameraNode, type CameraStage, type PendingSnapshot, type Region, type Taken, type VisualSettings,
} from "./visual";
import type { OcrState } from "./glance";

export type VisualState = { settings: VisualSettings; ocr: OcrState };

export function useChatCamera({ scope, count, disabled, onAttached }: {
  scope: () => number | null;
  count: number;
  disabled: boolean;
  onAttached: (scope: number | null) => void;
}) {
  const [stage, setStage] = useState<CameraStage | "waiting" | null>(null);
  const [cameras, setCameras] = useState<CameraNode[] | null>(null);
  const [snapshot, setSnapshot] = useState<PendingSnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [visual, setVisual] = useState<VisualState | null>(null);

  useEffect(() => {
    let active = true;
    const subscription = listen<CameraStage>("camera-stage", (event) => {
      if (active) setStage((current) => current === null ? current : event.payload);
    });
    invoke<VisualState>("visual_settings").then((value) => { if (active) setVisual(value); }).catch(() => undefined);
    return () => { active = false; void subscription.then((unlisten) => unlisten()).catch(() => undefined); };
  }, []);

  async function take(device?: string) {
    setCameras(null); setSnapshot(null); setError(null); setStage("waiting"); setBusy(true);
    try {
      const taken = await invoke<Taken>("camera_capture", { device: device ?? null });
      if (taken.kind === "choose") setCameras(taken.cameras); else setSnapshot(taken.snapshot);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setStage(null); setBusy(false);
    }
  }

  async function attach(region: Region | null) {
    if (!snapshot) return;
    const target = scope();
    setBusy(true); setError(null);
    try {
      await invoke("camera_attach", { request: { id: snapshot.id, region, conversationId: target, retention: "temporary" } });
      setSnapshot(null);
      onAttached(target);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }

  async function discard() {
    if (!snapshot) return;
    try { await invoke("camera_discard", { id: snapshot.id }); } catch { /* already gone */ }
    setSnapshot(null);
  }

  async function save(settings: VisualSettings) {
    try {
      setVisual(await invoke<VisualState>("visual_settings_save", { settings }));
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }

  const full = count >= maxAttachments;
  const button = (
    <button type="button" className="composer-pill attach-pill" disabled={disabled || busy || full || !!snapshot}
      aria-label="Take a camera snapshot" onClick={() => void take()}
      title={full ? `At most ${maxAttachments} attachments at a time` : "Take one camera snapshot, review it, then attach it"}>
      {stage ? <Spinner size={12} /> : <CameraIcon size={14} />}
      <span>Camera</span>
    </button>
  );

  const panel = (stage || cameras || snapshot || error) ? (
    <div className="chat-camera" role="group" aria-label="Camera snapshot">
      {stage && <CameraStatus stage={stage} onCancel={() => void invoke("camera_cancel").catch(() => undefined)} />}
      {cameras && !stage && <CameraPicker cameras={cameras} disabled={busy} onChoose={(key) => void take(key)} onCancel={() => setCameras(null)} />}
      {snapshot && <SnapshotReview snapshot={snapshot} url={previewUrl(snapshot.id)} disabled={busy || disabled} useLabel="Attach"
        onUse={(region) => void attach(region)} onDiscard={() => void discard()} />}
      {snapshot && visual?.ocr.available && (
        <OcrLanguages available={visual.ocr.languages} chosen={visual.ocr.defaults} disabled={busy}
          missing={visual.ocr.missingRecommended} onChange={(ocrLanguages) => void save({ ...visual.settings, ocrLanguages })} />
      )}
      {snapshot && <p className="hint">Attached snapshots are temporary unless you choose Keep. They are never saved to Pictures or anywhere else on disk.</p>}
      {error && <p className="error-text" role="alert">{error} <button type="button" className="link-button" onClick={() => setError(null)}>Dismiss</button></p>}
    </div>
  ) : null;

  return { button, panel, visual, save };
}
