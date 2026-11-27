// Included only by the opt-in Rust desktop test. The key is typed by an
// external XTest helper into the native GTK dialog; this script never sees it.
void (async () => {
  const ipc = window.__TAURI_INTERNALS__;
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const result = { pass: false, steps: [] };
  try {
    const view = await ipc.invoke("provider_save", {
      id: null,
      settings: {
        name: "Keyed fixture", endpoint: "http://127.0.0.1:__PORT__/v1", locality: "self_hosted", model: "fixture-model",
        contextTokens: 8192, maxOutputTokens: 128, temperature: 0.2, imageInput: false, streamUsage: true, requiresKey: true,
        retention: "none", retentionSummary: "", sharing: { history: false, memories: false, documents: false }, pricing: null,
        budget: { monthlyTokens: null, monthlyCostMicros: null },
      },
    });
    assert(view.credential === "missing", `Unexpected credential status ${view.credential}`);
    document.documentElement.dataset.dialogOpen = "1";
    const outcome = await ipc.invoke("provider_enter_credential", { id: view.id });
    assert(outcome === "stored", `Entry returned ${JSON.stringify(outcome)}`);
    result.steps.push("native dialog stored the key");
    await ipc.invoke("set_intelligence_settings", { settings: { localOnly: false, activeProvider: view.id } });
    const listed = (await ipc.invoke("provider_list"))[0];
    assert(listed.credential === "stored" && listed.readiness === "ready", "Status did not report the stored key");
    const test = await ipc.invoke("provider_test", { id: view.id });
    assert(test.models.includes("auth-ok"), `Credential not attached at the transport edge: ${test.models}`);
    result.steps.push("credential attached only at the transport edge");
    const removed = await ipc.invoke("provider_delete_credential", { id: view.id });
    assert(removed === "missing", "Key was not removed");
    const after = (await ipc.invoke("provider_list"))[0];
    assert(after.readiness === "needs_credential", "Provider still ready without a key");
    let failed = null;
    try { await ipc.invoke("provider_test", { id: view.id }); } catch (error) { failed = error; }
    assert(failed?.code === "credential_missing", `Missing key did not fail closed: ${JSON.stringify(failed)}`);
    result.steps.push("removed key fails closed");
    result.pass = true;
  } catch (error) {
    result.error = String(error?.message ?? JSON.stringify(error));
  }
  document.documentElement.dataset.credentialIpcResult = JSON.stringify(result);
})();
