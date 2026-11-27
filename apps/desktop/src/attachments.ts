// Conversation attachments (Stage 04-D): types mirrored from the engine and
// the pure presentation and request logic the composer and answers use.
// The engine re-checks every rule here; this module only explains them.

export type AttachmentKind = "text" | "markdown" | "pdf" | "png" | "jpeg";
export type AttachmentRetention = "temporary" | "conversation";
export type AttachmentPhase =
  | "queued" | "reading" | "extracting" | "recognizing" | "ready" | "failed" | "cancelled" | "expired";
export type PageCoverage = { pageCount: number; pagesExamined: number; pagesWithoutText: number };

export type AttachmentView = {
  id: string;
  name: string;
  kind: AttachmentKind;
  /** A file from the picker, or a confirmed camera snapshot (Stage 04-E). */
  origin?: "file" | "camera";
  fileBytes: number;
  retention: AttachmentRetention;
  /** Kept in the encrypted profile with its conversation. */
  saved: boolean;
  /** Not yet bound to a conversation. */
  draft: boolean;
  phase: AttachmentPhase;
  /** Set when the phase is failed. */
  message?: string;
  passages: number;
  characters: number;
  pages: PageCoverage | null;
  image: { width: number; height: number; remoteEligible: boolean } | null;
  recognition: string | null;
  notes: string[];
};

export type Chosen = { attachments: AttachmentView[]; rejected: { name: string; message: string }[] };

export type PassageSource =
  | { kind: "lines"; startLine: number; endLine: number }
  | { kind: "page"; page: number }
  | { kind: "image_text" };

export type AttachmentTask = "complete" | "relevant" | "representative" | "search";

export type AttachmentUse = {
  id: string;
  name: string;
  kind: AttachmentKind;
  retention: AttachmentRetention;
  totalPassages: number;
  pages?: PageCoverage | null;
  recognizedText: boolean;
  imageSent: boolean;
};

export type AttachmentRecord = {
  version: number;
  task: AttachmentTask;
  attachments: AttachmentUse[];
  passages: { attachment: string; ordinal: number; source: PassageSource; cited: boolean }[];
  sharedRemotely: boolean;
  noMatch: boolean;
};

/** What the user authorized for one remote message. */
export type ShareDraft = { confirmed: boolean; images: string[] };
export const emptyShare: ShareDraft = { confirmed: false, images: [] };

export const maxAttachments = 6;
export const accepted = "Text, Markdown, text-layer PDF, PNG and JPEG";

export const isImage = (kind: AttachmentKind) => kind === "png" || kind === "jpeg";

export const kindLabel: Record<AttachmentKind, string> = {
  text: "Text", markdown: "Markdown", pdf: "PDF", png: "PNG image", jpeg: "JPEG image",
};

export const preparing = (view: AttachmentView) =>
  view.phase === "queued" || view.phase === "reading" || view.phase === "extracting" || view.phase === "recognizing";

export const previewUrl = (id: string) => `attachment-preview://localhost/${id}`;

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const plural = (count: number, word: string) => `${count.toLocaleString("en-US")} ${word}${count === 1 ? "" : "s"}`;

export function phaseLabel(view: AttachmentView): string {
  switch (view.phase) {
    case "queued": return "Waiting…";
    case "reading": return "Reading…";
    case "extracting": return isImage(view.kind) ? "Checking image…" : "Extracting text…";
    case "recognizing": return "Recognizing text…";
    case "failed": return view.message ?? "Couldn't be prepared";
    case "cancelled": return "Cancelled";
    case "expired": return "Expired. Attach it again to use it.";
    case "ready": return readySummary(view);
  }
}

/** A one-line description of what was read. */
export function readySummary(view: AttachmentView): string {
  const parts: string[] = [kindLabel[view.kind]];
  if (view.pages) {
    parts.push(plural(view.pages.pageCount, "page"));
    if (view.pages.pagesExamined < view.pages.pageCount) parts.push(`first ${view.pages.pagesExamined} read`);
    if (view.pages.pagesWithoutText > 0) parts.push(`${view.pages.pagesWithoutText} without text`);
  } else if (view.image) {
    parts.push(`${view.image.width}×${view.image.height}`);
    parts.push(view.passages > 0 ? "text recognized" : "no text recognized");
  } else {
    parts.push(plural(view.characters, "character"));
  }
  return parts.join(" · ");
}

/** Why the current attachments can't be sent, if they can't. */
export function sendProblem(views: AttachmentView[], included: (id: string) => boolean): string | null {
  const active = views.filter((view) => included(view.id));
  const busy = active.find(preparing);
  if (busy) return `Wait for “${busy.name}” to finish preparing, or cancel it.`;
  const expired = active.find((view) => view.phase === "expired");
  if (expired) return `“${expired.name}” expired. Remove it or attach it again.`;
  const failed = active.find((view) => view.phase === "failed" || view.phase === "cancelled");
  if (failed) return `Remove “${failed.name}”, which couldn't be prepared.`;
  return null;
}

/** Attachments that will be used by the next message. */
export const readyAttachments = (views: AttachmentView[], included: (id: string) => boolean) =>
  views.filter((view) => view.phase === "ready" && included(view.id));

export type RemoteTarget = { id: number; name: string; imageInput: boolean } | null;

export type SharePlan = {
  /** A provider will receive attached content if the user agrees. */
  needed: boolean;
  /** What would be sent, one line per attachment. */
  lines: { id: string; text: string; imageChoice: "available" | "unsupported" | "too_large" | null }[];
  canSend: boolean;
};

/** The sharing notice for attachments sent to a remote or self-hosted provider. */
export function sharePlan(ready: AttachmentView[], remote: RemoteTarget, share: ShareDraft): SharePlan {
  if (!remote || ready.length === 0) return { needed: false, lines: [], canSend: true };
  const lines = ready.map((view) => {
    if (!isImage(view.kind)) {
      return { id: view.id, text: `Extracted text from “${view.name}” (relevant excerpts)`, imageChoice: null };
    }
    const sendsImage = share.images.includes(view.id);
    const imageChoice = !remote.imageInput ? "unsupported" as const
      : view.image && !view.image.remoteEligible ? "too_large" as const : "available" as const;
    const text = sendsImage
      ? `The image “${view.name}” itself (validated, metadata removed)${view.passages ? " and its recognized text" : ""}`
      : view.passages ? `Text recognized from “${view.name}”, not the image` : `Nothing from “${view.name}”: no text was recognized`;
    return { id: view.id, text, imageChoice };
  });
  return { needed: true, lines, canSend: share.confirmed };
}

/** The `attachments` option of a Chat request, or undefined for none. */
export function attachmentRequest(ready: AttachmentView[], remote: RemoteTarget, share: ShareDraft) {
  if (ready.length === 0) return undefined;
  const ids = ready.map((view) => view.id);
  if (!remote || !share.confirmed) return { ids };
  const images = share.images.filter((id) => ready.some((view) => view.id === id && isImage(view.kind)));
  return { ids, share: { provider: remote.id, ids, images } };
}

export function passageLabel(source: PassageSource): string {
  switch (source.kind) {
    case "lines": return source.startLine === source.endLine ? `line ${source.startLine}` : `lines ${source.startLine}–${source.endLine}`;
    case "page": return `page ${source.page}`;
    case "image_text": return "recognized text";
  }
}

/** Notes shown under an answer that used attachments. */
export function recordNotes(record: AttachmentRecord, supplied: number | null, available: (id: string) => boolean): string[] {
  const notes: string[] = [];
  const total = record.attachments.reduce((sum, item) => sum + item.totalPassages, 0);
  if (supplied !== null && supplied < total) {
    notes.push(record.task === "representative"
      ? `Based on ${supplied} representative ${supplied === 1 ? "excerpt" : "excerpts"} of ${total} passages, not the complete ${record.attachments.length === 1 ? "file" : "files"}.`
      : `Based on ${supplied} of ${total} passages.`);
  }
  if (record.noMatch) notes.push("No passage matched the question closely, so representative excerpts were used.");
  for (const item of record.attachments) {
    if (item.pages && item.pages.pagesWithoutText > 0) {
      notes.push(`${item.name}: ${item.pages.pagesWithoutText} of ${item.pages.pagesExamined} pages have no text layer and weren't read.`);
    }
    if (item.pages && item.pages.pagesExamined < item.pages.pageCount) {
      notes.push(`${item.name}: only the first ${item.pages.pagesExamined} of ${item.pages.pageCount} pages were read.`);
    }
    if (isImage(item.kind)) {
      notes.push(item.imageSent
        ? `${item.name}: the image was sent for visual understanding.`
        : item.recognizedText ? `${item.name}: only locally recognized text was used; the model didn't see the image.`
          : `${item.name}: no text was recognized and the image wasn't sent.`);
    }
    if (!available(item.id)) {
      notes.push(item.retention === "temporary"
        ? `${item.name} was temporary and is no longer available. Attach it again to ask more.`
        : `${item.name} is no longer attached.`);
    }
  }
  if (record.sharedRemotely) notes.push("Attached content was sent to the provider for this answer.");
  return notes;
}
