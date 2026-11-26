// Synthetic data exercised over the packaged local WebView with a real model.
void (async () => {
  const ipc = window.__TAURI_INTERNALS__;
  const invoke = (name, args) => ipc.invoke(name, args);
  const results = {};
  const check = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const channel = (receive) => `__CHANNEL__:${ipc.transformCallback((item) => {
    if ("message" in item) receive(item.message);
  })}`;
  const newChat = async () => (await invoke("create_conversation")).id;
  const send = async (id, content, options = {}, onEvent = () => {}) => {
    const events = [];
    const answer = await invoke("send_message", {
      id, content, options, events: channel((event) => {
        events.push(event);
        onEvent(event);
      }),
    });
    return { answer, coverage: events.find((event) => event.kind === "knowledge")?.coverage };
  };
  const references = (messageId) => invoke("chat_source_references", { messageId });
  const resolve = (messageId, citationId) => invoke("chat_resolve_source", { messageId, citationId });
  const failure = async (promise, code) => {
    try {
      await promise;
    } catch (error) {
      check(error.code === code, `Expected ${code}, got ${JSON.stringify(error)}`);
      return;
    }
    throw new Error(`Expected ${code}`);
  };
  try {
    const { models: [model] } = await invoke("bootstrap");
    const loaded = await invoke("load_model", { modelId: model.id, settings: model.settings });
    results.model = loaded.fileName;
    const docs = await invoke("knowledge_list_documents", { limit: 10 });
    const orchard = docs.find((doc) => doc.name === "orchard.pdf").id;
    const large = docs.find((doc) => doc.name === "large.txt").id;
    const dessert = docs.find((doc) => doc.name === "dessert.txt").id;
    const knowledge = { mode: "keyword", documentIds: [orchard] };
    const id = await newChat();

    await failure(send(id, "What is the orchard gate code?", {
      knowledge: { ...knowledge, mode: "semantic" },
    }), "embedding_index_unavailable");
    check((await invoke("get_conversation", { id })).messages.length === 0, "Failed preparation wrote a turn");

    const answer = await send(id, "What code opens the orchard gate?", { knowledge });
    results.grounded = answer.answer.content;
    const groundedCode = (text) => text.includes("FERN-742") && !text.includes("PURPLE-391")
      && (!text.includes("ORANGE-999")
        || /ORANGE-999.{0,30}(?:not|incorrect)|(?:not|incorrect).{0,20}ORANGE-999/i.test(text));
    check(groundedCode(answer.answer.content), "Grounding, selection or adversarial evidence failure");
    check(answer.coverage.retrieved === 1 && answer.coverage.partialDocumentIds[0] === orchard, "Partial document coverage missing");
    const refs = await references(answer.answer.id);
    check(refs.length === 1 && refs[0].documentId === orchard && refs[0].revision === 1
      && refs[0].chunkOrdinal === 0 && refs[0].source.page === 2 && refs[0].available, "Verified citation metadata missing");
    check((await resolve(answer.answer.id, "S1")).includes("FERN-742"), "Citation did not resolve");
    check(await resolve(answer.answer.id, "S2") === null, "Unsupplied source resolved");

    for (const documentId of [orchard, dessert]) {
      const jobId = await invoke("knowledge_reindex", { documentId });
      for (;;) {
        const progress = await invoke("knowledge_index_progress", { jobId });
        if (progress.phase === "completed") break;
        check(!["failed", "cancelled"].includes(progress.phase), JSON.stringify(progress));
        await new Promise((done) => setTimeout(done, 25));
      }
    }
    const paraphrase = await send(await newChat(), "How should I cool a citrus dessert?", {
      knowledge: { mode: "semantic", documentIds: [dessert, large] },
    });
    results.paraphrase = paraphrase.answer.content;
    check(/chill|refrigerat/i.test(paraphrase.answer.content), "Semantic paraphrase was not grounded");
    check(paraphrase.coverage.embeddings.indexedDocuments === 1 && paraphrase.coverage.embeddings.missingDocuments === 1,
      "Partial vector coverage missing");

    const adversarial = await send(await newChat(), "Which passphrase lets visitors enter?", {
      knowledge: { mode: "semantic", documentIds: [orchard, dessert] },
    });
    results.adversarialParaphrase = adversarial.answer.content;
    results.adversarialParaphraseGrounded = groundedCode(adversarial.answer.content);
    const hybrid = await send(await newChat(), "What is the orchard entrance code?", {
      knowledge: { ...knowledge, mode: "hybrid" },
    });
    results.hybrid = hybrid.answer.content;
    check(hybrid.coverage.mode === "hybrid" && hybrid.answer.content.includes("FERN-742"), "Hybrid Chat was not grounded");

    const absent = await send(await newChat(), "How much does the orchard gate weigh?", { knowledge });
    results.unanswerable = absent.answer.content;
    const abstains = (text) => /not|no (?:information|document evidence)|unknown|cannot|can't|doesn't|isn't/i.test(text);
    check(absent.coverage.retrieved === 1 && abstains(absent.answer.content), "Answer should acknowledge missing evidence");

    const none = await send(await newChat(), "How many moons orbit Neptune?", { knowledge });
    results.noResults = none.answer.content;
    check(none.coverage.retrieved === 0 && abstains(none.answer.content), "No-result state or response missing");
    check((await references(none.answer.id)).length === 0, "No-result answer resolved a citation");

    const bounded = await send(await newChat(), "What does the large file say?", {
      knowledge: { mode: "keyword", documentIds: [large] }, maxTokens: 600,
    });
    const budget = bounded.answer.stats.outputBudget;
    check(bounded.coverage.retrieved === 1 && bounded.answer.stats.contextUsage.sources === 0
      && budget.effective === 600 && bounded.answer.stats.promptTokens + budget.effective <= budget.contextLength,
    "Context fit did not preserve the explicit output budget");
    results.budget = { ...budget, ...bounded.answer.stats.contextUsage };

    const truncatedChat = await newChat();
    const truncated = await send(truncatedChat, "State the orchard gate code in a sentence.", { knowledge, maxTokens: 1 });
    check(truncated.answer.state === "truncated", "Expected contextual truncation");
    await failure(invoke("chat_continuation_remaining", {
      id: truncatedChat, messageId: truncated.answer.id,
    }), "invalid_config");

    let stopped;
    const cancelled = await send(await newChat(), "Explain the orchard gate code in great detail.", { knowledge }, (event) => {
      if (event.kind === "started") stopped = invoke("stop_generation");
    });
    check(await stopped && cancelled.answer.state === "cancelled", "Contextual cancellation did not stop native generation");

    const memory = await invoke("memory_create", { content: "My preferred tea is hibiscus." });
    check(await invoke("memory_retrieval_enabled") === false, "Memory was enabled by default");
    await invoke("memory_set_retrieval_enabled", { enabled: true });
    const other = await newChat();
    const recalled = await send(other, "Which tea do I prefer?");
    results.memory = recalled.answer.content;
    check(/hibiscus/i.test(recalled.answer.content) && recalled.answer.stats.contextUsage.memories === 1, "Cross-conversation recall failed");
    check((await references(recalled.answer.id)).length === 0, "Memory resolved as a document citation");
    await invoke("memory_set_retrieval_enabled", { enabled: false });
    const disabled = await send(other, "Which tea do I prefer?", { maxTokens: 24 });
    results.disabled = disabled.answer.content;
    check(!/hibiscus/i.test(disabled.answer.content) && !disabled.answer.stats.contextUsage, "Disabled memory leaked through history");
    await invoke("memory_edit", { id: memory.id, content: "My preferred tea is oolong." });
    check((await invoke("memory_list", { query: "OOLONG", limit: 10 }))[0].id === memory.id, "Edited memory search failed");
    await invoke("memory_delete", { id: memory.id });
    check((await invoke("memory_list", { limit: 10 })).length === 0, "Memory deletion failed");
    await invoke("memory_create", { content: "Saved explicitly for the clear-all check." });
    check(await invoke("memory_clear") === 1 && (await invoke("memory_list", { limit: 10 })).length === 0,
      "Clearing memories failed");

    await invoke("knowledge_delete_document", { documentId: orchard });
    check(!(await references(answer.answer.id))[0].available, "Deleted source stayed available");
    check(await resolve(answer.answer.id, "S1") === null, "Deleted source resolved");
    const regenerated = await invoke("regenerate", { id, options: { knowledge }, events: channel(() => {}) });
    check(!regenerated.content.includes("FERN-742"), "Regeneration reused deleted evidence");
    check((await references(regenerated.id)).length === 0, "Regeneration restored deleted citation");
    await invoke("knowledge_unload_embedding_model");
    await failure(send(await newChat(), "What is the orchard gate code?", {
      knowledge: { ...knowledge, mode: "semantic" },
    }), "embedding_unavailable");
    check((await invoke("list_agent_scopes")).length === 0, "Context created a filesystem permission");
    document.documentElement.dataset.contextualChatIpcResult = JSON.stringify({ pass: true, ...results });
  } catch (error) {
    document.documentElement.dataset.contextualChatIpcResult = JSON.stringify({
      pass: false, error: error.message || JSON.stringify(error), ...results,
    });
  }
})();
