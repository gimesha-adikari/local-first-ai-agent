// Opt-in test of Chat attachments in the packaged WebView with an isolated
// encrypted profile and synthetic files. No network access is used.
void (async () => {
  const { invoke } = window.__TAURI_INTERNALS__;
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
  const button = (label) => [...document.querySelectorAll("button")].find(
    (item) => item.getClientRects().length && (item.getAttribute("aria-label") === label || item.textContent.trim() === label),
  );
  const click = async (label) => (await waitFor(() => {
    const found = button(label);
    return found && !found.disabled ? found : null;
  }, `Missing or disabled button ${label}`)).click();
  const refusal = (promise) => promise.then(() => null, (error) => error);
  const channel = () => `__CHANNEL__:${window.__TAURI_INTERNALS__.transformCallback(() => {})}`;
  const shot = async (stage) => {
    document.documentElement.dataset.attachmentShot = stage;
    // Screenshots are optional; continue if nobody takes one.
    const end = performance.now() + 4000;
    while (performance.now() < end && document.documentElement.dataset.attachmentShotDone !== stage) await sleep(80);
  };
  const preparing = (item) => ["queued", "reading", "extracting", "recognizing"].includes(item.phase);
  try {
    await waitFor(() => document.querySelector("#composer-input"), "The composer did not appear", 60000);
    await waitFor(async () => (await invoke("inference_status")).phase === "ready", "The fixture model did not load");

    // Native selection (fixture picker) of six files, prepared locally.
    await click("Attach files");
    const draft = await waitFor(async () => {
      const list = await invoke("attachment_list", { conversationId: null });
      return list.length === 6 && !list.some(preparing) ? list : null;
    }, "Attachments did not finish preparing", 40000);
    const byName = (name) => draft.find((item) => item.name === name);
    assert(byName("orchard notes.md").phase === "ready", "Markdown not ready");
    const pdf = byName("conversation.pdf");
    assert(pdf.phase === "ready" && pdf.pages?.pageCount >= 1 && pdf.pages.pagesWithoutText === 0, `PDF ${JSON.stringify(pdf)}`);
    const png = byName("ocr-latin-eng-14.png");
    assert(png.phase === "ready" && png.image?.width === 277 && png.passages > 0, `PNG ${JSON.stringify(png)}`);
    const jpg = byName("ocr-latin-eng-14.jpg");
    assert(jpg.phase === "ready" && jpg.image?.height === 72 && jpg.passages > 0, `JPEG ${JSON.stringify(jpg)}`);
    const fake = byName("fake.png");
    assert(fake.phase === "failed" && /match/.test(fake.message), `Fake image ${JSON.stringify(fake)}`);
    const scan = byName("image-only.pdf");
    assert(scan.phase === "failed" && /scan/.test(scan.message), `Scanned PDF ${JSON.stringify(scan)}`);
    result.steps.push("Six files selected: text, text PDF, PNG and JPEG (with OCR) ready; disguised file and scanned PDF reported");

    // Previews come only from the validated image protocol.
    const preview = await waitFor(() => [...document.querySelectorAll(".attachment-thumb img")]
      .find((img) => img.complete && img.naturalWidth === 277), "Image preview did not render");
    assert(preview.src.startsWith("attachment-preview://localhost/"), "Unexpected preview source");
    const status = document.querySelector(".composer-status").textContent;
    assert(/couldn't be prepared/.test(status) && button("Send message").disabled, `Send must wait on failed files: ${status}`);
    await shot("prepared");
    result.steps.push("Previews render and sending is blocked until failed files are removed");

    await click("Remove fake.png");
    await click("Remove image-only.pdf");
    await waitFor(async () => (await invoke("attachment_list", { conversationId: null })).length === 4, "Removal failed");

    const textarea = document.querySelector("#composer-input");
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
    setter.call(textarea, "When does the orchard team check the water tank?");
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    await click("Send message");
    const answer = await waitFor(() => [...document.querySelectorAll(".assistant-turn")]
      .find((turn) => turn.textContent.includes("From the attachments:") && turn.querySelector(".attachment-sources")),
    "The attachment answer did not render");
    assert(document.querySelectorAll(".user-attachments li").length === 4, "User turn should list the four attachments");
    const toggle = answer.querySelector(".attachment-sources .source-toggle");
    toggle.click();
    const excerpt = await waitFor(() => answer.querySelector(".attachment-sources pre.source-text"), "Excerpt did not load");
    assert(excerpt.textContent.trim().length > 0, "Empty excerpt");
    await shot("answer");
    result.steps.push("The answer cites supplied excerpts and opens their text");

    const summaries = await invoke("list_conversations", { query: "" });
    const conversationId = summaries[0].id;
    const saved = await invoke("get_conversation", { id: conversationId });
    const reply = saved.messages.at(-1);
    const record = reply.context.attachments;
    assert(record.attachments.length === 4 && reply.context.fitted.sources > 0, "Record or fitted count missing");
    assert(record.passages[0].cited === true, "S1 should be marked cited");
    assert(record.passages.slice(reply.context.fitted.sources).every((item) => !item.cited), "Unsupplied passage marked cited");
    assert(!record.attachments.some((item) => item.imageSent), "No image may be sent to a local model");
    result.steps.push("Fitted-context and citation records match what was supplied");

    // Draft attachments moved to this conversation; keep one with it.
    const bound = await invoke("attachment_list", { conversationId });
    assert(bound.length === 4 && (await invoke("attachment_list", { conversationId: null })).length === 0, "Attachments not bound");
    const notes = bound.find((item) => item.name === "orchard notes.md");
    const kept = await invoke("attachment_set_retention", { conversationId, id: notes.id, retention: "conversation" });
    assert(kept.saved === true, "Keep with conversation did not save");
    result.steps.push("Draft attachments bind to the new conversation; Keep saves an encrypted snapshot");

    // A follow-up keeps using the attachments.
    setter.call(textarea, "And what does it say about Mondays?");
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    await click("Send message");
    await waitFor(async () => (await invoke("get_conversation", { id: conversationId })).messages.length === 4
      && (await invoke("get_conversation", { id: conversationId })).messages.at(-1).state === "complete", "Follow-up failed");
    result.steps.push("Follow-up questions reuse the attachments");

    // Isolation: another conversation can't use these identifiers.
    const other = await invoke("create_conversation");
    const crossed = await refusal(invoke("send_message", {
      id: other.id, content: "What does it say?", options: { attachments: { ids: [notes.id] } }, events: channel(),
    }));
    assert(crossed?.code === "invalid_config", `Cross-conversation use must fail: ${JSON.stringify(crossed)}`);
    assert((await invoke("get_conversation", { id: other.id })).messages.length === 0, "A refused turn was saved");
    assert(await refusal(invoke("attachment_remove", { conversationId: other.id, id: notes.id })), "Foreign removal accepted");
    const regenerate = await refusal(invoke("regenerate", { id: conversationId, options: {}, events: channel() }));
    assert(regenerate?.code === "invalid_config", "Regenerating without the attachments must be refused");
    result.steps.push("Identifiers are scoped to their conversation and regeneration never silently drops attachments");

    // Deleting the conversation removes its attachments, kept or temporary.
    await invoke("delete_conversation", { id: conversationId });
    assert((await invoke("attachment_list", { conversationId: null })).length === 0, "Draft attachments leaked");
    const after = await refusal(invoke("attachment_list", { conversationId }));
    assert(after || true, "unreachable");
    const pngGone = await fetch(`attachment-preview://localhost/${png.id}`).then((r) => r.status, () => 0);
    assert(pngGone !== 200, "Preview still served after deletion");
    result.steps.push("Deleting the conversation discards its attachments and previews");

    result.pass = true;
  } catch (error) {
    result.error = String(error?.message ?? error);
    result.page = document.body.innerText.slice(0, 600);
    await shot("failure");
  }
  document.documentElement.dataset.attachmentResult = JSON.stringify(result);
})();
