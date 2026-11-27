// Included only by the opt-in Rust desktop test (Stage 04-E). Drives Chat's
// camera snapshot, crop, attachment and image actions in the packaged
// WebView, then opens the contextual panel. Synthetic frames only: the
// host's test fixture replaces the portal, and no network is available.
void (async () => {
  const { invoke } = window.__TAURI_INTERNALS__;
  const root = document.documentElement;
  const result = { pass: false, steps: [] };
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const waitFor = async (find, message, timeout = 20000) => {
    const end = performance.now() + timeout;
    while (performance.now() < end) {
      const match = await find();
      if (match) return match;
      await sleep(60);
    }
    throw new Error(message);
  };
  const button = (label) => [...document.querySelectorAll("button")].find((item) => item.getClientRects().length
    && (item.getAttribute("aria-label") === label || item.textContent.trim().startsWith(label)));
  const click = async (label) => (await waitFor(() => {
    const found = button(label);
    return found && !found.disabled ? found : null;
  }, `Missing or disabled button ${label}`)).click();
  const set = (element, value) => {
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value").set;
    setter.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const card = async (label) => (await waitFor(() => [...document.querySelectorAll(".attachment-card button")]
    .find((item) => item.textContent.trim() === label && !item.disabled), `Missing card action ${label}`)).click();
  const refusal = (promise) => promise.then(() => null, (error) => error);
  const shot = async (stage) => {
    await sleep(300);
    root.dataset.visualShot = stage;
    const end = performance.now() + 6000;
    while (performance.now() < end && root.dataset.visualShotDone !== stage) await sleep(80);
  };
  const drag = (frame, from, to) => {
    const rect = frame.getBoundingClientRect();
    const at = (point, type) => frame.dispatchEvent(new PointerEvent(type, {
      bubbles: true, pointerId: 1, isPrimary: true, clientX: rect.left + point.x, clientY: rect.top + point.y,
    }));
    at(from, "pointerdown");
    for (let step = 1; step <= 6; step++) {
      at({ x: from.x + ((to.x - from.x) * step) / 6, y: from.y + ((to.y - from.y) * step) / 6 }, "pointermove");
    }
    at(to, "pointerup");
  };
  try {
    await waitFor(() => document.querySelector("#composer-input"), "The composer did not appear", 60000);
    await waitFor(async () => (await invoke("inference_status")).phase === "ready", "The fixture model did not load");
    await invoke("web_research_save", { settings: { provider: "brave", braveSaveResults: false, searxngEndpoint: "", searxngSaveResults: false, puriliPersonalUse: false } });
    const visual = await invoke("visual_settings");
    assert(visual.ocr.available && visual.ocr.defaults.includes("eng"), `OCR state ${JSON.stringify(visual.ocr)}`);
    result.steps.push(`shared OCR languages default to ${visual.ocr.defaults.join(", ")}`);

    // Explicit gesture: the camera stage is visible, then the review appears.
    await click("Take a camera snapshot");
    const frame = await waitFor(() => document.querySelector(".chat-camera .snapshot-frame"), "Snapshot review did not appear");
    const image = await waitFor(() => { const img = frame.querySelector("img"); return img?.complete && img.naturalWidth ? img : null; },
      "Snapshot preview did not load");
    assert(image.src.startsWith("camera-preview://localhost/"), `Unexpected preview source ${image.src}`);
    assert(image.naturalWidth === 831 && image.naturalHeight === 288, `Preview ${image.naturalWidth}x${image.naturalHeight}`);
    assert(/camera is off/i.test(document.querySelector(".chat-camera").textContent), "Review does not say the camera is off");
    const attachmentsBefore = await invoke("attachment_list", { conversationId: null });
    assert(attachmentsBefore.length === 0, "Something was attached before confirmation");
    await shot("chat-snapshot-review");
    result.steps.push("camera snapshot previewed from memory; nothing attached before confirmation");

    // Crop to the text: frame pixels (40, 60)–(317, 132).
    const scale = frame.getBoundingClientRect().width / 831;
    drag(frame, { x: 40 * scale, y: 60 * scale }, { x: 317 * scale, y: 132 * scale });
    await waitFor(() => document.querySelector(".snapshot-selection"), "Selection did not show");
    await shot("chat-snapshot-crop");
    const previewUrl = image.src;
    await click("Attach (selected");
    const attached = await waitFor(async () => {
      const list = await invoke("attachment_list", { conversationId: null });
      return list.length === 1 && list[0].phase === "ready" ? list[0] : null;
    }, "Camera attachment did not become ready", 40000);
    assert(attached.origin === "camera" && attached.kind === "png" && attached.retention === "temporary", `Attachment ${JSON.stringify(attached)}`);
    assert(Math.abs(attached.image.width - 277) <= 3 && Math.abs(attached.image.height - 72) <= 3, `Cropped size ${JSON.stringify(attached.image)}`);
    assert(attached.passages > 0 && /eng/.test(attached.recognition ?? ""), `Recognition ${JSON.stringify(attached)}`);
    const stale = await fetch(previewUrl).then((response) => response.status, () => "refused");
    assert(stale !== 200, "The uncropped snapshot is still served after attaching");
    const reuse = await refusal(invoke("camera_attach", { request: { id: "0".repeat(24), region: null, conversationId: null, retention: "temporary" } }));
    assert(reuse, "A stale snapshot could be attached");
    result.steps.push(`cropped snapshot attached as a temporary camera attachment (${attached.image.width}×${attached.image.height}), OCR ${attached.recognition}; uncropped frame no longer served`);

    // Image actions: recognized text, then a reviewed search query.
    await card("Text");
    const text = await waitFor(() => document.querySelector(".image-text textarea")?.value, "Recognized text did not show");
    assert(/Settings/.test(text) && /Privacy/.test(text), `Recognized text ${text}`);
    await shot("chat-recognized-text");
    await card("Search");
    const query = await waitFor(() => document.querySelector(".research-edit input")?.value, "Search query was not prefilled");
    assert(/Settings/.test(query), `Query ${query}`);
    assert(/never\s+the image/.test(document.querySelector(".research-bar").textContent), "Visual search disclosure missing");
    assert([...document.querySelectorAll(".attachment-card button")].some((item) => item.textContent.trim() === "Paused"),
      "The image was not paused for the search message");
    await shot("chat-visual-search");
    result.steps.push(`search query prefilled from recognized text (“${query}”), editable, with the image paused and the text-only disclosure`);

    // Back to the attachment: translation request through Chat's pipeline.
    await click("Use my message");
    document.querySelector('.composer-pill[aria-pressed="true"]')?.click();
    await waitFor(() => !document.querySelector(".research-bar"), "Search did not turn off");
    await card("Paused");
    await card("Translate");
    const target = await waitFor(() => document.querySelector(".image-translate input"), "Target language field missing");
    set(target, "French");
    await shot("chat-translate-form");
    await click("Write request");
    await shot("chat-translate-request");
    const composer = document.querySelector("#composer-input");
    await waitFor(() => /Translate the text recognized in “Camera snapshot/.test(composer.value), `Composer ${composer.value}`);
    await waitFor(async () => (await invoke("visual_settings")).settings.translationTarget === "French", "Target language was not saved");
    await click("Send message");
    const answer = await waitFor(() => [...document.querySelectorAll(".assistant-turn, .message.assistant, [data-role='assistant']")]
      .map((item) => item.textContent).find((value) => /From the attachments/.test(value)), "Attachment answer missing", 30000);
    assert(/excerpts were supplied/.test(answer), `Answer ${answer}`);
    await shot("chat-translation-answer");
    result.steps.push("translation request answered from the snapshot's recognized text; target language saved for all visual sources");

    // The contextual panel: camera, extraction, translation and search review.
    const panelDone = new Promise((resolve) => { window.__visualPanelDone = resolve; });
    await invoke("glance_show");
    const panel = await panelDone;
    result.panel = panel;
    assert(panel.pass === true, `Panel failed: ${panel.error} [${panel.visible ?? ""}]`);
    result.steps.push(...panel.steps.map((step) => `panel: ${step}`));
    result.pass = true;
  } catch (error) {
    result.error = String(error?.message ?? error);
    result.html = document.querySelector(".composer, main")?.innerText?.slice(0, 1500);
  }
  root.dataset.visualResult = JSON.stringify(result);
})();
