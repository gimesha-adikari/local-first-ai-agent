// Included only by the opt-in Rust desktop test. The main window prepares a
// synthetic profile, opens the native contextual panel and checks what the
// panel left behind. Loopback provider fixture only; no real network or data.
void (async () => {
  const ipc = window.__TAURI_INTERNALS__;
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const failure = async (promise) => {
    try {
      await promise;
    } catch (error) {
      return error;
    }
    throw new Error("Expected a failure");
  };
  const result = { pass: false, steps: [] };
  const call = (command, args) => { result.at = command; return ipc.invoke(command, args); };
  try {
    const panelDone = new Promise((resolve) => { window.__glancePanelDone = resolve; });
    const view = await call("provider_save", {
      id: null,
      settings: {
        name: "Loopback fixture",
        endpoint: "http://127.0.0.1:__PORT__/v1/",
        locality: "self_hosted",
        model: "fixture-model",
        contextTokens: 8192,
        maxOutputTokens: 128,
        temperature: 0.2,
        imageInput: false,
        streamUsage: true,
        requiresKey: false,
        retention: "none",
        retentionSummary: "",
        sharing: { history: false, memories: true, documents: false },
        pricing: null,
        budget: { monthlyTokens: 50000, monthlyCostMicros: null },
      },
    });
    await call("set_intelligence_settings", { settings: { localOnly: false, activeProvider: null } });
    await call("memory_set_retrieval_enabled", { enabled: true });
    await call("memory_create", { content: "Synthetic memory: the orchard rainwater tanks hold 400 litres." });
    assert((await call("list_conversations", { query: "" })).length === 0, "Profile is not empty");
    result.steps.push("synthetic profile: scripted local model, loopback provider, one memory, one document");

    for (const command of ["glance_state", "glance_clipboard", "glance_remember", "glance_dismiss"]) {
      const denied = await failure(call(command));
      assert(!denied?.code || denied.code !== "cancelled", `${command} reached the panel host from the main window`);
    }
    const run = await failure(call("glance_run", {
      input: { text: "x", recipe: "explain", share: false, memory: false, documents: [] }, deltas: "__CHANNEL__:0",
    }));
    assert(String(run?.message ?? run).includes("not allowed") || typeof run === "string", "Main window may run panel requests");
    result.steps.push("main window cannot use panel commands");

    await call("glance_show");
    const report = await panelDone;
    result.panel = report.panel;
    assert(report.panel.pass === true, `Panel fixture failed: ${report.panel.error}`);
    assert(report.closed === true, "Escape left a panel window or session behind");
    assert(report.clipboard === "Remote fixture answer", `External clipboard owner saw ${JSON.stringify(report.clipboard)}`);
    result.steps.push(...report.panel.steps.map((step) => `panel: ${step}`));
    result.steps.push("Escape destroyed the panel and its in-memory session; Copy reached the external clipboard");

    const memories = await call("memory_list", { query: null, beforeId: null, limit: 10 });
    assert(memories.length === 2, `Expected one remembered result, found ${memories.length - 1}`);
    const saved = memories[0];
    assert(saved.content.startsWith("Local fixture key points"), "Remember stored the wrong text");
    assert(saved.provenance?.source === "contextual_panel" && saved.provenance?.authorization === "explicit_user_remember",
      "Remembered memory has no panel provenance");
    assert(memories.every((memory) => !memory.content.includes("HOLD_LOCAL")), "A stopped request reached memory");
    assert((await call("list_conversations", { query: "" })).length === 0, "The panel created a conversation");
    assert((await call("provider_list"))[0].id === view.id, "Provider changed");
    result.steps.push("only the explicitly remembered result persisted, with provenance; no conversation was created");
    result.pass = true;
  } catch (error) {
    result.error = `${result.at}: ${String(error?.message ?? JSON.stringify(error))}`;
  }
  document.documentElement.dataset.glanceIpcResult = JSON.stringify(result);
})();
