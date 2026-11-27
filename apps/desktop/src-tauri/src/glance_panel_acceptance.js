// Included only by the opt-in Rust desktop test. Drives the contextual panel's
// real controls. The clipboard is owned by an external synthetic process.
void (async () => {
  const ipc = window.__TAURI_INTERNALS__;
  const root = document.documentElement;
  const CLIPBOARD = "Synthetic note: the orchard rainwater tanks collect water for dry weeks.";
  const result = { pass: false, steps: [] };
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const until = async (probe, what, ms = 15000) => {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const value = probe();
      if (value) return value;
      await sleep(50);
    }
    throw new Error(`Timed out waiting for ${what}`);
  };
  const failure = async (promise) => {
    try {
      await promise;
    } catch (error) {
      return error;
    }
    throw new Error("Expected a failure");
  };
  const shot = async (stage) => {
    await sleep(250);
    root.dataset.glanceStage = stage;
    await until(() => root.dataset.glanceShot === stage, `screenshot ${stage}`, 20000);
    return Number(root.dataset.remoteSeen);
  };
  const button = (label) => [...document.querySelectorAll("button")]
    .find((item) => item.textContent.trim().startsWith(label) && !item.disabled);
  const click = async (label) => (await until(() => button(label), `enabled button "${label}"`)).click();
  const set = (element, value) => {
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value").set;
    setter.call(element, value);
    element.dispatchEvent(new Event(element.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
  };
  const source = () => document.getElementById("glance-source");
  const select = (label) => [...document.querySelectorAll("label")]
    .find((item) => item.textContent.trim().startsWith(label))?.querySelector("select");
  const output = () => document.querySelector(".glance-output")?.textContent ?? "";
  const receipt = () => JSON.parse(document.querySelector(".glance-receipt pre")?.textContent || "null");
  const check = (label) => [...document.querySelectorAll(".glance-choice")]
    .find((item) => item.textContent.includes(label))?.querySelector("input");
  const toggle = (label, on) => { const box = check(label); assert(box && !box.disabled, `${label} unavailable`); if (box.checked !== on) box.click(); };
  try {
    await until(() => select("Process with")?.options.length === 2, "panel state with one provider");
    assert(source().value === "", "Opening the panel captured text");
    assert(document.activeElement === source(), "The source editor did not take focus");
    for (const command of ["bootstrap", "memory_list", "provider_list", "send_message", "glance_show", "set_intelligence_settings"]) {
      const denied = await failure(ipc.invoke(command, {}));
      assert(String(denied?.message ?? denied).toLowerCase().includes("not allowed"), `Panel reached ${command}: ${JSON.stringify(denied)}`);
    }
    result.steps.push("opened empty and focused; main-window commands are denied by the panel capability");

    await click("Use selection");
    await until(() => document.body.textContent.includes("Selection capture is unavailable"), "selection notice");
    assert(source().value === "", "Use selection read text");
    assert(await shot("01-open") === 0, "A request left before any action");
    result.steps.push("selection capture stays disabled and explains the paste path");

    await click("Read clipboard");
    await until(() => source().value === CLIPBOARD, "clipboard text in the editable preview");
    assert(document.querySelector(".glance-section-head span").textContent.startsWith("Clipboard"), "Clipboard origin not shown");
    set(select("Action"), "key_points");
    document.querySelector(".glance-context").open = true;
    toggle("Use saved memories", true);
    toggle("Synthetic orchard plan", true);
    await shot("02-clipboard-preview");
    result.steps.push("Read clipboard filled the editable preview from the external owner; nothing processed yet");

    await click("Extract key points");
    await until(() => receipt()?.state === "completed", "local result", 20000);
    document.querySelector(".glance-receipt").open = true;
    const local = receipt();
    assert(output() === "Local fixture key points: the tanks collect rainwater; memory supplied; document supplied.", `Local output ${output()}`);
    assert(local.external.length === 0 && local.application_retention === "session", "Local receipt is wrong");
    const kinds = local.used.map((item) => item.kind);
    for (const kind of ["clipboard", "memory", "knowledge"]) assert(kinds.includes(kind), `Receipt omits ${kind}: ${kinds}`);
    document.querySelector(".glance-receipt").scrollIntoView({ block: "end" });
    assert(await shot("03-local-result") === 0, "Local processing reached the provider");
    await click("Remember");
    await until(() => document.body.textContent.includes("Result saved to memory."), "remember notice");
    const again = await failure(ipc.invoke("glance_remember"));
    assert(again.code === "invalid_config", "The same result was remembered twice");
    result.steps.push("local key points used the clipboard, memory and document; receipt matches; remembered once");

    set(select("Process with"), select("Process with").options[1].value);
    document.querySelector(".glance-context").open = true;
    toggle("Use saved memories", true);
    assert(document.body.textContent.includes("This provider’s settings withhold documents."), "Documents offered to a provider that withholds them");
    await click("Review request");
    await until(() => document.querySelector(".glance-review"), "sharing review");
    assert(await shot("04-remote-review") === 0, "A request left before Send once");
    const unconfirmed = await failure(ipc.invoke("glance_run", {
      input: { text: CLIPBOARD, recipe: "explain", provider: Number(select("Process with").value), revision: "forged", share: true, memory: false, documents: [] },
      deltas: `__CHANNEL__:${ipc.transformCallback(() => {})}`,
    }));
    assert(unconfirmed.code === "policy_denied", "A stale provider revision was accepted");
    result.steps.push("remote request waits for a per-request review; a stale revision is refused");

    await click("Send once");
    await until(() => receipt()?.state === "completed", "remote result", 20000);
    document.querySelector(".glance-receipt").open = true;
    const remote = receipt();
    assert(output() === "Remote fixture answer", `Remote output ${output()}`);
    assert(remote.external.length === 1 && remote.external[0].transmission === "sent", "Remote receipt has no transmission");
    assert(!remote.used.some((item) => item.kind === "knowledge"), "Receipt reports a withheld document as used");
    document.querySelector(".glance-receipt").scrollIntoView({ block: "end" });
    assert(await shot("05-remote-result") === 1, "Expected exactly one provider request");
    result.steps.push("one confirmed remote request; receipt records the sent transmission and no document");

    await click("Copy");
    await until(() => document.body.textContent.includes("Result copied."), "copy notice");
    await click("Read clipboard");
    await until(() => source().value === "Remote fixture answer", "copied text read back");
    result.steps.push("Copy placed only the answer on the clipboard");

    set(select("Process with"), "");
    set(source(), "HOLD_LOCAL synthetic text for Stop.");
    set(select("Action"), "explain");
    await click("Explain");
    await until(() => output().startsWith("Holding"), "streaming delta");
    await shot("06-streaming");
    await click("Stop");
    await until(() => document.querySelector(".glance-error")?.textContent.includes("stopped"), "stopped state", 10000);
    assert(receipt()?.state === "cancelled", "Stopped receipt is not cancelled");
    assert(output() === "Holding the synthetic request", "The interrupted response was not kept");
    assert(!button("Remember"), "A stopped result can be remembered");
    await shot("07-stopped");
    result.steps.push("Stop cancelled streaming; the partial response stays visible, the receipt is cancelled and Remember is unavailable");
    result.pass = true;
  } catch (error) {
    result.error = String(error?.message ?? JSON.stringify(error));
    result.visibleError = document.querySelector(".glance-error")?.textContent ?? null;
    result.page = { href: location.href, root: document.getElementById("root")?.innerHTML.slice(0, 300) ?? null,
      scripts: [...document.scripts].map((item) => item.src), sheets: document.styleSheets.length };
    result.state = await ipc.invoke("glance_state").catch((e) => ({ failed: e }));
    await shot("99-failure").catch(() => {});
  }
  root.dataset.glanceResult = JSON.stringify(result);
})();
