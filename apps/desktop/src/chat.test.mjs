import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Markdown from "react-markdown";
import { applyChatEvent, earlierReplies, splitReasoning, validModelSettings } from "./chat.ts";
import { offlineMarkdown } from "./markdown.ts";

const message = (id, position, role, state, content) => (
  { id, position, role, state, content, createdAt: 1, stats: null }
);

test("starts a turn, preserves order, and streams only into the pending assistant", () => {
  const earlier = message(10, 1, "assistant", "complete", "Earlier");
  const user = message(11, 2, "user", "complete", "Hello");
  const assistant = message(12, 3, "assistant", "streaming", "");
  const started = applyChatEvent([earlier], { kind: "started", user, assistant });
  assert.deepEqual(started.map(({ id }) => id), [10, 11, 12]);

  const first = applyChatEvent(started, { kind: "delta", id: 12, text: "First" });
  const second = applyChatEvent(first, { kind: "delta", id: 12, text: "\n世界" });
  assert.equal(second[2].content, "First\n世界");
  assert.equal(applyChatEvent(second, { kind: "delta", id: 10, text: "stale" }), second);
  assert.equal(applyChatEvent(second, { kind: "delta", id: 11, text: "stale" }), second);
  assert.equal(applyChatEvent(second, { kind: "delta", id: 99, text: "stale" }), second);
});

test("regeneration starts a new attempt without duplicating the user's message", () => {
  const user = message(1, 0, "user", "complete", "Hello");
  const initial = message(2, 1, "assistant", "cancelled", "First");
  const retry = message(3, 2, "assistant", "streaming", "");
  const next = applyChatEvent([user, initial], { kind: "started", user: null, assistant: retry });
  assert.deepEqual(next, [user, initial, retry]);
  assert.deepEqual([...earlierReplies(next)], [2]);
});

test("reasoning delimiters tolerate arbitrary stream boundaries", () => {
  const raw = "<think>\nprivate notes\n</think>\n\nThe answer";
  for (let offset = 1; offset < raw.length; offset++) {
    const partial = splitReasoning(raw.slice(0, offset), true);
    assert.doesNotMatch(partial.answer, /think|private/);
    assert.doesNotMatch(partial.reasoning ?? "", /<\/?think|<\//);
  }
  assert.deepEqual(splitReasoning(raw, false), {
    answer: "The answer", reasoning: "private notes", reasoningComplete: true,
  });
});

test("interrupted reasoning stays separate and plain answers preserve Markdown", () => {
  assert.deepEqual(splitReasoning("<think>partial</th", false), {
    answer: "", reasoning: "partial", reasoningComplete: false,
  });
  const plain = "The tag `<think>` is a literal.\n\n    indented code";
  assert.deepEqual(splitReasoning(plain, false), {
    answer: plain, reasoning: null, reasoningComplete: false,
  });
  assert.equal(splitReasoning("<think>done</think>\n\n    indented code", false).answer, "    indented code");
});

test("Markdown retains code and text while omitting raw markup, images and link destinations", () => {
  const malicious = [
    "**Visible text**",
    '<script>globalThis.executed = true</script>',
    '<iframe src="https://evil.invalid"></iframe>',
    "![fetch me](https://evil.invalid/a.png)",
    "[bad](javascript:alert%281%29) [remote](https://evil.invalid/)",
    "```html\n<img src=x onerror=alert(1)>\n```",
  ].join("\n\n");
  const html = renderToStaticMarkup(createElement(Markdown, offlineMarkdown, malicious));
  assert.match(html, /<strong>Visible text<\/strong>/);
  assert.match(html, /<pre><code[^>]*>&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.match(html, /bad/);
  assert.match(html, /remote/);
  assert.doesNotMatch(html, /<script|<iframe|<img|href=|src="|evil\.invalid|javascript:/);
});

test("model settings reserve room for input and reject invalid number fields", () => {
  const defaults = { contextLength: 2048, threads: 4, temperature: 0.7, maxTokens: 256,
    topP: 0.8, topK: 20, enableThinking: false };
  assert.equal(validModelSettings(defaults), true);
  assert.equal(validModelSettings({ ...defaults, contextLength: 256 }), false);
  assert.equal(validModelSettings({ ...defaults, threads: 1.5 }), false);
  assert.equal(validModelSettings({ ...defaults, temperature: NaN }), false);
});
