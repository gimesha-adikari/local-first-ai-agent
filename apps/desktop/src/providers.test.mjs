import assert from "node:assert/strict";
import test from "node:test";
import {
  activeRemote, disclosure, draftProblem, formatMoney, listText, localityLabel, microsText, newProvider, parseMicros,
  readinessCopy, sharingSummary, usageLines, usedTokens,
} from "./providers.ts";

const view = (patch = {}) => ({
  id: 1,
  settings: { ...newProvider("cloud"), name: "Fixture", endpoint: "https://api.example.com/v1", model: "m", retentionSummary: "30 days" },
  origin: "https://api.example.com:443",
  network: "public",
  encrypted: true,
  credential: "stored",
  period: "2026-11",
  usage: { requests: 0, reportedTokens: 0, estimatedTokens: 0, reservedTokens: 0, costMicros: 0, unpricedRequests: 0 },
  modalities: ["text"],
  readiness: "ready",
  active: true,
  ...patch,
});

test("only the explicitly selected provider receives Chat", () => {
  const providers = [view()];
  assert.equal(activeRemote({ activeProvider: null }, providers), null);
  assert.equal(activeRemote({ activeProvider: 2 }, providers), null);
  assert.equal(activeRemote({ activeProvider: 1 }, providers)?.id, 1);
  assert.equal(activeRemote(null, providers), null);
});

test("disclosure names the provider, shared and withheld categories", () => {
  const summary = sharingSummary({ history: true, memories: false, documents: false });
  assert.deepEqual(summary.shared, ["your message", "conversation history"]);
  assert.deepEqual(summary.withheld, ["saved memories", "document excerpts"]);
  assert.equal(listText(["a", "b", "c"]), "a, b and c");
  const text = disclosure(view());
  assert.match(text, /^Sends your message and conversation history to Fixture \(cloud\)\. Withholds saved memories and document excerpts\.$/);
  assert.equal(localityLabel(view({ settings: { ...newProvider("self_hosted") }, network: "loopback", encrypted: false })),
    "Self-hosted on this computer, unencrypted");
});

test("money and usage keep measured, estimated and unknown values apart", () => {
  assert.equal(parseMicros(""), null);
  assert.equal(parseMicros("2.5"), 2_500_000);
  assert.equal(parseMicros("0.000001"), 1);
  assert.ok(Number.isNaN(parseMicros("1e3")));
  assert.ok(Number.isNaN(parseMicros("-1")));
  assert.equal(microsText(2_500_000), "2.5");
  assert.equal(microsText(3_000_000), "3");
  assert.equal(formatMoney(4_500_000, "USD"), "$4.50");
  const used = view({
    usage: { requests: 3, reportedTokens: 1000, estimatedTokens: 500, reservedTokens: 100, costMicros: 0, unpricedRequests: 0 },
  });
  assert.equal(usedTokens(used.usage), 1600);
  const lines = usageLines(used);
  assert.match(lines[0], /1,000 tokens reported by the provider · 500 estimated where no report arrived · 100 reserved in flight/);
  assert.equal(lines[1], "Cost unknown: no prices configured.");
  assert.match(lines[2], /998,400 of 1,000,000 tokens left/);
  const priced = view({
    settings: { ...view().settings, pricing: { currency: "USD", inputMicrosPerMillion: 1_000_000, outputMicrosPerMillion: 2_000_000 } },
    usage: { ...used.usage, costMicros: 1_250_000, unpricedRequests: 1 },
  });
  assert.match(usageLines(priced)[1], /Estimated cost \$1\.25, plus 1 unpriced request\./);
});

test("readiness and draft problems explain what blocks a provider", () => {
  assert.equal(readinessCopy(view({ readiness: "needs_credential", credential: "missing" })).label, "Needs an API key");
  assert.equal(readinessCopy(view({ readiness: "credential_unavailable", credential: "locked" })).label, "Keyring locked");
  assert.equal(readinessCopy(view({ readiness: "credential_unavailable", credential: "unavailable" })).tone, "error");
  const draft = { ...newProvider("cloud"), name: "X", endpoint: "http://api.example.com", model: "m", retentionSummary: "kept" };
  assert.equal(draftProblem(draft), "Cloud providers must use https.");
  assert.equal(draftProblem({ ...draft, endpoint: "https://api.example.com/v1" }), null);
  assert.equal(draftProblem({ ...draft, endpoint: "https://api.example.com/v1", retentionSummary: "" }),
    "Summarize the provider's retention policy.");
  assert.equal(draftProblem({ ...draft, endpoint: "https://api.example.com/v1", budget: { monthlyTokens: null, monthlyCostMicros: 5 } }),
    "A spending limit needs prices.");
  const local = { ...newProvider("self_hosted"), name: "Server", model: "llama" };
  assert.equal(local.requiresKey, false);
  assert.equal(draftProblem(local), null);
  // A new provider shares nothing beyond the message and history until allowed.
  assert.deepEqual(newProvider("cloud").sharing, { history: true, memories: false, documents: false });
});
