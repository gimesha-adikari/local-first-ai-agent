// Opt-in test against the packaged WebView and an isolated encrypted profile.
void (async () => {
  const { invoke } = window.__TAURI_INTERNALS__;
  const result = { pass: false, steps: [] };
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const waitFor = async (find, message) => {
    const end = performance.now() + 6000;
    while (performance.now() < end) {
      const match = find();
      if (match) return match;
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
    throw new Error(message);
  };
  const button = (text) => [...document.querySelectorAll("button")].find(
    (item) => item.getClientRects().length && (item.textContent.trim() === text || item.getAttribute("aria-label") === text),
  );
  const click = async (text) => (await waitFor(() => button(text), `Missing button ${text}`)).click();
  const key = "lfm2.5-1.2b-instruct-q4_k_m";
  try {
    await click("Models");
    await waitFor(() => document.querySelectorAll(".catalog-card").length === 3, "The three catalog cards did not render");
    assert(button("Download Qwen3.5 4B (UD-Q4_K_XL)") && !button("Download Qwen3.5 4B (UD-Q4_K_XL)").disabled,
      "An available catalog entry cannot be downloaded");
    const unknown = await invoke("start_model_download", { key: "https://example.com/model", restart: false }).catch((error) => error);
    assert(unknown?.code === "not_found", "The command accepted an arbitrary model URL");
    result.steps.push("Three catalog cards, enabled direct download, curated-only command");

    await click("Downloads");
    await waitFor(() => document.querySelector("progress")?.value === 32, "Interrupted model progress missing");
    assert(button("Retry") && !button("Retry").disabled, "Interrupted download cannot be retried");
    await click("Discard partial…");
    await click("Discard partial");
    await waitFor(() => document.body.textContent.includes("No downloads in progress"), "Partial download was not cleared");
    const state = await invoke("model_catalog");
    assert(state.entries.find((entry) => entry.artifact.key === key)?.status === "available", "Discard did not update the engine");
    result.steps.push("Interrupted download progress and confirmed discard");

    await click("Installed");
    await waitFor(() => document.body.textContent.includes("No model loaded"), "Installed library did not render");
    (await waitFor(() => document.querySelector(".model-import summary"), "Manual import missing")).click();
    assert(!button("Import Chat GGUF…")?.disabled && !!button("Import Chat GGUF…"), "Manual Chat path unavailable");
    assert(!button("Import BGE GGUF…")?.disabled && !!button("Import BGE GGUF…"), "Manual BGE path unavailable");
    assert((await invoke("bootstrap")).activeModelId === null, "The catalog loaded a model");
    result.steps.push("Installed and both native import entry points with an empty local runtime");
    result.pass = true;
  } catch (error) {
    result.error = error?.message || String(error);
  } finally {
    document.documentElement.dataset.modelManagerResult = JSON.stringify(result);
  }
})();
