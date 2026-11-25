// Task 03-B: response continuation, recommended settings, PDF/file results and GFM tables.
// Synthetic fixtures only; no Tauri or model involvement.
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Markdown from "react-markdown";
import {
  errorGuidance, pdfAnswerScope, pdfCoverage, pdfView, pdfWarnings, resultView, searchFilterLabel,
} from "./agent.ts";
import {
  commandError, continuationCandidate, continuationErrorCopy, latestChatAttempt, outputBudgetNote, responseNotice,
  settingChanges,
} from "./chat.ts";
import { offlineMarkdown } from "./markdown.ts";
import { agentTurnTaskIds, applyChatEventToTurns } from "./unified.ts";

const stats = (extra = {}) => ({
  reason: "token_limit", promptTokens: 40, promptTokensPerSecond: null, generatedTokens: 256,
  timeToFirstTokenMs: null, elapsedMs: 10, tokensPerSecond: 1, excludedTurns: 0, ...extra,
});
const message = (id, role, state, content, extra = {}) => (
  { id, position: 1, role, state, content, createdAt: 1, stats: null, ...extra }
);
const chatTurn = (id, user, assistants) => ({
  id: String(id), kind: "chat", user, assistants: assistants.map((item) => ({ message: item, modelName: "m.gguf" })),
  taskId: null, taskTimeoutMs: null, taskState: null, taskError: null, taskErrorMessage: null,
  excludedTurns: null, nativeResultsAvailable: false,
});
const success = (operationId, tool, result) => ({
  record: { operationId, tool, status: "succeeded", error: null },
  observation: { tool, status: "success", result },
});
const render = (text) => renderToStaticMarkup(createElement(Markdown, offlineMarkdown, text));

test("only a token-limited assistant reply in the newest Chat turn is a continuation candidate", () => {
  const cut = message(125, "assistant", "truncated", "Here is the beginning", { stats: stats() });
  assert.equal(continuationCandidate(cut), true);
  assert.equal(continuationCandidate({ ...cut, stats: stats({ reason: "end_of_generation" }) }), false);
  assert.equal(continuationCandidate({ ...cut, state: "complete" }), false);
  assert.equal(continuationCandidate({ ...cut, stats: null }), false);
  assert.equal(continuationCandidate(null), false);

  const user = message(124, "user", "complete", "Explain");
  assert.equal(latestChatAttempt([chatTurn(124, user, [cut])])?.id, 125);
  const agentTurn = { ...chatTurn(130, user, []), kind: "agent" };
  assert.equal(latestChatAttempt([chatTurn(124, user, [cut]), agentTurn]), null);
});

test("a continuation updates the same assistant message without duplicating saved text", () => {
  const user = message(124, "user", "complete", "Explain");
  const earlier = message(123, "assistant", "complete", "Old attempt");
  const cut = message(125, "assistant", "truncated", "Here is the beginning", { stats: stats() });
  const later = chatTurn(200, message(200, "user", "complete", "Next"), [message(201, "assistant", "complete", "Done")]);
  let turns = [chatTurn(124, user, [earlier, cut]), later];

  // `started` carries no user and the saved prefix; it targets the attempt by ID, not the latest turn.
  turns = applyChatEventToTurns(turns, {
    kind: "started", user: null,
    assistant: { ...cut, state: "streaming", stats: null },
  }, "other.gguf");
  turns = applyChatEventToTurns(turns, { kind: "delta", id: 125, text: "… and the rest." }, null);
  turns = applyChatEventToTurns(turns, { kind: "delta", id: 999, text: "stray" }, null);

  assert.equal(turns.length, 2);
  assert.equal(turns[0].assistants.length, 2);
  assert.equal(turns[0].assistants[1].message.content, "Here is the beginning… and the rest.");
  assert.equal(turns[0].assistants[1].modelName, "m.gguf");
  assert.equal(turns[0].assistants[0].message.content, "Old attempt");
  assert.deepEqual(turns[1], later);
});

test("continuation reasoning stays split when Thinking resumes in a later segment", async () => {
  const { splitReasoning } = await import("./chat.ts");
  let turns = [chatTurn(1, message(1, "user", "complete", "Q"), [])];
  turns = applyChatEventToTurns(turns, { kind: "started", user: null,
    assistant: message(2, "assistant", "streaming", "<think>\nPartial reasoning") }, null);
  turns = applyChatEventToTurns(turns, { kind: "delta", id: 2, text: " continues</think>\n\n**Answer**" }, null);
  const split = splitReasoning(turns[0].assistants[0].message.content, false);
  assert.equal(split.reasoning, "Partial reasoning continues");
  assert.equal(split.answer, "**Answer**");
});

test("continuation errors map to specific copy and the notice offers Continue only when eligible", () => {
  assert.match(continuationErrorCopy({ code: "context_limit", message: "" }).title, /context/i);
  assert.match(continuationErrorCopy({ code: "no_model", message: "Load the original model to continue this response." }).detail,
    /original model/);
  const legacy = continuationErrorCopy({ code: "invalid_config", message: "This response can't be continued" });
  assert.match(legacy.detail, /^This response can't be continued\. .*regenerated/);
  assert.equal(continuationErrorCopy({ code: "storage", message: "" }).title, "Couldn't continue the response");
  assert.deepEqual(commandError({ code: "context_limit", message: "x" }), { code: "context_limit", message: "x" });
  assert.deepEqual(commandError("plain"), { code: "unknown", message: "plain" });

  const cut = message(5, "assistant", "truncated", "Partial answer", { stats: stats() });
  assert.match(responseNotice(cut, 256, true).detail, /Continue generating/);
  assert.doesNotMatch(responseNotice(cut, 256, false).detail, /Continue/);
});

test("the effective output budget is explained only when it is below the requested ceiling", () => {
  assert.equal(outputBudgetNote(stats({ outputBudget: { requested: 4096, effective: 4096, contextLength: 8192 } })), null);
  assert.equal(outputBudgetNote(stats()), null);
  assert.equal(
    outputBudgetNote(stats({ outputBudget: { requested: 4096, effective: 1536, contextLength: 8192 } })),
    "1,536 of 4,096 output tokens fit after the prompt and history in the 8,192-token context",
  );
});

test("recommended settings are listed as explicit changes with reload requirements", () => {
  const saved = { contextLength: 4096, threads: 4, maxTokens: 256, temperature: 0.7, topP: 0.8, topK: 20, enableThinking: false };
  const next = { ...saved, contextLength: 8192, maxTokens: 4096 };
  assert.deepEqual(settingChanges(saved, next), [
    { key: "contextLength", label: "Context", reload: true, from: "4,096", to: "8,192" },
    { key: "maxTokens", label: "Output limit", reload: false, from: "256", to: "4,096" },
  ]);
  assert.deepEqual(settingChanges(saved, saved), []);
});

test("GFM tables render inside a scroll region and offline restrictions still apply", () => {
  const html = render("| File | Pages |\n| --- | ---: |\n| alpha.pdf | 2 |\n\nSee https://example.com and <b>raw</b> ![x](https://e/x.png)");
  assert.match(html, /<div class="markdown-table" tabindex="0" role="region" aria-label="Table"><table>/);
  assert.match(html, /<th>File<\/th>/);
  assert.match(html, /<td style="text-align:right">2<\/td>/);
  assert.doesNotMatch(html, /href=|<img|<b>/);
  assert.match(html, /<span class="markdown-link">https:\/\/example.com<\/span>/);

  const invalid = render("| File | Pages |\n| alpha.pdf | 2 |");
  assert.doesNotMatch(invalid, /<table/);
  assert.match(invalid, /^<p>/);
});

test("native PDF results are validated and show explicit page coverage", () => {
  const result = {
    page_count: 5, first_page: 2, last_page: 3, file_bytes: 918, truncated: true,
    pages: [
      { page_number: 2, text: "Synthetic page two.", truncated: false },
      { page_number: 3, text: "Synthetic page three.", truncated: true },
    ],
  };
  const view = resultView(success("op_1", "files.read", result));
  assert.equal(view.kind, "pdf");
  assert.equal(pdfCoverage(view.pdf), "Pages 2–3 of 5");
  assert.equal(pdfCoverage({ pageCount: 1, firstPage: 1, lastPage: 1 }), "Page 1 of 1");
  assert.deepEqual(pdfWarnings(view.pdf).map((item) => item.kind), ["omitted", "clipped"]);
  assert.match(pdfWarnings(view.pdf)[0].text, /^3 of 5 pages were not read/);

  // Out-of-range, unordered or malformed pages are not reinterpreted.
  assert.equal(pdfView({ ...result, pages: [{ page_number: 4, text: "x" }] }), null);
  assert.equal(pdfView({ ...result, pages: [result.pages[1], result.pages[0]] }), null);
  assert.equal(pdfView({ ...result, last_page: 6 }), null);
  assert.equal(resultView(success("op_2", "files.read", { ...result, page_count: "5" })).kind, "unrecognized");

  // TXT results keep their existing shape.
  assert.equal(resultView(success("op_3", "files.read", { content: "Note", file_bytes: 4, truncated: false })).kind, "file-read");

  const whole = { ...result, page_count: 2, first_page: 1, last_page: 2, truncated: false,
    pages: [{ page_number: 1, text: "One", truncated: false }, { page_number: 2, text: "Two", truncated: false }] };
  assert.deepEqual(pdfWarnings(pdfView(whole)), []);
  assert.equal(pdfAnswerScope([success("a", "files.read", whole)]), null);
  assert.match(pdfAnswerScope([success("b", "files.read", result)]), /Pages 2–3 of 5/);
  assert.equal(pdfAnswerScope([success("c", "math.calculate", { value: 1 })]), null);
});

test("file searches report the applied extension filter and PDF errors have specific guidance", () => {
  const view = resultView(success("op", "files.search", { paths: ["a/notes.PDF", "b.pdf"], extension: "pdf", truncated: false }));
  assert.deepEqual(view, { kind: "file-search", paths: ["a/notes.PDF", "b.pdf"], extension: "pdf", truncated: false });
  assert.equal(searchFilterLabel("pdf"), "Extension filter: .pdf (final suffix, any letter case)");
  assert.equal(searchFilterLabel(null), "No extension filter");
  assert.equal(resultView(success("op", "files.search", { paths: [], truncated: false })).extension, null);

  assert.match(errorGuidance({ code: "pdf_no_text", message: "" }).detail, /OCR/);
  assert.match(errorGuidance({ code: "pdf_encrypted", message: "" }).title, /encrypted/);
  assert.match(errorGuidance({ code: "pdf_limit", message: "" }).detail, /four pages/);
});

test("conversation load fetches journals only for turns with session-held native results", () => {
  const agent = (id, taskId, nativeResultsAvailable) => ({
    ...chatTurn(id, message(id, "user", "complete", "x"), []), kind: "agent", taskId, nativeResultsAvailable,
  });
  const conversation = { id: 1, title: "", systemPrompt: "", createdAt: 0, updatedAt: 0,
    turns: [agent(1, "11", false), agent(2, "12", true), agent(3, null, true)] };
  assert.deepEqual(agentTurnTaskIds(conversation), ["11", "12"]);
  assert.deepEqual(agentTurnTaskIds(conversation, { withNativeResults: true }), ["12"]);
});
