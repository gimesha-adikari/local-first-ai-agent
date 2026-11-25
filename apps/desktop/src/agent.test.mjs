import assert from "node:assert/strict";
import test from "node:test";
import {
  applyAgentEvent, createLiveTask, currentActivity, errorGuidance, executionLabel, mergeNativeResults,
  outcomeSummary, progressSteps, resultView, scopeSummary, successfulResults, usageFrom, validTimeout,
} from "./agent.ts";

test("a final answer is presented as a model reply, independent of native work", () => {
  const finished = { state: "completed", answer: "Here is an explanation.", error: null };
  assert.equal(outcomeSummary(finished, 0), "Model replied");
  assert.equal(outcomeSummary(finished, 1), "Model replied");
});

const record = (operationId, tool, status = "succeeded") => ({ operationId, tool, status, error: null });
const success = (operationId, tool, result) => ({
  record: record(operationId, tool), observation: { tool, status: "success", result },
});

function run(events, id = "9007199254740993") {
  let task = createLiveTask(id, "Find ledger files", 300_000, 1_000, 0);
  events.forEach((event, index) => {
    task = applyAgentEvent(task, { taskId: id, ...event }, { key: index, at: 2_000 + index, perf: 10 + index });
  });
  return task;
}

test("native results stay visible when model synthesis fails after a successful tool", () => {
  const search = success("op_1", "files.search", { paths: ["ledger.txt", "nested/budget ledger.txt"], truncated: true });
  const task = run([
    { kind: "started", taskTimeoutMs: 300_000 },
    { kind: "dispatch_started", operationId: "op_1", tool: "files.search" },
    { kind: "execution", record: search.record },
    { kind: "observation", ...search },
    // The terminal event omits nativeResults, as an older host or a missed merge might.
    {
      kind: "finished", state: "failed", answer: null,
      usage: { toolCalls: 1, modelCalls: 2, retries: 0, generatedTokens: 12 },
      error: { code: "model_context_limit", message: "The task context exceeded the loaded model context." },
      executions: [search.record], journalIncomplete: false,
    },
  ]);
  assert.equal(task.finished.state, "failed");
  assert.equal(task.finished.answer, null);
  assert.equal(successfulResults(task.nativeResults).length, 1);
  assert.deepEqual(resultView(task.nativeResults[0]), {
    kind: "file-search", paths: ["ledger.txt", "nested/budget ledger.txt"], truncated: true, extension: null,
  });
  assert.equal(outcomeSummary(task.finished, 1), "Native step succeeded, answer failed");
  const guidance = errorGuidance(task.finished.error);
  assert.match(guidance.title, /context/);
  assert.match(guidance.detail, /larger context length/);
});

test("terminal native results merge with live observations by operation ID", () => {
  const live = success("op_1", "math.calculate", { value: 4 });
  const replacement = success("op_1", "math.calculate", { value: 12193 });
  const device = success("op_2", "device.info", { os: "Linux", architecture: "x86_64", cpu: "CPU", logical_cpus: 6, memory_total_bytes: 1024 });
  const task = run([
    { kind: "observation", ...live },
    {
      kind: "finished", state: "completed", answer: "Done", error: null, journalIncomplete: false,
      usage: { toolCalls: 2, modelCalls: 3, retries: 0, generatedTokens: 40 }, executions: [],
      nativeResults: [replacement, device],
    },
  ]);
  assert.deepEqual(task.nativeResults.map((item) => item.record.operationId), ["op_1", "op_2"]);
  assert.deepEqual(resultView(task.nativeResults[0]), { kind: "calculator", value: 12193 });
  assert.equal(mergeNativeResults(task.nativeResults, undefined), task.nativeResults);
});

test("events for another task are ignored", () => {
  const task = createLiveTask("7", "prompt", 1000, 0, 0);
  assert.equal(applyAgentEvent(task, { taskId: "8", kind: "state_changed", state: "planning" }, { key: 1, at: 1, perf: 1 }), task);
});

test("approval keeps the trusted target and clears once the operation executes", () => {
  const target = { displayName: "KCalc", desktopFile: "/usr/share/applications/org.kde.kcalc.desktop", executable: "/usr/bin/kcalc" };
  const waiting = run([
    { kind: "state_changed", state: "awaiting_authorization" },
    { kind: "approval_required", operationId: "op_9", tool: "apps.launch", expiresInMs: 60_000, target },
  ]);
  assert.deepEqual(waiting.approval.target, target);
  assert.equal(waiting.approval.expiresAt, 11 + 60_000);
  assert.equal(currentActivity(waiting.events.map((entry) => entry.event)), "Waiting for your approval");
  assert.deepEqual(progressSteps(waiting.events.map((entry) => entry.event), true).at(-1),
    { key: "op_9", label: "Waiting for approval to launch KCalc", status: "waiting" });

  const launched = applyAgentEvent(waiting, { taskId: waiting.id, kind: "execution", record: record("op_9", "apps.launch") },
    { key: 9, at: 9, perf: 9 });
  assert.equal(launched.approval, null);
  assert.deepEqual(resultView(success("op_9", "apps.launch", { app_id: "app_1", dispatched: true }), new Map(Object.entries(launched.targets))),
    { kind: "launch", appId: "app_1", dispatched: true, target });

  const denied = [...waiting.events.map((entry) => entry.event), { kind: "finished", state: "denied", answer: null,
    error: { code: "approval_declined", message: "The operation was denied." }, usage: {}, executions: [], journalIncomplete: false }];
  assert.deepEqual(progressSteps(denied, false).at(-1), { key: "op_9", label: "You declined to launch KCalc", status: "notice" });
  assert.deepEqual(errorGuidance({ code: "approval_declined", message: "The operation was denied." }, "denied"),
    { title: "You declined the request", detail: "The action you declined was not run." });
  const missing = { code: "permission_denied", message: "An active file grant is required." };
  assert.doesNotMatch(errorGuidance(missing, "denied").title, /You declined/);
  assert.equal(outcomeSummary({ state: "denied", error: missing }, 0), "Permission denied");
  const expired = { code: "approval_expired", message: "The approval window closed before the application was authorized." };
  assert.equal(errorGuidance(expired, "timed_out").title, "Application approval expired");
  assert.match(errorGuidance(expired, "timed_out").detail, /Nothing was launched.*new approval/);
  assert.doesNotMatch(errorGuidance(expired, "timed_out").detail, /Increase the task time limit/);
  assert.equal(outcomeSummary({ state: "timed_out", error: expired }, 0), "Approval expired");
  const timedOut = [...denied.slice(0, -1), { ...denied.at(-1), state: "timed_out", error: expired }];
  assert.deepEqual(progressSteps(timedOut, false).at(-1),
    { key: "op_9", label: "Application launch approval expired", status: "notice" });
});

test("progress summarises low-level events and never overstates outcomes", () => {
  const events = [
    { kind: "state_changed", state: "planning" },
    { kind: "proposal_rejected", code: "invalid_arguments" },
    { kind: "dispatch_started", operationId: "op_1", tool: "files.read" },
  ];
  assert.deepEqual(progressSteps(events, true).map((step) => step.status), ["notice", "active"]);
  assert.equal(currentActivity([...events, { kind: "state_changed", state: "executing" }]), "Reading selected file");
  // When the task is no longer running, unfinished steps are not shown as still working.
  assert.deepEqual(progressSteps(events, false).map((step) => step.status), ["notice", "notice"]);
  assert.equal(executionLabel({ tool: "apps.launch", status: "uncertain", error: "tool_failed" }),
    "Application launch outcome is uncertain (tool failed)");
  assert.equal(executionLabel({ tool: "files.search", status: "not_dispatched", error: null }), "File search was not run");
});

test("structured result views respect native truncation and reject unexpected shapes", () => {
  assert.deepEqual(resultView(success("a", "files.read", { content: "hello", file_bytes: 70_000, truncated: true })),
    { kind: "file-read", content: "hello", fileBytes: 70_000, truncated: true });
  assert.deepEqual(resultView(success("b", "apps.list", { applications: [{ app_id: "x", display_name: "Files" }, { bad: 1 }], truncated: true })),
    { kind: "apps", applications: [{ appId: "x", displayName: "Files" }], truncated: true });
  const device = resultView(success("c", "device.info", {
    os: "Linux", architecture: "x86_64", cpu: "Intel", logical_cpus: 6,
    memory_total_bytes: 16 * 1024 ** 3, memory_available_bytes: 4 * 1024 ** 3, storage_total_bytes: null, gpu: null,
  }));
  assert.equal(device.kind, "device");
  assert.deepEqual(device.rows.find((row) => row.label === "Memory"), { label: "Memory", value: "4.0 GiB available of 16.0 GiB" });
  assert.deepEqual(device.rows.find((row) => row.label === "Root storage"), { label: "Root storage", value: "Not reported" });
  assert.equal(resultView(success("d", "math.calculate", { value: "12" })).kind, "unrecognized");
  assert.deepEqual(resultView({ record: record("e", "files.search", "failed"), observation: { tool: "files.search", status: "failure", error: { code: "tool_failed", message: "Unreadable" } } }),
    { kind: "failure", error: { code: "tool_failed", message: "Unreadable" } });
});

test("permission labels use sanitized metadata and never the revocation reference", () => {
  const scope = { id: "grant-1f2e3d-4", displayName: "Documents", kind: "directory", access: "read_only", state: "active", remainingMs: 52 * 60_000 };
  const label = scopeSummary(scope, scope.remainingMs);
  assert.equal(label, "Documents · Folder · Read-only · 52 min left");
  assert.doesNotMatch(label, /grant-|1f2e3d/);
  assert.equal(scopeSummary({ ...scope, kind: "file", displayName: "notes.txt", state: "revoked" }, 0), "notes.txt · File · Read-only · Revoked");
  assert.equal(scopeSummary({ ...scope, state: "expired" }, 0), "Documents · Folder · Read-only · Expired");
});

test("usage parsing accepts host camel case and persisted snake case", () => {
  assert.deepEqual(usageFrom({ toolCalls: 1, modelCalls: 2, retries: 0, generatedTokens: 9 }), { toolCalls: 1, modelCalls: 2, retries: 0, generatedTokens: 9 });
  assert.deepEqual(usageFrom({ tool_calls: 1, model_calls: 2, retries: 1, generated_tokens: 9 }), { toolCalls: 1, modelCalls: 2, retries: 1, generatedTokens: 9 });
  assert.equal(usageFrom({ toolCalls: 1 }), null);
  assert.equal(validTimeout(300), true);
  assert.equal(validTimeout(0), false);
  assert.equal(validTimeout(601), false);
  assert.equal(validTimeout(2.5), false);
});

test("a scope mismatch is labelled as a mismatch, not a generic denial or a user refusal", async () => {
  const { outcomeSummary } = await import("./agent.ts");
  const error = { code: "scope_kind_mismatch", message: "The request did not match the selected resource." };
  assert.equal(outcomeSummary({ state: "denied", answer: null, error }, 0), "Not run: the request didn't match the selection");
  assert.equal(outcomeSummary({ state: "denied", answer: null, error: { code: "approval_declined", message: "" } }, 0), "You declined the request");
  assert.equal(outcomeSummary({ state: "timed_out", answer: null, error: { code: "approval_expired", message: "" } }, 0), "Approval expired");
});
