import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Markdown from "react-markdown";
import {
  applyChatEvent, chatRequestOptions, continuationCandidate, continuationErrorCopy,
} from "./chat.ts";
import {
  citationState, contextUsageLabel, coverageLines, coverageSummary, createLatest, embeddingCopy, preparationLabel, documentIndexState, documentSummary,
  importCancellable, importInDialog, importOutcomeNotice, importTerminal, indexPercent, indexTerminal, isContextual,
  knowledgeErrorCopy, knowledgeOption, knowledgeQueryValid, knowledgeReadiness, memoryInputError, memoryQueryValid,
  mergePage, ownsChatEvent, pruneSelection, remarkCitations, sourceLabel, splitCitations, textCoverageNote,
} from "./knowledge.ts";
import { offlineMarkdown } from "./markdown.ts";
import { referenceProfile } from "./models.ts";
import { applyChatEventToTurns } from "./unified.ts";

const doc = (id, overrides = {}) => ({
  id, name: `doc-${id}.txt`, format: "txt", fileBytes: 2048, textBytes: 2000, chunkCount: 2,
  indexingStatus: "ready", pdfCoverage: null, createdAt: 1, updatedAt: 1, ...overrides,
});
const ready = { supported: true, available: true, name: "bge", sha256: "x", preprocessing: 1, dimensions: 384, contextTokens: 512, cpuThreads: 2 };
const selection = (overrides = {}) => ({ enabled: true, mode: "keyword", documentIds: [1, 2], ...overrides });

// ---------------------------------------------------------------- lifecycle
test("import phases settle, offer cancellation only while processing, and describe outcomes", () => {
  for (const phase of ["selecting", "confirming"]) {
    assert.equal(importInDialog({ phase }), true);
    assert.equal(importCancellable({ phase }), false);
  }
  for (const phase of ["reading", "extracting", "indexing"]) assert.equal(importCancellable({ phase }), true);
  for (const phase of ["committing", "cancelling"]) assert.equal(importCancellable({ phase }), false);
  assert.equal(importTerminal({ phase: "completed", outcome: { kind: "imported", documentId: 3 } }), true);
  assert.equal(importTerminal({ phase: "failed", error: { code: "pdf_invalid", message: "" } }), true);
  assert.equal(importTerminal({ phase: "cancelled" }), true);
  assert.equal(importTerminal({ phase: "committing" }), false);
  assert.match(importOutcomeNotice({ kind: "replaced", documentId: 3 }, "plan.md"), /“plan\.md”.*citations to the previous version are now unavailable/);
  assert.match(importOutcomeNotice({ kind: "duplicate", documentId: 3 }, null), /already in your library/);
  assert.equal(knowledgeErrorCopy({ code: "pdf_encrypted", message: "" }).title, "The PDF couldn't be read");
});

test("index progress is bounded and terminal phases stop polling", () => {
  assert.equal(indexPercent({ processedChunks: 0, totalChunks: 0 }), null);
  assert.equal(indexPercent({ processedChunks: 3, totalChunks: 4 }), 75);
  assert.equal(indexPercent({ processedChunks: 9, totalChunks: 4 }), 100);
  for (const phase of ["completed", "cancelled", "failed"]) assert.equal(indexTerminal(phase), true);
  for (const phase of ["indexing", "cancelling", "committing"]) assert.equal(indexTerminal(phase), false);
});

test("document metadata reports missing, incompatible and partial coverage accurately", () => {
  assert.equal(documentIndexState({ indexedDocuments: 1, missingDocuments: 0, incompatibleDocuments: 0 }), "indexed");
  assert.equal(documentIndexState({ indexedDocuments: 0, missingDocuments: 0, incompatibleDocuments: 1 }), "incompatible");
  assert.equal(documentIndexState({ indexedDocuments: 0, missingDocuments: 1, incompatibleDocuments: 0 }), "missing");
  assert.equal(documentIndexState(null), null);
  const pdf = doc(4, { format: "pdf", indexingStatus: "partial", pdfCoverage: { pageCount: 140, pagesExamined: 100, pagesWithoutText: 2 } });
  assert.equal(documentSummary(pdf), "PDF · 140 pages · 2 KB");
  assert.match(textCoverageNote(pdf), /^Only the first 100 pages of 140 were read and 2 pages had no selectable text/);
  assert.equal(textCoverageNote(doc(5)), null);
  assert.equal(textCoverageNote(doc(6, { indexingStatus: "partial" })), "Some of this document's text couldn't be read.");
  assert.equal(sourceLabel({ kind: "page", page: 2 }), "Page 2");
  assert.equal(sourceLabel({ kind: "lines", startLine: 4, endLine: 4 }), "Line 4");
  assert.equal(sourceLabel({ kind: "lines", startLine: 4, endLine: 9 }), "Lines 4–9");
});

// ---------------------------------------------------------------- ownership
test("only the newest request token is current", () => {
  const gate = createLatest();
  const first = gate.next();
  const second = gate.next();
  assert.equal(gate.isCurrent(first), false);
  assert.equal(gate.isCurrent(second), true);
  gate.invalidate();
  assert.equal(gate.isCurrent(second), false);
  assert.equal(ownsChatEvent({ generation: 3, conversationId: 7 }, { generation: 3, conversationId: 7 }), true);
  assert.equal(ownsChatEvent({ generation: 3, conversationId: 7 }, { generation: 4, conversationId: 7 }), false);
  assert.equal(ownsChatEvent({ generation: 3, conversationId: 7 }, { generation: 3, conversationId: 8 }), false);
});

test("knowledge events never create or alter saved turns", () => {
  const event = { kind: "knowledge", coverage: { mode: "keyword", embeddings: null, partialDocumentIds: [], unavailableDocumentIds: [], retrieved: 2 } };
  const turns = [];
  assert.equal(applyChatEventToTurns(turns, event, null), turns);
  const messages = [];
  assert.equal(applyChatEvent(messages, event), messages);
});

// ---------------------------------------------------------------- selection
test("the request option keeps the explicit selection and never widens it", () => {
  assert.equal(knowledgeOption(selection({ enabled: false })), null);
  assert.equal(knowledgeOption(selection({ documentIds: [] })), null);
  assert.deepEqual(knowledgeOption(selection({ mode: "hybrid", documentIds: [2, 2, 0, -1, 1.5, 9] })), { mode: "hybrid", documentIds: [2, 9] });
  assert.equal(knowledgeOption(selection({ documentIds: Array.from({ length: 129 }, (_, i) => i + 1) })), null);
  const ids = [3, 4];
  const options = chatRequestOptions({ thinking: false, supportsThinking: false, knowledge: { mode: "semantic", documentIds: ids } });
  assert.deepEqual(options, { enableThinking: false, knowledge: { mode: "semantic", documentIds: [3, 4] } });
  assert.notEqual(options.knowledge.documentIds, ids);
  assert.equal("knowledge" in chatRequestOptions({ thinking: false, supportsThinking: false }), false);
});

test("deleted documents leave the selection without adding others", () => {
  const ids = [1, 2];
  assert.equal(pruneSelection(ids, [doc(1), doc(2), doc(3)]), ids);
  assert.deepEqual(pruneSelection(ids, [doc(2), doc(3)]), [2]);
  assert.deepEqual(pruneSelection(ids, []), []);
});

test("readiness blocks missing models and indexes without changing the mode", () => {
  assert.equal(knowledgeReadiness({ selection: selection({ enabled: false }), embedding: null, coverage: null }).blocked, null);
  assert.equal(knowledgeReadiness({ selection: selection({ documentIds: [] }), embedding: ready, coverage: null }).blocked.fix, "select");
  assert.equal(knowledgeReadiness({ selection: selection(), embedding: null, coverage: null }).blocked, null);
  const semantic = selection({ mode: "semantic", grounding: "documents" });
  assert.equal(knowledgeReadiness({ selection: semantic, embedding: { ...ready, available: false }, coverage: null }).blocked.fix, "model");
  // Automatic grounding explains the missing model without blocking Chat.
  const automatic = knowledgeReadiness({ selection: selection({ mode: "semantic" }), embedding: { ...ready, available: false }, coverage: null });
  assert.equal(automatic.blocked, null);
  assert.match(automatic.notes[0], /Word matches are used instead/);
  assert.equal(knowledgeReadiness({
    selection: semantic, embedding: ready, coverage: { indexedDocuments: 0, missingDocuments: 1, incompatibleDocuments: 1 },
  }).blocked.fix, "index");
  const partial = { indexedDocuments: 1, missingDocuments: 1, incompatibleDocuments: 0 };
  const dense = knowledgeReadiness({ selection: semantic, embedding: ready, coverage: partial });
  assert.equal(dense.blocked, null);
  assert.match(dense.notes[0], /won't be searched/);
  assert.match(knowledgeReadiness({ selection: selection({ mode: "hybrid" }), embedding: ready, coverage: partial }).notes[0], /only word matches/);
  assert.equal(semantic.mode, "semantic");
});

test("coverage lines report partial, unavailable, unindexed and unfitted evidence", () => {
  const names = (id) => (id === 1 ? "orchard.pdf" : null);
  const lines = coverageLines({
    mode: "hybrid", embeddings: { indexedDocuments: 1, missingDocuments: 1, incompatibleDocuments: 0 },
    partialDocumentIds: [1], unavailableDocumentIds: [5], retrieved: 4,
  }, { sources: 2, memories: 1 }, names);
  assert.deepEqual(lines, [
    "4 passages matched; 2 fit in the model's context.",
    "Only part of the text was searchable in “orchard.pdf”.",
    "1 selected document is no longer in the library.",
    "1 document had no usable semantic index, so only word matches were used for it.",
  ]);
  assert.deepEqual(coverageLines({ mode: "keyword", embeddings: null, partialDocumentIds: [], unavailableDocumentIds: [], retrieved: 0 }, { sources: 0, memories: 0 }, names),
    ["No passages in the selected documents matched this question."]);
  assert.deepEqual(coverageLines(null, { sources: 1, memories: 0 }, names), []);
  const routed = (extra) => ({ mode: "keyword", embeddings: null, partialDocumentIds: [], unavailableDocumentIds: [], retrieved: 0, ...extra });
  assert.match(coverageLines(routed({ intent: "general" }), null, names)[0], /answered as ordinary Chat/);
  assert.deepEqual(coverageLines(routed({ intent: "search" }), null, names), ["The search text wasn't found in the selected documents."]);
  assert.deepEqual(coverageLines(routed({
    intent: "overview", retrieved: 4, overview: [{ documentId: 1, suppliedChunks: 4, totalChunks: 10 }],
  }), { sources: 4, memories: 0 }, names), ["Overview read 4 of 10 passages, spread across the selected documents."]);
  assert.match(coverageLines(routed({ mode: "keyword", requestedMode: "semantic", intent: "question", retrieved: 1 }), null, names)[0],
    /Semantic search wasn't available/);
  assert.equal(contextUsageLabel({ sources: 2, memories: 1 }), "Supplied to the model: 2 passages · 1 memory");
  assert.equal(contextUsageLabel({ sources: 0, memories: 3 }), "Supplied to the model: 3 memories");
  assert.equal(contextUsageLabel({ sources: 0, memories: 0 }), "Supplied to the model: 0 passages");
  assert.equal(contextUsageLabel(null), null);
  assert.equal(knowledgeQueryValid("  "), false);
  assert.equal(knowledgeQueryValid("--"), false);
  assert.equal(knowledgeQueryValid("é".repeat(129)), false);
  assert.equal(knowledgeQueryValid("gate code"), true);
});

// ----------------------------------------------------------------- sources
test("only citation markers returned by the host are linked", () => {
  const valid = new Set(["S1", "S2"]);
  assert.deepEqual(splitCitations("A [S1] and [S1, S2] but [S3] and [S2, S4].", valid), [
    "A ", { ids: ["S1"], raw: "[S1]" }, " and ", { ids: ["S1", "S2"], raw: "[S1, S2]" }, " but [S3] and [S2, S4].",
  ]);
  assert.deepEqual(splitCitations("No markers", valid), ["No markers"]);
  assert.deepEqual(splitCitations("[S1]", new Set()), ["[S1]"]);
});

test("citations render through safe Markdown while code, unknown markers and links stay plain", () => {
  const render = (text, ids) => renderToStaticMarkup(createElement(Markdown, {
    ...offlineMarkdown,
    remarkPlugins: [...offlineMarkdown.remarkPlugins, [remarkCitations, { ids: new Set(ids) }]],
    components: {
      ...offlineMarkdown.components,
      span: ({ node: _node, children, ...props }) => (props["data-citation"]
        ? createElement("button", { "data-cite": props["data-citation"] }, children)
        : createElement("span", props, children)),
    },
  }, text));
  const html = render("Gate code FERN-742 [S1]. Fake [S9]. `[S1]` and [link [S1]](https://example.com)\n\n<b>[S1]</b>", ["S1"]);
  // The prose marker and the marker left after raw HTML is skipped; not code or link text.
  assert.equal((html.match(/data-cite="S1"/g) ?? []).length, 2);
  assert.match(html, /<span class="markdown-link">link \[S1\]<\/span>/);
  assert.match(html, /Fake \[S9\]/);
  assert.match(html, /<code>\[S1\]<\/code>/);
  assert.doesNotMatch(html, /<b>|href=/);
  assert.equal(render("Answer [S1].", []).includes("data-cite"), false);
});

test("deleted or replaced sources are unavailable and never resolved", () => {
  assert.equal(citationState({ available: false }), "unavailable");
  assert.equal(citationState({ available: false }, "stale excerpt"), "unavailable");
  assert.equal(citationState({ available: true }), "resolve");
  assert.equal(citationState({ available: true }, null), "unavailable");
  assert.equal(citationState({ available: true }, "FERN-742"), "ready");
});

// ------------------------------------------------------------------ memory
test("memory input respects the trimmed 1–1,024 UTF-8 byte limit", () => {
  assert.equal(memoryInputError("   "), "Enter something to remember.");
  assert.equal(memoryInputError(` ${"a".repeat(1024)} `), null);
  assert.match(memoryInputError("a".repeat(1025)), /1,024 bytes/);
  assert.equal(memoryInputError("é".repeat(512)), null);
  assert.match(memoryInputError("é".repeat(513)), /bytes/);
  assert.equal(memoryQueryValid("x".repeat(256)), true);
  assert.equal(memoryQueryValid("x".repeat(257)), false);
  assert.equal(knowledgeErrorCopy({ code: "memory_limit", message: "" }).title, "You've reached the 512-memory limit");
});

test("memory pages merge in descending ID order without duplicates", () => {
  const memory = (id) => ({ id, content: `m${id}`, createdAt: id, updatedAt: id });
  assert.deepEqual(mergePage([memory(9), memory(7)], [memory(7), memory(4), memory(2)]).map(({ id }) => id), [9, 7, 4, 2]);
});

// ------------------------------------------------------------ continuation
test("contextual answers offer Regenerate while ordinary truncated answers keep Continue", () => {
  const truncated = (stats) => ({ id: 5, position: 1, role: "assistant", state: "truncated", content: "Part", createdAt: 1, stats });
  const base = { reason: "token_limit", promptTokens: 1, promptTokensPerSecond: null, generatedTokens: 1, timeToFirstTokenMs: null, elapsedMs: 1, tokensPerSecond: 1, excludedTurns: 0 };
  const ordinary = truncated(base);
  const contextual = truncated({ ...base, contextUsage: { sources: 1, memories: 0 } });
  const memoryOnly = truncated({ ...base, contextUsage: { sources: 0, memories: 2 } });
  assert.equal(continuationCandidate(ordinary) && !isContextual(ordinary), true);
  assert.equal(continuationCandidate(contextual) && isContextual(contextual), true);
  assert.equal(isContextual(memoryOnly), true);
  assert.equal(isContextual({ ...ordinary, stats: null }), false);
  assert.match(continuationErrorCopy({ code: "contextual", message: "" }).detail, /Regenerate searches again with your current knowledge settings/);
  assert.match(continuationErrorCopy({ code: "invalid_config", message: "" }).detail, /documents or memory/);
});

test("durable contextual metadata identifies failed attempts without stats", () => {
  assert.equal(isContextual({ context: { coverage: null, memoryEnabled: true, fitted: null }, stats: null }), true);
});

// ------------------------------------------------------------ Task 06-B
test("embedding status reflects remembered selection, restore, unload, missing and incompatible files", () => {
  const status = (extra) => ({ ...ready, available: false, ...extra });
  assert.equal(embeddingCopy(null, null).tone, "busy");
  assert.equal(embeddingCopy(status({ supported: false }), null).tone, "unsupported");
  assert.equal(embeddingCopy(status({ restore: "pending" }), null).label, "Restoring…");
  const restored = embeddingCopy({ ...ready, restore: "restored" }, null);
  assert.deepEqual([restored.tone, restored.label, restored.action], ["ready", "Ready · restored at launch", "unload"]);
  assert.equal(embeddingCopy({ ...ready, restore: "selected" }, null).label, "Ready · remembered for next launch");
  assert.equal(embeddingCopy(status({ restore: "unloaded" }), null).action, "choose");
  assert.match(embeddingCopy(status({ restore: "missing" }), null).detail, /disconnected drive/);
  assert.equal(embeddingCopy(status({ restore: "incompatible" }), null).tone, "error");
  assert.doesNotMatch(embeddingCopy(status({ restore: "not_remembered" }), null).detail, /session/);
  assert.equal(embeddingCopy(ready, "choosing").label, "Verifying and loading…");
});

test("coverage headlines name the route, exact search results and overview coverage", () => {
  const base = { mode: "keyword", embeddings: null, partialDocumentIds: [], unavailableDocumentIds: [], retrieved: 0 };
  assert.equal(coverageSummary(null), null);
  assert.equal(coverageSummary({ ...base, intent: "general" }).title, "Ordinary Chat · no document evidence");
  assert.equal(coverageSummary({ ...base, intent: "search", retrieved: 2 }).title, "Exact search · 2 matches");
  assert.equal(coverageSummary({ ...base, intent: "search" }).title, "Exact search · no match");
  const partial = coverageSummary({ ...base, intent: "overview", retrieved: 4, overview: [{ documentId: 1, suppliedChunks: 4, totalChunks: 12 }] });
  assert.deepEqual([partial.title, partial.tone], ["Partial overview · 4 of 12 passages", "warning"]);
  assert.equal(coverageSummary({ ...base, intent: "overview", retrieved: 3, overview: [{ documentId: 1, suppliedChunks: 3, totalChunks: 3 }] }).title,
    "Overview · all 3 passages");
  const fallback = coverageSummary({ ...base, intent: "question", retrieved: 1, requestedMode: "semantic" });
  assert.equal(fallback.detail, "Semantic requested · Keyword used");
  assert.equal(coverageSummary({ ...base, mode: "hybrid", intent: "question", retrieved: 2 }).detail, "Hybrid retrieval");
  // Legacy attempts without an intent keep the Task 05 wording.
  assert.equal(coverageSummary({ ...base, retrieved: 0 }).title, "No relevant passages");
  assert.equal(preparationLabel({ ...base, intent: "general" }), "No relevant passages. Answering as ordinary Chat…");
  assert.equal(preparationLabel({ ...base, intent: "overview", retrieved: 4 }), "Reading 4 passages for an overview…");
  // Headline lines are not repeated in the detail list.
  assert.deepEqual(coverageLines({ ...base, intent: "general" }, null, () => null, true), []);
});

test("readiness waits for a pending restore and only blocks strict grounding", () => {
  const pending = { ...ready, available: false, restore: "pending" };
  const semantic = selection({ mode: "semantic" });
  assert.equal(knowledgeReadiness({ selection: semantic, embedding: pending, coverage: null }).blocked, null);
  assert.equal(knowledgeReadiness({ selection: { ...semantic, grounding: "documents" }, embedding: pending, coverage: null }).blocked.fix, "model");
});

test("only the bundled row is labeled built-in", () => {
  assert.equal(referenceProfile({ fileName: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf", bundled: true }).builtIn, true);
  const copy = referenceProfile({ fileName: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf", bundled: false });
  assert.deepEqual([copy.builtIn, copy.role], [false, "Imported lightweight assistant"]);
  assert.equal(referenceProfile({ fileName: "custom.gguf", bundled: false }), null);
});
