// Opt-in test against the packaged WebView and an isolated encrypted profile.
// Runs without network access: every check is local or a policy refusal.
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
    (item) => item.getClientRects().length && (item.textContent.trim() === text || item.getAttribute("aria-label") === text
      || item.getAttribute("title") === text),
  );
  const click = async (text) => (await waitFor(() => {
    const found = button(text);
    return found && !found.disabled ? found : null;
  }, `Missing or disabled button ${text}`)).click();
  const refusal = (promise) => promise.then(() => null, (error) => error);
  try {
    const initial = await invoke("web_research_settings");
    assert(initial.localOnly === true && initial.settings.provider === null, "A new profile must start Local-only without a provider");
    assert(initial.braveKey !== undefined, "Brave key status missing");

    await click("Search the web or read a page for your next message");
    await waitFor(() => document.querySelector(".research-bar"), "The research disclosure did not open");
    await waitFor(() => document.body.textContent.includes("even with a local model"), "Local-only warning missing");
    const textarea = document.querySelector("#composer-input");
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
    setter.call(textarea, "What changed this week?");
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => document.body.textContent.includes("Choose a search provider") || document.body.textContent.includes("even with a local model"), "Plan problem missing");
    assert(button("Send message")?.disabled, "Send must stay disabled in Local-only mode");
    result.steps.push("Search toggle shows the disclosure and Local-only blocks sending");

    const conversation = await invoke("create_conversation");
    const channel = `__CHANNEL__:${window.__TAURI_INTERNALS__.transformCallback(() => {})}`;
    const denied = await refusal(invoke("send_message", {
      id: conversation.id, content: "Read it",
      options: { research: { mode: "page", url: "https://example.org/" } }, events: channel,
    }));
    assert(denied?.code === "policy_denied", `Local-only research must be refused, got ${JSON.stringify(denied)}`);
    assert((await invoke("get_conversation", { id: conversation.id })).messages.length === 0, "A refused research turn was saved");
    result.steps.push("The engine refuses research in Local-only mode before saving anything");

    const unsafe = await refusal(invoke("web_research_save", {
      settings: { provider: "searxng", braveSaveResults: false, searxngEndpoint: "http://169.254.169.254", searxngSaveResults: false },
    }));
    assert(unsafe?.code === "invalid_config", "A metadata-service SearXNG address was accepted");
    const saved = await invoke("web_research_save", {
      settings: { provider: "searxng", braveSaveResults: false, searxngEndpoint: "http://127.0.0.1:8888/", searxngSaveResults: false },
    });
    assert(saved.settings.searxngEndpoint === "http://127.0.0.1:8888" && saved.searxngNetwork === "loopback", "SearXNG was not canonicalized");
    const extra = await refusal(invoke("web_research_save", { settings: { provider: null, extra: true } }));
    assert(extra, "Unknown settings fields were accepted");
    result.steps.push("SearXNG settings are canonicalized and unsafe or unknown values refused");

    assert(await invoke("chat_web_research", { messageId: 1 }) === null, "A message without research returned sources");
    const link = await refusal(invoke("web_research_open", { messageId: 1, target: { kind: "source", index: 0 } }));
    assert(link?.code === "invalid_config", "A link was opened without a trusted research record");
    const far = await refusal(invoke("web_research_open", { messageId: 1, target: { kind: "discovered", index: 99 } }));
    assert(far?.code === "invalid_config", "An out-of-range link index was accepted");
    const raw = await refusal(invoke("web_research_open", { messageId: 1, url: "https://evil.example/" }));
    assert(raw, "The open command accepted a raw URL");
    result.steps.push("Links open only from trusted research records");

    await click("Settings");
    await click("Web search");
    await waitFor(() => document.body.textContent.includes("Brave Search API"), "Web search settings did not render");
    await waitFor(() => document.body.textContent.includes("http://127.0.0.1:8888 · This computer"), "Saved SearXNG address not shown");
    result.steps.push("Settings show the provider configuration");

    await click("Purili (experimental)");
    await waitFor(() => document.querySelector(".error-text[role=alert]")?.textContent.includes("non-commercial"),
      "Selecting Purili without confirmation was not refused");
    assert((await invoke("web_research_settings")).settings.provider === "searxng", "A refused selection replaced the provider");
    const consent = await waitFor(() => [...document.querySelectorAll("[role=switch]")].find(
      (item) => item.labels?.[0]?.textContent.includes("only for personal, non-commercial")), "Purili confirmation switch missing");
    consent.click();
    await waitFor(() => consent.getAttribute("aria-checked") === "true" && !button("Purili (experimental)")?.disabled,
      "The confirmation was not saved");
    await click("Purili (experimental)");
    await waitFor(() => button("Purili (experimental)")?.getAttribute("aria-checked") === "true", "Purili was not selected");
    const chosen = (await invoke("web_research_settings")).settings;
    assert(chosen.provider === "purili" && chosen.puriliPersonalUse === true, "Purili settings were not stored");
    consent.click();
    await waitFor(() => button("None")?.getAttribute("aria-checked") === "true", "Withdrawing the confirmation kept Purili selected");
    result.steps.push("Purili needs the personal-use confirmation and is deselected when it is withdrawn");
    result.pass = true;
  } catch (error) {
    result.error = error?.message || String(error);
  } finally {
    document.documentElement.dataset.researchResult = JSON.stringify(result);
  }
})();
