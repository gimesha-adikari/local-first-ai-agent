import assert from "node:assert/strict";
import test from "node:test";
import { appendDelta } from "./inference.ts";

test("stream chunks preserve whitespace and Unicode", () => {
  const output = { id: 5, text: "" };
  const first = appendDelta(output, 5, { text: "Hello" });
  assert.deepEqual(appendDelta(first, 5, { text: "\n世界" }), { id: 5, text: "Hello\n世界" });
});

test("a late chunk cannot change another generation", () => {
  const output = { id: 6, text: "Current" };
  assert.equal(appendDelta(output, 5, { text: "stale" }), output);
  assert.equal(appendDelta(output, 6, { text: "" }), output);
});
