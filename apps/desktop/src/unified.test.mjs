import assert from "node:assert/strict";
import test from "node:test";
import {
  agentTurnDisplay, agentTurnTaskIds, applyChatEventToTurns, applyTurnEvent, canRegenerateLatest,
  contextOmissionNote, createLiveTurn, ownsEvent, permissionGuidance, sameIdentity, scopeContext,
} from "./unified.ts";

// Decimal-string identifiers above Number.MAX_SAFE_INTEGER must survive untouched.
const owner = { conversationId: 17, turnId: "9007199254740993", taskId: "9007199254740995" };
const message = (id, role, state, content) => ({ id, position: 0, role, state, content, createdAt: 1, stats: null });
const success = (operationId, tool, result) => ({
  record: { operationId, tool, status: "succeeded", error: null },
  observation: { tool, status: "success", result },
});
const scoped = (identity, event, excludedTurns = 0) => ({
  conversationId: identity.conversationId, turnId: identity.turnId, excludedTurns,
  event: { taskId: identity.taskId, ...event },
});
let key = 0;
const entry = () => ({ key: ++key, at: 1_000 + key, perf: 10 + key });

test("events are owned only when conversation, turn and task all match", () => {
  assert.equal(ownsEvent(owner, scoped(owner, { kind: "state_changed", state: "planning" })), true);
  assert.equal(ownsEvent(owner, scoped({ ...owner, conversationId: 18 }, { kind: "state_changed", state: "planning" })), false);
  assert.equal(ownsEvent(owner, scoped({ ...owner, turnId: "9007199254740994" }, { kind: "state_changed", state: "planning" })), false);
  assert.equal(ownsEvent(owner, scoped({ ...owner, taskId: "9007199254740996" }, { kind: "state_changed", state: "planning" })), false);
  assert.equal(ownsEvent(null, scoped(owner, { kind: "state_changed", state: "planning" })), false);
  assert.equal(sameIdentity(owner, { ...owner }), true);
  assert.equal(sameIdentity(owner, { ...owner, taskId: "9007199254740994" }), false);
});

test("a live turn ignores other turns, deduplicates observations and keeps the largest omission", () => {
  const other = { conversationId: 18, turnId: "40", taskId: "41" };
  let turn = createLiveTurn(owner, "Read the first matching file.", 300_000, 1, 1);
  const read = success("session_op_8", "files.read", { content: "Synthetic note", file_bytes: 14, truncated: false });
  turn = applyTurnEvent(turn, scoped(owner, { kind: "started", taskTimeoutMs: 300_000 }, 0), entry());
  turn = applyTurnEvent(turn, scoped(owner, { kind: "observation", ...read }, 2), entry());
  turn = applyTurnEvent(turn, scoped(owner, { kind: "observation", ...read }, 1), entry());
  const before = turn;
  turn = applyTurnEvent(turn, scoped(other, { kind: "observation", ...success("x", "math.calculate", { value: 1 }) }), entry());
  assert.equal(turn, before, "an unrelated conversation's event must not change this turn");
  assert.equal(turn.nativeResults.length, 1);
  assert.equal(turn.excludedTurns, 2);
  assert.equal(turn.id, owner.taskId);
  assert.equal(turn.turnId, owner.turnId);
});

test("native results remain after model synthesis fails in the same turn", () => {
  let turn = createLiveTurn(owner, "Find ledger files.", 300_000, 1, 1);
  const search = success("op_1", "files.search", { paths: ["alpha_ledger.txt"], truncated: true });
  turn = applyTurnEvent(turn, scoped(owner, { kind: "observation", ...search }), entry());
  turn = applyTurnEvent(turn, scoped(owner, {
    kind: "finished", state: "failed", answer: null, executions: [search.record], journalIncomplete: false,
    usage: { toolCalls: 1, modelCalls: 2, retries: 0, generatedTokens: 3 },
    error: { code: "model_context_limit", message: "The task context exceeded the loaded model context." },
  }), entry());
  const saved = {
    id: owner.turnId, kind: "agent", user: message(1, "user", "complete", "Find ledger files."),
    assistants: [{ message: message(2, "assistant", "failed", ""), modelName: "local.gguf" }],
    taskId: owner.taskId, taskTimeoutMs: 300_000, nativeResultsAvailable: true,
  };
  const display = agentTurnDisplay(saved, turn, undefined, { running: false, loadedAt: 0 });
  assert.equal(display.nativeResults.length, 1);
  assert.equal(display.answer, null);
  assert.equal(display.error.code, "model_context_limit");
  assert.equal(display.state, "failed");
});

test("streamed chat events build their own turn and never touch Agent turns", () => {
  const agentTurn = {
    id: "10", kind: "agent", user: message(10, "user", "complete", "Calculate"),
    assistants: [{ message: message(11, "assistant", "complete", "12193"), modelName: null }],
    taskId: "5", taskTimeoutMs: 300_000, nativeResultsAvailable: false,
  };
  const user = message(12, "user", "complete", "Is that above 12,000?");
  let turns = applyChatEventToTurns([agentTurn], { kind: "started", user, assistant: message(13, "assistant", "streaming", "") }, "m.gguf");
  turns = applyChatEventToTurns(turns, { kind: "delta", id: 13, text: "Yes" }, "m.gguf");
  turns = applyChatEventToTurns(turns, { kind: "delta", id: 11, text: " stale" }, "m.gguf");
  assert.deepEqual(turns.map((turn) => [turn.id, turn.kind]), [["10", "agent"], ["12", "chat"]]);
  assert.equal(turns[0].assistants[0].message.content, "12193");
  assert.equal(turns[1].assistants[0].message.content, "Yes");
  assert.equal(turns[1].assistants[0].modelName, "m.gguf");

  // A regeneration appends an attempt under the latest Chat turn.
  const retry = message(14, "assistant", "streaming", "");
  turns = applyChatEventToTurns(turns, { kind: "started", user: null, assistant: retry }, "m.gguf");
  assert.deepEqual(turns[1].assistants.map((attempt) => attempt.message.id), [13, 14]);
  assert.equal(canRegenerateLatest({ turns }), true);
  assert.equal(canRegenerateLatest({ turns: [turns[1], agentTurn] }), false, "no regeneration for an Agent turn");
  assert.equal(canRegenerateLatest(null), false);
  assert.deepEqual(agentTurnTaskIds({ id: 1, title: "", systemPrompt: "", createdAt: 0, updatedAt: 0, turns }), ["5"]);
});

test("saved Agent outcome and omitted context display without a journal detail", () => {
  const turn = {
    id: "12", kind: "agent", user: message(12, "user", "complete", "Read the file"),
    assistants: [{ message: message(13, "assistant", "failed", ""), modelName: null }],
    taskId: "5", taskTimeoutMs: 300_000, nativeResultsAvailable: false,
    taskState: "denied", taskError: "permission_denied", taskErrorMessage: "Access was denied.", excludedTurns: 4,
  };
  const display = agentTurnDisplay(turn, undefined, undefined, { running: false, loadedAt: 100 });
  assert.equal(display.state, "denied");
  assert.deepEqual(display.error, { code: "permission_denied", message: "Access was denied." });
  assert.equal(display.excludedTurns, 4);
});

test("after reload, unavailable native payloads are not shown or invented", () => {
  const saved = {
    id: "124", kind: "agent", user: message(124, "user", "complete", "Read the selected file."),
    assistants: [{ message: message(125, "assistant", "complete", "The file says that rainwater is collected."), modelName: "local-model.gguf" }],
    taskId: "91", taskTimeoutMs: 300_000, nativeResultsAvailable: false,
  };
  const detail = {
    task: { id: "91", state: "completed", createdAt: 1, updatedAt: 2, finalized: true, usage: null, error: null },
    events: [], audit: [], nativeResults: [],
    executions: [{ operationId: "session_op_8", tool: "files.read", status: "succeeded", error: null, createdAt: 1, updatedAt: 2 }],
  };
  // A live entry that ended before this snapshot was read must defer to the backend flag.
  let live = createLiveTurn({ conversationId: 17, turnId: "124", taskId: "91" }, "Read", 300_000, 1, 1);
  live = { ...live, endedAt: 50, nativeResults: [success("session_op_8", "files.read", { content: "old", file_bytes: 3, truncated: false })] };
  const display = agentTurnDisplay(saved, live, detail, { running: false, loadedAt: 100 });
  assert.deepEqual(display.nativeResults, []);
  assert.equal(display.nativeUnavailable, true);
  assert.equal(display.answer, "The file says that rainwater is collected.");
  assert.equal(display.answerSource, "saved");

  // Before the snapshot is refreshed, a just-finished turn still shows its live results.
  const fresh = agentTurnDisplay(saved, { ...live, endedAt: 150 }, detail, { running: false, loadedAt: 100 });
  assert.equal(fresh.nativeResults.length, 1);
  assert.equal(fresh.nativeUnavailable, false);

  const interrupted = agentTurnDisplay(
    { ...saved, assistants: [{ message: message(125, "assistant", "interrupted", ""), modelName: null }] },
    undefined, undefined, { running: false, loadedAt: 100 });
  assert.equal(interrupted.state, "interrupted");
  assert.equal(interrupted.answer, null);
});

test("a kind mismatch never asks for an already granted resource again", () => {
  const scopes = [
    { id: "grant-1", displayName: "ledger.txt", kind: "file", access: "read_only", state: "active", remainingMs: 1000 },
    { id: "grant-2", displayName: "Old", kind: "directory", access: "read_only", state: "revoked", remainingMs: 0 },
  ];
  const context = scopeContext(scopes);
  assert.deepEqual(context, { files: ["ledger.txt"], folders: [] });
  const fileCase = permissionGuidance("scope_kind_mismatch", context);
  assert.match(fileCase.detail, /“ledger\.txt” is still selected/);
  assert.match(fileCase.detail, /read the selected file directly/);
  assert.doesNotMatch(fileCase.detail, /select (it|the file) again/i);
  assert.doesNotMatch(JSON.stringify(fileCase), /grant-1/);
  const folderCase = permissionGuidance("scope_kind_mismatch", { files: [], folders: ["Ledgers"] });
  assert.match(folderCase.detail, /search that folder/);
  const none = permissionGuidance("scope_kind_mismatch", { files: [], folders: [] });
  assert.match(none.detail, /no longer active/);
  assert.match(permissionGuidance("permission_denied", context).detail, /never grant access/);
  assert.equal(permissionGuidance("permission_denied", { files: [], folders: [] }), null);
  assert.equal(permissionGuidance("approval_declined", context), null);
});

test("context omission is reported accurately", () => {
  assert.equal(contextOmissionNote(0), null);
  assert.equal(contextOmissionNote(Number.NaN), null);
  assert.match(contextOmissionNote(1), /^1 earlier turn was left out/);
  assert.match(contextOmissionNote(3), /^3 earlier turns were left out.*They remain saved/);
});
