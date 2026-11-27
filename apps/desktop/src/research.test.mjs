import assert from "node:assert/strict";
import test from "node:test";
import {
  emptyResearch, historyNote, normalizeQuery, queryProblem, researchOptions, researchPlan, researchSummary,
  stageLabel, supplyLabel, urlProblem,
} from "./research.ts";

const view = (patch = {}) => ({
  settings: { provider: "brave", braveSaveResults: false, searxngEndpoint: "", searxngSaveResults: false, puriliPersonalUse: false },
  braveKey: "stored",
  searxngOrigin: null,
  searxngNetwork: null,
  searxngError: null,
  localOnly: false,
  ...patch,
});

test("queries are normalized and bounded like the engine", () => {
  assert.equal(normalizeQuery("  rust \n 1.100  "), "rust 1.100");
  assert.match(queryProblem("   "), /Enter something/);
  assert.match(queryProblem("a".repeat(401)), /at most 400/);
  assert.equal(queryProblem("é".repeat(400)), null);
});

test("direct page addresses are pre-checked for public https", () => {
  assert.equal(urlProblem("https://news.example.org/a?b=1"), null);
  assert.match(urlProblem("http://news.example.org"), /Only https/);
  assert.match(urlProblem("news.example.org"), /complete https/);
  assert.match(urlProblem("https://user:pw@example.org"), /user name/);
  assert.match(urlProblem("https://example.org:8443/"), /standard HTTPS port/);
  for (const local of ["https://127.0.0.1/", "https://[::1]/", "https://localhost/", "https://router.local/", "https://intranet/"]) {
    assert.match(urlProblem(local), /public websites/, local);
  }
});

test("the plan discloses provider, outgoing query and where the answer is written", () => {
  const draft = { ...emptyResearch, enabled: true };
  const local = researchPlan({ draft, message: "When did  it open?", view: view(), remoteName: null, modelReady: true });
  assert.equal(local.canSend, true);
  assert.equal(local.destination, "Brave Search");
  assert.equal(local.outgoing, "When did it open?");
  assert.equal(local.answer, "local");
  assert.match(local.answerNote, /still use the internet/);

  const edited = researchPlan({ draft: { ...draft, query: "bridge opening" }, message: "private text", view: view(), remoteName: null, modelReady: true });
  assert.equal(edited.outgoing, "bridge opening");

  const remote = researchPlan({ draft, message: "q", view: view(), remoteName: "Cloud", modelReady: false });
  assert.equal(remote.answer, "none");
  assert.match(remote.answerNote, /Allow Cloud/);
  assert.equal(researchPlan({ draft: { ...draft, share: true }, message: "q", view: view(), remoteName: "Cloud", modelReady: false }).answer, "remote");

  const noModel = researchPlan({ draft, message: "q", view: view(), remoteName: null, modelReady: false });
  assert.equal(noModel.canSend, true);
  assert.equal(noModel.answer, "none");

  assert.match(researchPlan({ draft, message: "q", view: view({ braveKey: "missing" }), remoteName: null, modelReady: true }).problem, /API key/);
  assert.match(researchPlan({ draft, message: "q", view: view({ settings: { ...view().settings, provider: null } }), remoteName: null, modelReady: true }).problem, /Choose a search provider/);
  const offline = researchPlan({ draft, message: "q", view: view({ localOnly: true }), remoteName: null, modelReady: true });
  assert.equal(offline.canSend, false);
  assert.match(offline.problem, /even with a local model/);

  const page = researchPlan({ draft: { ...draft, mode: "page", url: "https://example.org/a" }, message: "Summarize", view: view({ settings: { ...view().settings, provider: null } }), remoteName: null, modelReady: true });
  assert.equal(page.canSend, true);
  assert.equal(page.outgoing, null);
});

test("Purili is named as experimental, needs no key and keeps results to the session", () => {
  const purili = view({ settings: { ...view().settings, provider: "purili", puriliPersonalUse: true }, braveKey: "missing" });
  const draft = { ...emptyResearch, enabled: true };
  const plan = researchPlan({ draft, message: "history of the bicycle", view: purili, remoteName: null, modelReady: true });
  assert.equal(plan.canSend, true);
  assert.equal(plan.destination, "Purili (experimental)");
  assert.deepEqual(researchOptions(draft, purili), { mode: "search", provider: "purili" });
  const note = historyNote({ transient: false, record: { provider: "purili" }, sources: [{ saved: false }] });
  assert.match(note, /^Purili results are kept only/);
  assert.doesNotMatch(note, /storage terms/);
});

test("options carry only what was authorized for this message", () => {
  assert.equal(researchOptions(emptyResearch, view()), undefined);
  assert.deepEqual(researchOptions({ ...emptyResearch, enabled: true }, view()), { mode: "search", provider: "brave" });
  assert.deepEqual(researchOptions({ ...emptyResearch, enabled: true, query: " a  b ", share: true }, view()),
    { mode: "search", provider: "brave", query: "a b", shareWithProvider: true });
  // A query started from an attached image's text names that attachment, so
  // the engine labels the query with its classes; nothing else changes.
  const visual = researchOptions({ ...emptyResearch, enabled: true, query: "CAMERA TEST 42", queryAttachments: [`a${"1".repeat(32)}`] }, view());
  assert.deepEqual(visual.queryAttachments, [`a${"1".repeat(32)}`]);
  assert.equal(visual.query, "CAMERA TEST 42");
  assert.equal(researchOptions({ ...emptyResearch, enabled: true, queryAttachments: ["x"] }, view()).queryAttachments, undefined);
  assert.deepEqual(researchOptions({ ...emptyResearch, enabled: true, mode: "page", url: " https://e.org/ ", resultsOnly: true }, view()),
    { mode: "page", url: "https://e.org/", resultsOnly: true });
});

test("summaries and labels stay honest about what was read and saved", () => {
  const record = { version: 1, mode: "search", provider: "brave", synthesis: "local", discovered: 5, retrieved: 2, unavailable: 1, sources: [], transientResults: true };
  assert.equal(researchSummary(record), "Brave Search · 5 results · 2 pages read · 1 unavailable · answered on this device");
  assert.equal(researchSummary({ ...record, mode: "page", synthesis: "none", retrieved: 1, unavailable: 0 }), "Direct page · 1 page read · no written answer");
  const source = { supplied: true, cited: false };
  assert.equal(supplyLabel(source, "local"), "Supplied, not cited");
  assert.equal(supplyLabel({ ...source, cited: true }, "local"), "Cited");
  assert.equal(supplyLabel(source, "none"), "Listed");
  assert.equal(historyNote({ transient: true, sources: [] }), null);
  assert.match(historyNote({ transient: false, record, sources: [{ saved: false }] }), /storage terms/);
  assert.equal(stageLabel("reading", "page"), "Reading the page…");
});
