import assert from "node:assert/strict";
import test from "node:test";
import { inputProblem, prepareGlance, recipes } from "./glance.ts";

const state = { localAvailable: true, localOnly: false, memoryEnabled: true, providers: [], documents: [{ id: 3, name: "Notes" }] };
const provider = { id: 7, revision: "exact-revision", sharing: { documents: true, memories: false } };

test("all eight recipes keep the reviewed text intact and begin without a sharing grant", () => {
  assert.equal(Object.keys(recipes).length, 8);
  for (const recipe of Object.keys(recipes)) {
    const input = prepareGlance(state, provider, recipe, "  text\n", "French", "Why?", true, [3, 4]);
    assert.equal(input.text, "  text\n");
    assert.equal(input.recipe, recipe);
    assert.equal(input.share, false);
    assert.equal(input.provider, 7);
    assert.equal(input.revision, "exact-revision");
    assert.equal(input.language, recipe === "translate" ? "French" : undefined);
    assert.equal(input.question, recipe === "custom" ? "Why?" : undefined);
    assert.equal(input.memory, false);
    assert.deepEqual(input.documents, [3]);
  }
});

test("local model and context are explicitly selected and checked for availability", () => {
  const prepare = (current, selected, memory, ids) => prepareGlance(current, selected, "explain", "text", "", "", memory, ids);
  assert.equal(prepare({ ...state, localAvailable: false }, undefined, false, []), null);
  const local = prepare(state, undefined, false, []);
  assert.equal(local.provider, undefined);
  assert.equal(local.revision, undefined);
  assert.equal(local.memory, false);
  assert.deepEqual(local.documents, []);
  const restricted = prepare({ ...state, memoryEnabled: false }, { ...provider, sharing: { documents: false, memories: true } }, true, [3]);
  assert.equal(restricted.memory, false);
  assert.deepEqual(restricted.documents, []);
});

test("source, custom question and language respect native UTF-8 limits", () => {
  assert.equal(inputProblem("🌱".repeat(2048), "explain", "", ""), null);
  assert.ok(inputProblem("🌱".repeat(2049), "explain", "", ""));
  assert.ok(inputProblem("a\0", "explain", "", ""));
  assert.ok(inputProblem("text", "translate", "Ä".repeat(51), ""));
  assert.ok(inputProblem("text", "translate", "French\nSwiss", ""));
  assert.ok(inputProblem("text", "custom", "", "Ä".repeat(1025)));
  assert.equal(prepareGlance(state, undefined, "translate", "text", "  ", "", false, []), null);
  assert.equal(prepareGlance(state, undefined, "custom", "text", "", "", false, []), null);
});

test("a screen crop is offered only to image providers and makes text optional", () => {
  const vision = { ...provider, imageInput: true };
  const image = prepareGlance(state, vision, "explain", "", "", "", false, [], "crop-1");
  assert.equal(image.image, true);
  assert.equal(image.crop, "crop-1");
  assert.equal(image.text, "");
  assert.equal(image.share, false);
  // No silent switch: a crop cannot go to a text-only provider or the local model.
  assert.equal(prepareGlance(state, provider, "explain", "text", "", "", false, [], "crop-1"), null);
  assert.equal(prepareGlance(state, undefined, "explain", "text", "", "", false, [], "crop-1"), null);
  // Recognized text alone goes anywhere text may go.
  const text = prepareGlance(state, undefined, "summarize", "Recognized text", "", "", false, []);
  assert.equal(text.image, undefined);
  assert.equal(text.crop, undefined);
});

test("OCR languages show readable names and keep unknown codes", async () => {
  const { languageName } = await import("./glance.ts");
  assert.equal(languageName("chi_sim"), "Chinese (Simplified)");
  assert.equal(languageName("ara"), "Arabic");
  assert.equal(languageName("xyz"), "xyz");
});
