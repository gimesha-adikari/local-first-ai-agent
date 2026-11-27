// Included only by the opt-in Rust desktop test (Stage 04-E). Drives the
// contextual panel's camera snapshot, text extraction, translation and
// visual-assisted search review with synthetic frames and no network.
void (async () => {
  const root = document.documentElement;
  const result = { pass: false, steps: [] };
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const until = async (probe, what, ms = 20000) => {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const value = probe();
      if (value) return value;
      await sleep(50);
    }
    throw new Error(`Timed out waiting for ${what}`);
  };
  const shot = async (stage) => {
    await sleep(300);
    root.dataset.visualStage = stage;
    await until(() => root.dataset.visualShotDone === stage, `screenshot ${stage}`, 20000);
  };
  const button = (label) => [...document.querySelectorAll("button")]
    .find((item) => item.textContent.trim().startsWith(label) && !item.disabled && item.getClientRects().length);
  const click = async (label) => (await until(() => button(label), `enabled button "${label}"`)).click();
  const set = (element, value) => {
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value").set;
    setter.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  };
  try {
    // The warm panel loads hidden; wait until the summoned session exists.
    const end = Date.now() + 30000;
    for (;;) {
      try { await window.__TAURI_INTERNALS__.invoke("glance_state"); break; } catch (error) {
        if (Date.now() > end) throw new Error(`no panel session: ${error?.message ?? error}`);
        await sleep(150);
      }
    }
    await until(() => document.visibilityState === "visible" && button("Camera"), "panel visible");
    await sleep(300);
    const localOnly = /Local-only mode is on/.test(document.body.textContent);
    await click("Camera");
    const image = await until(() => { const img = document.querySelector(".snapshot-review img"); return img?.complete && img.naturalWidth ? img : null; },
      "panel snapshot preview");
    assert(image.src.startsWith("glance-crop://localhost/"), `preview ${image.src}`);
    await shot("panel-snapshot-review");
    await click("Use (whole snapshot)");
    await until(() => [...document.querySelectorAll(".glance-crop strong")].some((item) => item.textContent === "Camera snapshot"), "camera crop");
    result.steps.push("snapshot previewed, confirmed and shown as the session's camera image");

    await click("Extract text");
    const text = await until(() => document.getElementById("glance-source").value, "recognized text");
    assert(/Settings/.test(text), `recognized ${text}`);
    assert(/Camera snapshot/.test(document.querySelector(".glance-crop").textContent), "source label");
    await shot("panel-camera-text");
    result.steps.push("text extracted on this device from the camera snapshot");

    await click("Translate");
    const target = await until(() => [...document.querySelectorAll("label")].find((item) => item.textContent.startsWith("Target language"))?.querySelector("input"), "target field");
    assert(target.value === "French", `shared target ${target.value}`);
    assert(/local model on this device/.test(document.querySelector(".glance-options").textContent), "locality disclosure");
    const run = [...document.querySelectorAll(".glance-foot button")].find((item) => !item.disabled && item.textContent.includes("Translate"));
    assert(run, "Translate run button");
    run.click();
    await until(() => document.querySelector(".glance-output")?.textContent, "translation output", 30000);
    const receipt = JSON.parse(document.querySelector(".glance-receipt pre")?.textContent || "null");
    assert(receipt?.used?.some((item) => item.kind === "camera_text"), `receipt ${JSON.stringify(receipt?.used)}`);
    assert(receipt.external.length === 0, "translation left the device");
    await shot("panel-translation");
    result.steps.push("translated by the local model with the shared target language; receipt records camera text and no transmission");

    await click("Search the web");
    const box = await until(() => document.querySelector(".glance-search")?.textContent, "search composer");
    if (localOnly) {
      // Local-only: capture and recognition work, web search stays off.
      assert(/Local-only mode is on\. Web search needs internet access/.test(box), `local-only search ${box}`);
      assert(!button("Search once") && !document.getElementById("glance-query"), "search offered in Local-only mode");
      await shot("panel-search-local-only");
      result.steps.push("in Local-only mode the camera, text extraction and local translation worked while web search was off with an explanation");
    } else {
      const query = await until(() => document.getElementById("glance-query")?.value, "prefilled query");
      assert(/Settings/.test(query), `query ${query}`);
      assert(/Only the query text above is sent to Brave Search/.test(box) && /not reverse image matching/.test(box), "search disclosure");
      set(document.getElementById("glance-query"), "Save changes before closing dialog");
      await shot("panel-search-review");
      result.steps.push("search query prefilled from the recognized text, editable, with the exact text-only disclosure");
    }
    result.pass = true;
  } catch (error) {
    result.error = String(error?.message ?? error);
    result.visible = [...document.querySelectorAll(".glance-error, .glance-note, [role=status]")].map((item) => item.textContent.trim()).join(" | ").slice(0, 800);
  }
  root.dataset.visualResult = JSON.stringify(result);
})();
