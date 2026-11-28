// Runs the packaged WebView against a disposable profile and a loopback search fixture.
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
    assert(initial.settings.provider === "purili" && initial.settings.puriliPersonalUse, "Unexpected fixture search settings");
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
    await waitFor(() => !button("Send message")?.disabled, "Search did not use the configured provider and model");

    await selectMode("Search · Search and answer", "Read a webpage");
    set(composer.querySelector('input[type="url"]'), "https://localhost/");
    await waitFor(() => composer.textContent.includes("Only public websites"), "Private page address was accepted");
    assert(button("Send message")?.disabled, "An unsafe page can be submitted");
    set(composer.querySelector('input[type="url"]'), "https://example.org/article");
    await waitFor(() => !button("Send message")?.disabled, "Public page requires a search provider");
    await selectMode("Search · Read a webpage", "Search and answer");
    result.steps.push("Search menu supports keyboard navigation and validates direct webpage entry");

    const coordinates = () => {
      const bounds = composer.getBoundingClientRect();
      const send = composer.querySelector(".send-button").getBoundingClientRect();
      const actions = composer.querySelector(".composer-toolbar").getBoundingClientRect();
      assert(Math.abs(send.bottom - actions.bottom) < 1 && send.left >= actions.right, "Send wrapped into the toolbar");
      return Math.round(bounds.right - send.right);
    };
    assert(!button("Thinking"), "The composer exposed Thinking for structured research steps");
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

    await click("Add search focus");
    set(composer.querySelector('input[placeholder="Search focus"]'), "Edited fixture focus");
    await send();
    const finished = async (count) => waitFor(async () => {
      const [thread] = await invoke("list_conversations", { query: "" });
      if (!thread) return null;
      const { messages } = await invoke("get_conversation", { id: thread.id });
      const answers = messages.filter((message) => message.role === "assistant" && message.state === "complete");
      return answers.length === count ? invoke("chat_web_research", { messageId: answers.at(-1).id }) : null;
    }, "Search turn did not finish");
    const answer = await finished(1);
    assert(answer.record.synthesis === "none", "The fixture claimed to answer without evidence");
    assert(answer.record.provider === "purili" && answer.attempts.length === 2, "Two bounded attempts were not recorded");
    assert(answer.attempts[0].query === "municipal bridge construction files" && answer.attempts[1].query === "railway crossing access timetable",
      "The composed searches did not use the selected model's two queries");
    const details = await waitFor(() => [...document.querySelectorAll("details")].find(
      (element) => element.querySelector("summary")?.textContent.trim() === "Search details"), "Search details did not render");
    assert(!details.open, "Search details opened automatically");
    details.querySelector("summary").click();
    assert(details.textContent.includes("railway crossing access timetable"), "Retry query is missing from Search details");
    await selectMode("Search", "Search results only");
    set(textarea, "Typed fixture results only");
    await send();
    const results = await finished(2);
    assert(results.record.provider === "purili" && results.query === "historical bridge opening dates" && results.record.synthesis === "none",
      "Results-only mode did not send the model's query without synthesis");
    assert(results.sources.length === 1 && results.sources[0].title === "Historic bridge", "The relevant result was not displayed");
    result.steps.push("Two generated attempts are visible in collapsed details and results-only lists sources without an answer");
    result.pass = true;
  } catch (error) {
    result.error = error?.message || String(error);
  } finally {
    document.documentElement.dataset.researchResult = JSON.stringify(result);
  }
})();
