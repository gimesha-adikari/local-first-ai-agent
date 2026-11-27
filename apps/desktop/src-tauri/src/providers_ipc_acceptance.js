// Included only by the opt-in Rust desktop test. It uses the actual Tauri IPC
// against a loopback OpenAI-compatible fixture with synthetic data.
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
  const channel = (received) => `__CHANNEL__:${ipc.transformCallback((item) => {
    if ("message" in item) received.push(item.message);
  })}`;
  const result = { pass: false, steps: [] };
  try {
    const boot = await ipc.invoke("bootstrap");
    assert(boot.intelligence.activeProvider === null, "New profile has a remote Chat target");
    assert(boot.models.length === 0 && boot.inference.model === null, "Profile has a local model");
    assert((await ipc.invoke("provider_list")).length === 0, "Unexpected providers");
    result.steps.push("new profile selects the local model");

    const settings = {
      name: "Fixture server",
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
      sharing: { history: true, memories: false, documents: false },
      pricing: null,
      budget: { monthlyTokens: 50000, monthlyCostMicros: null },
    };
    const smuggled = await failure(ipc.invoke("provider_save", { id: null, settings: { ...settings, apiKey: "sk-fixture" } }));
    assert(smuggled, "A credential field was accepted over IPC");
    const insecure = await failure(ipc.invoke("provider_save", {
      id: null, settings: { ...settings, locality: "cloud", requiresKey: true, retention: "provider_policy", retentionSummary: "30 days" },
    }));
    assert(insecure.code === "invalid_config", "A cloud provider over http was accepted");
    const view = await ipc.invoke("provider_save", { id: null, settings });
    assert(view.settings.endpoint === "http://127.0.0.1:__PORT__/v1", "Endpoint was not canonicalized");
    assert(view.readiness === "ready" && view.network === "loopback" && view.credential === null, "Unexpected provider status");
    assert(!JSON.stringify(view).includes("sk-"), "Provider view contains a credential");
    result.steps.push("provider saved without credential fields");

    const thread = await ipc.invoke("create_conversation");
    const blocked = await failure(ipc.invoke("send_message", { id: thread.id, content: "Hello", events: channel([]) }));
    assert(blocked.code === "no_model", "Unselected provider received Chat");
    await ipc.invoke("set_intelligence_settings", { settings: { activeProvider: view.id } });
    const stale = await failure(ipc.invoke("send_message", {
      id: thread.id, content: "Stale local target", options: { expectedTarget: { providerId: null } }, events: channel([]),
    }));
    assert(stale.code === "invalid_config", "An outdated local target was routed to the server");
    const test = await ipc.invoke("provider_test", { id: view.id });
    assert(test.modelListed === true, "Discovery did not list the model");
    assert(test.models.includes("seen-0"), `Requests reached the fixture before the explicit connection test: ${test.models}`);
    result.steps.push("discovery through the boundary; zero earlier requests");

    const events = [];
    const answer = await ipc.invoke("send_message", { id: thread.id, content: "Remote hello", options: { expectedTarget: { providerId: view.id } }, events: channel(events) });
    assert(answer.content === "Remote fixture answer" && answer.state === "complete", `Unexpected answer ${answer.content}`);
    assert(events.some((event) => event.kind === "delta"), "Remote answer did not stream");
    assert(answer.stats.promptTokens === 21 && answer.stats.generatedTokens === 3, "Provider usage was not reported");
    const listed = (await ipc.invoke("provider_list"))[0];
    assert(listed.usage.reportedTokens === 24 && listed.usage.reservedTokens === 0, "Usage was not reconciled");
    result.steps.push("configured remote Chat completed without a local model");

    let firstDelta;
    const waiting = new Promise((resolve) => { firstDelta = resolve; });
    const holdChannel = `__CHANNEL__:${ipc.transformCallback((event) => {
      if (event.message?.kind === "delta") firstDelta();
    })}`;
    const held = failure(ipc.invoke("send_message", { id: thread.id, content: "HOLD_FOR_POLICY", events: holdChannel }));
    await waiting;
    const busyDelete = await failure(ipc.invoke("provider_delete", { id: view.id }));
    assert(busyDelete.code === "busy", "Deletion was admitted during a turn");
    const busySelection = await failure(ipc.invoke("set_intelligence_settings", { settings: { activeProvider: null } }));
    assert(busySelection.code === "busy", "Chat target changed while the turn was active");
    assert(await ipc.invoke("stop_generation"), "Stop was not accepted");
    const stopped = await held;
    assert(stopped.code === "cancelled", "Stop did not cancel the remote turn");
    await ipc.invoke("set_intelligence_settings", { settings: { activeProvider: null } });
    assert((await ipc.invoke("intelligence_settings")).activeProvider === null, "Local selection was not saved");
    const again = await failure(ipc.invoke("send_message", { id: thread.id, content: "Stay local", events: channel([]) }));
    assert(again.code === "no_model", "Local selection did not keep Chat local");
    result.steps.push("Stop cancels remote Chat, and choosing the local target keeps Chat on-device");
    await ipc.invoke("provider_delete", { id: view.id });
    assert((await ipc.invoke("provider_list")).length === 0, "Provider was not removed");
    result.steps.push("local target saved and provider removed");
    result.pass = true;
  } catch (error) {
    result.error = String(error?.message ?? JSON.stringify(error));
  }
  document.documentElement.dataset.providersIpcResult = JSON.stringify(result);
})();
