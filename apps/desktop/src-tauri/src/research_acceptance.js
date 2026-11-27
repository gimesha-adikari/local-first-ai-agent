// Runs the packaged WebView against a loopback search fixture with no public results.
void (async () => {
  const { invoke, transformCallback } = window.__TAURI_INTERNALS__;
  const result = { pass: false, steps: [] };
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const waitFor = async (find, message) => {
    const end = performance.now() + 6500;
    while (performance.now() < end) {
      const match = await find();
      if (match) return match;
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
    throw new Error(message);
  };
  const button = (text) => [...document.querySelectorAll(text === "Search" ? ".composer button" : "button")].find(
    (item) => item.getClientRects().length && (item.textContent.trim() === text || item.getAttribute("aria-label") === text
      || item.getAttribute("title") === text),
  );
  const click = async (text) => (await waitFor(() => {
    const found = button(text);
    return found && !found.disabled ? found : null;
  }, `Missing or disabled button ${text}`)).click();
  const set = (element, value) => {
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value").set;
    setter.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const refusal = (promise) => promise.then(() => null, (error) => error);
  const send = () => click("Send message");
  const selectMode = async (current, next) => {
    await click(current);
    await click(next);
  };
  try {
    const initial = await invoke("web_research_settings");
    assert(initial.settings.provider === null && initial.braveKey !== undefined, "Unexpected first-run search settings");
    const composer = await waitFor(() => document.querySelector(".composer"), "The composer did not load");
    const textarea = composer.querySelector("#composer-input");

    await click("Search");
    await waitFor(() => document.activeElement.textContent.trim() === "Search and answer", "Search menu did not take focus");
    document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    assert(document.activeElement.textContent.trim() === "Search results only", "Arrow key did not move between search modes");
    document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await waitFor(() => document.activeElement === button("Search") && !document.querySelector('[role="menu"]'), "Escape did not return focus");
    await selectMode("Search", "Search and answer");
    set(textarea, "Fixture question");
    await waitFor(() => button("Set up search") && button("Send message")?.disabled, "Search without a provider missed its setup action");

    await selectMode("Search · Search and answer", "Read a webpage");
    set(composer.querySelector('input[type="url"]'), "https://localhost/");
    await waitFor(() => composer.textContent.includes("Only public websites"), "Private page address was accepted");
    assert(button("Send message")?.disabled, "An unsafe page can be submitted");
    set(composer.querySelector('input[type="url"]'), "https://example.org/article");
    await waitFor(() => !button("Send message")?.disabled, "Public page requires a search provider");
    await selectMode("Search · Read a webpage", "Search and answer");
    result.steps.push("Search menu supports keyboard navigation, a compact setup action and provider-free public webpage entry");

    const unsafe = await refusal(invoke("web_research_save", {
      settings: { provider: "searxng", braveSaveResults: false, searxngEndpoint: "http://169.254.169.254", searxngSaveResults: false },
    }));
    assert(unsafe?.code === "invalid_config", "A metadata-service SearXNG address was accepted");
    const saved = await invoke("web_research_save", {
      settings: { provider: "searxng", braveSaveResults: false, searxngEndpoint: "http://127.0.0.1:__PORT__/", searxngSaveResults: false },
    });
    assert(saved.settings.searxngEndpoint === "http://127.0.0.1:__PORT__" && saved.searxngNetwork === "loopback", "SearXNG was not canonicalized");

    await click("Set up search");
    await waitFor(() => document.body.textContent.includes("http://127.0.0.1:__PORT__ · This computer"), "Saved SearXNG address not shown");
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
    assert(chosen.provider === "purili" && chosen.puriliPersonalUse, "Purili confirmation was not stored");
    consent.click();
    await waitFor(() => button("None")?.getAttribute("aria-checked") === "true", "Withdrawing confirmation kept Purili selected");
    await click("SearXNG");
    await waitFor(() => button("SearXNG")?.getAttribute("aria-checked") === "true", "Could not return to the loopback search provider");
    await click("Close");
    result.steps.push("Purili selection requires the stored personal-use confirmation");

    const coordinates = () => {
      const bounds = composer.getBoundingClientRect();
      const send = composer.querySelector(".send-button").getBoundingClientRect();
      const actions = composer.querySelector(".composer-toolbar").getBoundingClientRect();
      assert(Math.abs(send.bottom - actions.bottom) < 1 && send.left >= actions.right, "Send wrapped into the toolbar");
      return Math.round(bounds.right - send.right);
    };
    assert(button("Thinking"), "The layout fixture is missing the Thinking control");
    const inset = coordinates();
    composer.style.width = "310px";
    assert(composer.querySelector(".composer-toolbar").getBoundingClientRect().height > 35, "Controls did not wrap at the narrow width");
    assert(coordinates() === inset, "Send moved from the right edge when controls wrapped");
    composer.style.removeProperty("width");
    result.steps.push("Send stays at the same right inset when the full Chat toolbar wraps");

    const conversation = await invoke("create_conversation");
    const channel = `__CHANNEL__:${transformCallback(() => {})}`;
    const badPage = await refusal(invoke("send_message", {
      id: conversation.id, content: "Read it",
      options: { research: { mode: "page", url: "https://localhost/" } }, events: channel,
    }));
    assert(badPage?.code === "invalid_config", "The engine accepted a private direct page");
    assert(!(await invoke("get_conversation", { id: conversation.id })).messages.length, "The refused page saved a turn");
    await invoke("delete_conversation", { id: conversation.id });

    await click("Edit query");
    set(composer.querySelector('input[placeholder="Search query"]'), "Edited fixture query");
    await send();
    const finished = async (count) => waitFor(async () => {
      const [thread] = await invoke("list_conversations", { query: "" });
      if (!thread) return null;
      const { messages } = await invoke("get_conversation", { id: thread.id });
      const answers = messages.filter((message) => message.role === "assistant" && message.state === "complete");
      return answers.length === count ? invoke("chat_web_research", { messageId: answers.at(-1).id }) : null;
    }, "Search turn did not finish");
    const answer = await finished(1);
    assert(answer.record.synthesis === "local", "Search and answer did not select the local model");
    assert(answer.record.provider === "searxng" && answer.query === "Edited fixture query", "Edited query did not reach the fixture");
    await selectMode("Search", "Search results only");
    set(textarea, "Typed results only");
    await send();
    const results = await finished(2);
    assert(results.record.provider === "searxng" && results.query === "Typed results only" && results.record.synthesis === "none",
      "Results-only mode did not send the typed query without synthesis");
    result.steps.push("Edited Search and answer and typed results-only both completed via the Rust boundary without a confirmation");
    result.pass = true;
  } catch (error) {
    result.error = error?.message || String(error);
  } finally {
    document.documentElement.dataset.researchResult = JSON.stringify(result);
  }
})();
