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
    const png = draft.find((item) => item.name.endsWith("-orchard-notice.png"));
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
    const composer = document.querySelector(".composer");
    const pngCard = [...composer.querySelectorAll(".attachment-card")]
      .find((card) => card.getAttribute("aria-label").startsWith(png.name));
    const name = pngCard.querySelector(".attachment-name");
    assert(name.title.startsWith(png.name) && name.scrollWidth > name.clientWidth, "Long filename is missing truncation or its tooltip");
    const checkCardBounds = () => {
      const cards = [...composer.querySelectorAll(".attachment-card")];
      assert(cards.length === 6, "The layout fixture needs all six cards");
      for (const card of cards) {
        const bounds = card.getBoundingClientRect();
        for (const child of card.querySelectorAll(".attachment-thumb, .attachment-thumb img, .attachment-name, .attachment-status, .attachment-actions > *, .image-text textarea, .image-translate label, .image-translate input, .image-translate button, .chip-remove")) {
          const box = child.getBoundingClientRect();
          assert(box.left >= bounds.left - 1 && box.right <= bounds.right + 1,
            `${child.className || child.tagName} overflows ${card.getAttribute("aria-label")}`);
        }
        const thumb = card.querySelector(".attachment-thumb").getBoundingClientRect();
        const close = card.querySelector(".chip-remove").getBoundingClientRect();
        assert(Math.abs(thumb.top - close.top) < 1 && close.left > thumb.right,
          "Preview and remove button are not aligned");
      }
    };
    checkCardBounds();
    composer.style.width = "240px";
    checkCardBounds();
    const actions = [...pngCard.querySelectorAll(".image-actions .attachment-actions button")];
    assert(new Set(actions.map((action) => action.offsetTop)).size > 1, "Image actions do not wrap at narrow widths");
    actions.find((action) => action.textContent.trim() === "Translate").click();
    await waitFor(() => pngCard.querySelector(".image-translate"), "Translation controls did not open");
    checkCardBounds();
    actions.find((action) => action.textContent.trim() === "Translate").click();
    actions.find((action) => action.textContent.trim() === "Text").click();
    await waitFor(() => pngCard.querySelector(".image-text textarea"), "Recognized text did not open");
    checkCardBounds();
    actions.find((action) => action.textContent.trim() === "Text").click();
    composer.style.removeProperty("width");
    result.steps.push("Six cards contain filenames, metadata, previews, buttons and expanded image actions at normal and narrow widths");
    await shot("prepared");
    result.steps.push("Previews render and sending is blocked until failed files are removed");

    await click("Remove fake.png");
    await click("Remove image-only.pdf");
    await waitFor(async () => (await invoke("attachment_list", { conversationId: null })).length === 4, "Removal failed");

    const typeDraft = (value) => {
      const textarea = document.querySelector("#composer-input");
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(textarea, value);
      textarea.dispatchEvent(new Event("input", { bubbles: true }));
    };
    const invalid = "Unaccepted draft\u0000";
    typeDraft(invalid);
    await click("Send message");
    await waitFor(() => document.body.textContent.includes("without NUL characters"), "The fixture did not exercise a pre-acceptance refusal");
    await waitFor(async () => (await invoke("list_conversations", { query: "" })).length === 0, "The refused request saved a conversation");
    assert(document.querySelector("#composer-input").value === invalid, "The refused submission cleared the draft");
    await waitFor(() => document.querySelectorAll(".composer .attachment-card").length === 4, "The refused submission cleared attachments");
    assert((await invoke("attachment_list", { conversationId: null })).length === 4, "The refused submission moved the draft");

    const textCard = [...document.querySelectorAll(".composer .attachment-card")]
      .find((card) => card.getAttribute("aria-label").startsWith("orchard notes.md"));
    textCard.querySelector(".attachment-keep").click();
    await waitFor(() => textCard.querySelector(".attachment-keep").getAttribute("aria-pressed") === "true", "Could not select retention");
    typeDraft("When does the orchard team check the water tank?");
    await click("Send message");
    await waitFor(() => document.querySelector(".user-attachments li"), "The user’s accepted turn did not render");
    assert(document.querySelectorAll(".composer .attachment-card").length === 0, "Accepted files stayed visible in the composer");
    assert(button("Stop generating"), "The tray only cleared after generation completed");
    result.steps.push("A refused send kept its draft and files. Acceptance cleared the submitted cards during generation.");
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

    // Draft files moved into the conversation and the requested retention still applies.
    const bound = await invoke("attachment_list", { conversationId });
    assert(bound.length === 4 && (await invoke("attachment_list", { conversationId: null })).length === 0, "Attachments not bound");
    const notes = bound.find((item) => item.name === "orchard notes.md");
    assert(notes.saved === true && notes.retention === "conversation", "Kept attachment did not retain its encrypted snapshot");
    assert(document.querySelectorAll(".composer .attachment-card").length === 0, "Refreshing attachments restored submitted cards");
    result.steps.push("Accepted files stayed bound to the sent message and conversation with their chosen retention");

    typeDraft("And what does it say about Mondays?");
    await click("Send message");
    await waitFor(async () => {
      const { messages } = await invoke("get_conversation", { id: conversationId });
      return messages.length === 4 && messages.at(-1).state === "complete";
    }, "Follow-up failed");
    await waitFor(() => document.querySelectorAll(".user-attachments").length === 2, "Follow-up attachment chips did not render");
    const followUp = (await invoke("get_conversation", { id: conversationId })).messages.at(-1);
    assert(followUp.context.attachments.attachments.map((item) => item.id).join() === record.attachments.map((item) => item.id).join(),
      "Follow-up did not reuse the sent files");
    await click("Regenerate with current knowledge settings");
    await waitFor(async () => {
      const { messages } = await invoke("get_conversation", { id: conversationId });
      return messages.length === 5 && messages.at(-1).state === "complete";
    }, "Regenerating with the sent files failed");
    const regenerated = (await invoke("get_conversation", { id: conversationId })).messages.at(-1);
    assert(regenerated.context.attachments.attachments.map((item) => item.id).join() === record.attachments.map((item) => item.id).join(),
      "Regeneration changed the original attachment selection");
    result.steps.push("A follow-up and regeneration reused the sent files while their cards stayed out of the tray");

    await click("Attach files");
    await waitFor(() => [...document.querySelectorAll(".composer .attachment-card")].length === 3, "The new batch did not appear independently");
    await waitFor(async () => {
      const pending = await invoke("attachment_list", { conversationId: null });
      return pending.length === 3 && pending.every((item) => item.phase === "ready");
    }, "The new draft did not prepare");
    typeDraft("What is in these three files?");
    await click("Send message");
    const newReply = await waitFor(async () => {
      const { messages } = await invoke("get_conversation", { id: conversationId });
      return messages.length === 7 && messages.at(-1).state === "complete" ? messages.at(-1) : null;
    }, "New attachments were not sent");
    assert(newReply.context.attachments.attachments.length === 3
      && newReply.context.attachments.attachments.every((item) => item.name.startsWith("fresh-")),
      "New submission included previous attachments");
    assert((await invoke("attachment_list", { conversationId })).length === 7, "Sending a new batch discarded earlier attachments");
    result.steps.push("A new batch used its own three slots and preserved all seven files in the conversation");

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
    const after = await invoke("attachment_list", { conversationId }).catch(() => []);
    assert(after.length === 0, "Deleted conversation kept its attachments");
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
