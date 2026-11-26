// Opt-in IPC and performance checks on a synthetic, maximum-size library.
void (async () => {
  const ipc = window.__TAURI_INTERNALS__;
  const invoke = (name, args) => ipc.invoke(name, args);
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const check = (ok, message) => { if (!ok) throw new Error(message); };
  const result = {};
  const synthetic = (s) => document.documentElement.dataset.task05Stage = s;
  const channel = (part) => `__CHANNEL__:${ipc.transformCallback(({ message }) => {
    if (message) part(message);
  })}`;
  const reject = async (promise, code) => {
    try { await promise; } catch (error) {
      check(error.code === code, JSON.stringify(error));
      return;
    }
    throw new Error(`Expected ${code}`);
  };
  const clock = async (fn, count) => {
    const ms = [];
    for (let i = 0; i < count; i++) {
      const start = performance.now();
      await fn();
      ms.push(performance.now() - start);
    }
    return ms.sort((a, b) => a - b).map((n) => Math.round(n * 100) / 100);
  };
  const library = () => invoke("knowledge_library");
  const search = (mode, query, documentIds) => invoke("knowledge_retrieve", {
    mode, request: { query, documentIds, limit: 5 },
  });
  const refs = (messageId) => invoke("chat_supplied_evidence", { messageId });

  async function terminal(jobId) {
    for (let i = 0; i < 300; i++) {
      const state = await invoke("knowledge_index_progress", { jobId });
      if (["completed", "cancelled", "failed"].includes(state.phase)) return state;
      await wait(30);
    }
    throw new Error("Index job timed out");
  }

  async function indexed(documentId) {
    const progress = await terminal(await invoke("knowledge_reindex", { documentId }));
    check(progress.phase === "completed", JSON.stringify(progress));
    return progress.windowCount;
  }

  try {
    synthetic("baseline");
    const docs = await library();
    check(docs.length === 128 && docs.every((d) => d.revision === 1 && d.embeddingState === "missing"), "Maximum snapshot failed");
    const ids = docs.map(({ id }) => id);
    result.bgeLoadMs = Number(document.documentElement.dataset.bgeLoadMs);
    result.baselineRefreshMs = await clock(async () => {
      const first = await invoke("knowledge_list_documents", { limit: 100 });
      const second = await invoke("knowledge_list_documents", { limit: 100, beforeId: first.at(-1).id });
      const coverage = await Promise.all([...first, ...second].map((d) =>
        invoke("knowledge_embedding_coverage", { documentIds: [d.id] })));
      check(coverage.length === 128, "Baseline work was incomplete");
    }, 5);
    result.batchRefreshMs = await clock(async () => {
      check((await library()).length === 128, "Batch work was incomplete");
    }, 5);

    synthetic("index");
    const beforeIndex = performance.now();
    result.windows = await indexed(1) + await indexed(3);
    result.indexMsIncludingPoll = Math.round(performance.now() - beforeIndex);
    const indexJob = await invoke("knowledge_reindex", { documentId: 4 });
    check(await invoke("knowledge_cancel_index", { jobId: indexJob }), "Index cancellation rejected");
    check((await terminal(indexJob)).phase === "cancelled", "Index did not stop");
    const after = await library();
    check(after.filter((doc) => doc.embeddingState === "ready").length === 2
      && after.find((doc) => doc.id === 4).embeddingState === "missing", "Cancelled index became usable");

    synthetic("retrieval");
    for (const [mode, query] of [["keyword", "orchard"], ["semantic", "where can I keep a citrus dessert cold"], ["hybrid", "orchard"]]) {
      const sample = await search(mode, query, ids);
      check(sample.hits.length > 0 && sample.mode === mode, `Missing ${mode} matches`);
      if (mode !== "keyword") check(sample.coverage.indexedDocuments === 2 && sample.coverage.missingDocuments === 126, "Wrong mixed coverage");
      result[`${mode}Ms`] = await clock(() => search(mode, query, ids), 9);
    }
    await reject(search("keyword", "orchard", [...ids, ids[0]]), "invalid_arguments");

    synthetic("chat");
    const { models: [model] } = await invoke("bootstrap");
    result.chatModelLoadMs = (await clock(() => invoke("load_model", { modelId: model.id, settings: model.settings }), 1))[0];
    const { id } = await invoke("create_conversation");
    const options = { knowledge: { mode: "keyword", documentIds: [1], grounding: "documents" } };
    let started;
    const launch = performance.now();
    const answer = await invoke("send_message", {
      id, content: "What is the orchard gate code?", options,
      events: channel((event) => { if (event.kind === "started") started = performance.now(); }),
    });
    result.preparationMs = Math.round((started - launch) * 100) / 100;
    result.answer = answer.content;
    const evidence = await refs(answer.id);
    check(answer.content.includes("FERN-742") && !answer.content.includes("PURPLE-391"), "Wrong selected answer");
    check(evidence.length === 1 && evidence[0].source.page === 2 && evidence[0].revision === 1, "Missing fitted evidence");
    check(answer.context.fitted.sources === 1 && answer.context.coverage.partialDocumentIds[0] === 1, "Missing durable coverage");
    check((await invoke("chat_resolve_evidence", { messageId: answer.id, citationId: "S1" })).includes("FERN-742"),
      "Evidence could not be inspected");
    check(await invoke("chat_resolve_evidence", { messageId: answer.id, citationId: "S2" }) === null, "Unsupplied evidence resolved");

    const uncitedChat = await invoke("create_conversation");
    const uncited = await invoke("send_message", {
      id: uncitedChat.id, content: "What is the orchard gate code? Code only.",
      options: { ...options, maxTokens: 5 }, events: channel(() => {}),
    });
    result.shortAnswer = uncited.content;
    result.shortCitationCount = (await invoke("chat_source_references", { messageId: uncited.id })).length;
    check((await refs(uncited.id)).length === 1, "Short answer lost its supplied passage");

    const maximum = await invoke("create_conversation");
    const checked = await invoke("send_message", {
      id: maximum.id, content: "What is a quasar?",
      options: { maxTokens: 4, knowledge: { mode: "keyword", documentIds: ids, grounding: "documents" } },
      events: channel(() => {}),
    });
    check(checked.context.coverage.retrieved === 0 && checked.context.fitted.sources === 0, "Zero results or maximum selection failed");
    check((await refs(checked.id)).length === 0, "Zero-result answer associated with evidence");

    const budget = await invoke("create_conversation");
    await reject(invoke("send_message", {
      id: budget.id, content: "orchard",
      options: { ...options, maxTokens: 1023 }, events: channel(() => {}),
    }), "context_limit");
    const failed = (await invoke("get_conversation", { id: budget.id })).messages.at(-1);
    check(failed.state === "failed" && failed.context.coverage.retrieved === 1 && failed.context.fitted === null,
      "Contextual fit failure did not preserve metadata");

    const busyChat = await invoke("create_conversation");
    let busyQuery;
    const cancelled = await invoke("send_message", {
      id: busyChat.id, content: "Explain exactly where the orchard ladder is.", options,
      events: channel((e) => {
        if (e.kind === "started") {
          busyQuery = reject(search("semantic", "orchard", [1]), "busy");
          void busyQuery.finally(() => invoke("stop_generation"));
        }
      }),
    });
    await busyQuery;
    check(cancelled.state === "cancelled", "Generation did not stop");

    const preparation = await invoke("create_conversation");
    const pending = invoke("send_message", {
      id: preparation.id,
      content: "Which passage describes the orchard gate access code for visitors arriving at the entrance?",
      options: { knowledge: { mode: "semantic", documentIds: ids } },
      events: channel(() => {}),
    });
    const stoppedDuringPreparation = reject(pending, "cancelled");
    for (let i = 0; i < 6; i++) {
      if (await invoke("stop_generation")) break;
      await wait(1);
    }
    await stoppedDuringPreparation;
    check((await invoke("get_conversation", { id: preparation.id })).messages.length === 0,
      "Stopping preparation saved a turn");

    synthetic("memory");
    const preferred = await invoke("memory_create", { content: "My favorite snack is saffron popcorn." });
    await invoke("memory_set_retrieval_enabled", { enabled: true });
    const memoryChat = await invoke("create_conversation");
    const recall = await invoke("send_message", {
      id: memoryChat.id, content: "What is my favorite snack?", options: { maxTokens: 32 }, events: channel(() => {}),
    });
    check(/saffron popcorn/i.test(recall.content) && recall.context.fitted.memories === 1, "Memory recall failed");
    await invoke("memory_edit", { id: preferred.id, content: "My favorite snack is sesame crackers." });
    const updated = await invoke("regenerate", { id: memoryChat.id, options: { maxTokens: 32 }, events: channel(() => {}) });
    check(/sesame crackers/i.test(updated.content) && !/saffron popcorn/i.test(updated.content), "Regeneration reused edited memory");
    await invoke("memory_delete", { id: preferred.id });
    const forgotten = await invoke("regenerate", { id: memoryChat.id, options: { maxTokens: 24 }, events: channel(() => {}) });
    check(!forgotten.context && !/saffron popcorn|sesame crackers/i.test(forgotten.content), "Deleted memories leaked into regeneration");
    await invoke("memory_set_retrieval_enabled", { enabled: false });

    for (let i = 0; i < 511; i++) await invoke("memory_create", { content: `Synthetic mémoire ${i}` });
    await invoke("memory_create", { content: "é".repeat(512) });
    await reject(invoke("memory_create", { content: "é".repeat(513) }), "invalid_config");
    await reject(invoke("memory_create", { content: "capacity overflow" }), "memory_limit");
    const page = await invoke("memory_page", { query: "MÉMOIRE", limit: 50 });
    const next = await invoke("memory_page", { query: "mémoire", beforeId: page.items.at(-1).id, limit: 50 });
    check(page.total === 512 && next.total === 512 && page.hasMore && next.items.length === 50
      && next.items[0].id < page.items.at(-1).id, "Memory count, search or pagination failed");
    check(await invoke("memory_clear") === 512, "Memory clear count incorrect");

    synthetic("unload");
    await invoke("knowledge_unload_embedding_model");
    await reject(search("semantic", "orchard", [1]), "embedding_unavailable");
    check((await library()).find((doc) => doc.id === 1).embeddingState === "ready", "Unloading changed saved indexes");
    check((await invoke("list_agent_scopes")).length === 0, "Evidence created an Agent grant");
    document.documentElement.dataset.task05Result = JSON.stringify({ ...result, pass: true });
  } catch (error) {
    document.documentElement.dataset.task05Result = JSON.stringify({
      ...result, pass: false, stage: document.documentElement.dataset.task05Stage,
      error: error?.message ?? JSON.stringify(error),
    });
  }
})();
