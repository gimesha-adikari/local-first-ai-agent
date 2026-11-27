import assert from "node:assert/strict";
import test from "node:test";
import {
  cameraSummary, queryProblem, searchDisclosure, searchUnavailable, selectionRegion, suggestQuery, toggleLanguage, visualClassNames,
} from "./visual.ts";

test("a drawn selection maps to frame pixels, rounding outward, and tiny ones mean the whole image", () => {
  const view = { width: 320, height: 240 };
  const frame = { width: 640, height: 480 };
  assert.deepEqual(selectionRegion({ x: 10.2, y: 20.7 }, { x: 110.1, y: 70.2 }, view, frame), { x: 20, y: 41, width: 201, height: 100 });
  // Corners in any order; clamped to the frame.
  assert.deepEqual(selectionRegion({ x: 400, y: 300 }, { x: 300, y: 200 }, view, frame), { x: 600, y: 400, width: 40, height: 80 });
  assert.equal(selectionRegion({ x: 5, y: 5 }, { x: 6, y: 6 }, view, frame), null);
  assert.equal(selectionRegion({ x: 0, y: 0 }, { x: NaN, y: 9 }, view, frame), null);
  assert.equal(selectionRegion({ x: 0, y: 0 }, { x: 9, y: 9 }, { width: 0, height: 9 }, frame), null);
});

test("suggested queries collapse whitespace and respect the search limits", () => {
  assert.equal(suggestQuery("  CAMERA TEST 42\n\nVisual   search "), "CAMERA TEST 42 Visual search");
  assert.equal(suggestQuery("word ".repeat(80)).split(" ").length, 50);
  const long = suggestQuery("abcdefghij ".repeat(60));
  assert.ok(Array.from(long).length <= 400);
  // Right-to-left and non-Latin text is kept intact.
  assert.equal(suggestQuery(" مرحبا بالعالم "), "مرحبا بالعالم");
  assert.equal(queryProblem("   "), "Enter something to search for.");
  assert.match(queryProblem("x ".repeat(51)), /at most 400 characters and 50 words/);
  assert.equal(queryProblem("CAMERA TEST 42"), null);
});

test("search is explained when unavailable and the disclosure is exact", () => {
  assert.match(searchUnavailable({ provider: "brave", name: "Brave Search", personalUseOnly: false }, true), /Local-only mode/);
  assert.match(searchUnavailable({ provider: null, name: null, personalUseOnly: false }, false), /Settings → Web search/);
  assert.equal(searchUnavailable({ provider: "brave", name: "Brave Search", personalUseOnly: false }, false), null);
  const purili = searchDisclosure({ provider: "purili", name: "Purili", personalUseOnly: true }, true);
  assert.match(purili, /Only the query text above is sent to Purili/);
  assert.match(purili, /No screenshot, camera image, document, memory or conversation is sent/);
  assert.match(purili, /not reverse image matching/);
  assert.match(purili, /model wrote/);
  assert.match(purili, /personal, non-commercial use/);
  assert.doesNotMatch(searchDisclosure({ provider: "brave", name: "Brave Search", personalUseOnly: false }, false), /model wrote|Purili/);
});

test("OCR languages toggle in order up to four, and receipts name camera classes", () => {
  let chosen = toggleLanguage([], "eng", true);
  for (const code of ["sin", "tam", "ara", "jpn"]) chosen = toggleLanguage(chosen, code, true);
  assert.deepEqual(chosen, ["eng", "sin", "tam", "ara"]);
  assert.deepEqual(toggleLanguage(chosen, "eng", true), chosen);
  assert.deepEqual(toggleLanguage(chosen, "sin", false), ["eng", "tam", "ara"]);
  assert.equal(visualClassNames["core.camera-image/1"], "Camera image");
  const record = { camera: "Laptop camera", camerasOffered: 2, releasedMs: 1200,
    stream: { width: 1280, height: 720, format: "YUY2" } };
  assert.equal(cameraSummary(record), "Laptop camera (one of 2) · 1280 × 720 YUY2 · camera released after 1200 ms");
});
