import assert from "node:assert/strict";
import test from "node:test";
import {
  attachmentRequest, emptyShare, formatBytes, passageLabel, phaseLabel, pendingAttachments, readyAttachments, recordedAttachments, recordNotes, sendProblem, sharePlan,
} from "./attachments.ts";

const view = (patch = {}) => ({
  id: `a${"1".repeat(32)}`, name: "report.pdf", kind: "pdf", fileBytes: 2048, retention: "temporary", saved: false,
  draft: true, phase: "ready", passages: 3, characters: 1200, image: null, recognition: null, notes: [],
  pages: { pageCount: 4, pagesExamined: 4, pagesWithoutText: 1 }, ...patch,
});
const all = () => true;

test("summaries describe what was read honestly", () => {
  assert.equal(phaseLabel(view()), "PDF · 4 pages · 1 without text");
  assert.equal(phaseLabel(view({ kind: "png", pages: null, passages: 0, image: { width: 4, height: 3, remoteEligible: true } })),
    "PNG image · 4×3 · no text recognized");
  assert.equal(phaseLabel(view({ phase: "failed", message: "This PDF has no text layer." })), "This PDF has no text layer.");
  assert.equal(formatBytes(3 * 1024 * 1024), "3.0 MB");
  assert.equal(passageLabel({ kind: "lines", startLine: 2, endLine: 5 }), "lines 2–5");
});

test("sending waits for preparation and refuses expired or failed attachments", () => {
  assert.match(sendProblem([view({ phase: "reading" })], all), /finish preparing/);
  assert.match(sendProblem([view({ phase: "expired" })], all), /expired/);
  assert.match(sendProblem([view({ phase: "failed" })], all), /couldn't be prepared/);
  assert.equal(sendProblem([view({ phase: "failed" })], () => false), null);
  assert.equal(sendProblem([view()], all), null);
});

test("remote sharing needs an explicit, exact authorization", () => {
  const image = view({ id: `a${"2".repeat(32)}`, name: "photo.png", kind: "png", pages: null, passages: 1,
    image: { width: 4, height: 3, remoteEligible: true } });
  const ready = readyAttachments([view(), image], all);
  // Local answers send nothing and need no notice.
  assert.equal(sharePlan(ready, null, emptyShare).needed, false);
  assert.deepEqual(attachmentRequest(ready, null, emptyShare), { ids: ready.map((item) => item.id) });
  const remote = { id: 7, name: "Cloud", imageInput: false };
  const plan = sharePlan(ready, remote, emptyShare);
  assert.equal(plan.needed, true);
  assert.equal(plan.canSend, false);
  assert.equal(plan.lines[1].imageChoice, "unsupported");
  assert.match(plan.lines[1].text, /not the image/);
  // Without confirmation no share is requested, so the engine refuses.
  assert.equal(attachmentRequest(ready, remote, emptyShare).share, undefined);
  const confirmed = { confirmed: true, images: [image.id, "unknown"] };
  assert.deepEqual(attachmentRequest(ready, { ...remote, imageInput: true }, confirmed).share,
    { provider: 7, ids: ready.map((item) => item.id), images: [image.id] });
  assert.match(sharePlan(ready, { ...remote, imageInput: true }, confirmed).lines[1].text, /image .* itself/);
});

test("answer notes report sampled coverage, OCR limits and availability", () => {
  const record = {
    version: 1, task: "representative", sharedRemotely: false, noMatch: true,
    attachments: [
      { id: "a", name: "big.pdf", kind: "pdf", retention: "temporary", totalPassages: 30, recognizedText: false, imageSent: false,
        pages: { pageCount: 120, pagesExamined: 100, pagesWithoutText: 2 } },
      { id: "b", name: "sign.png", kind: "png", retention: "conversation", totalPassages: 1, recognizedText: true, imageSent: false },
    ],
    passages: [],
  };
  const notes = recordNotes(record, 6, (id) => id === "b");
  assert.ok(notes.some((note) => /6 representative excerpts of 31 passages, not the complete files/.test(note)));
  assert.ok(notes.some((note) => /No passage matched/.test(note)));
  assert.ok(notes.some((note) => /first 100 of 120 pages/.test(note)));
  assert.ok(notes.some((note) => /didn't see the image/.test(note)));
  assert.ok(notes.some((note) => /big.pdf was temporary and is no longer available/.test(note)));
  assert.ok(!notes.some((note) => /sign.png is no longer/.test(note)));
});

test("only accepted attachment IDs leave the tray, including on refresh and reopen", () => {
  const sent = view();
  const paused = view({ id: "paused" });
  const later = view({ id: "later", phase: "reading" });
  const retained = { ...sent, draft: false, saved: true };
  const record = { attachments: [{ id: sent.id }] };
  const attempt = { message: { context: { attachments: record } } };
  const accepted = [{ kind: "chat", assistants: [attempt] }];

  // A refusal before acceptance leaves no attempt record to hide the draft.
  assert.deepEqual(pendingAttachments([sent, paused], []), [sent, paused]);
  assert.deepEqual(pendingAttachments([sent, paused], accepted), [paused]);
  assert.deepEqual(pendingAttachments([retained, paused, later], accepted), [paused, later]);
  assert.deepEqual(recordedAttachments([retained, paused, later], record), [retained]);
  assert.deepEqual(recordedAttachments([view({ ...retained, phase: "expired" })], record), []);
});
