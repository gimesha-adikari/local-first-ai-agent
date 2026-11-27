// Opt-in in a fresh synthetic profile, with one real native model selection.
void (async () => {
  const invoke = (name, args) => window.__TAURI_INTERNALS__.invoke(name, args);
  const define = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const reject = async (work, code) => {
    try {
      await work();
    } catch (error) {
      define(error?.code === code, `Expected ${code}, got ${JSON.stringify(error)}`);
      return;
    }
    throw new Error(`Expected ${code}`);
  };
  const status = () => invoke("knowledge_embedding_status");
  const query = (mode, query, documentIds) =>
    invoke("knowledge_retrieve", { mode, request: { query, documentIds, limit: 2 } });
  const index = (id) => invoke("knowledge_reindex", { documentId: id });
  async function terminal(jobId) {
    for (let attempt = 0; attempt < 250; attempt += 1) {
      const result = await invoke("knowledge_index_progress", { jobId });
      if (["completed", "cancelled", "failed"].includes(result?.phase)) return result;
      await new Promise((resolve) => setTimeout(resolve, 30));
    }
    throw new Error("Index job did not finish");
  }
  try {
    const initial = await status();
    define(initial.supported && !initial.available && initial.dimensions === 384, "Unexpected model status");
    define((await query("keyword", "regional trains")).hits[0]?.documentName === "Rail", "Keyword search requires no model");
    await reject(() => query("semantic", "regional trains"), "embedding_unavailable");

    const selected = await invoke("knowledge_choose_embedding_model");
    define(selected?.available && selected.sha256 === initial.sha256, "Verified model did not load");
    await reject(() => query("hybrid", "regional trains"), "embedding_index_unavailable");
    const longId = await index(3);
    define(await invoke("knowledge_cancel_index", { jobId: longId }), "Index cancellation was not accepted");
    define((await terminal(longId)).phase === "cancelled", "Index cancellation did not finish");
    for (const doc of [1, 2]) {
      const run = await terminal(await index(doc));
      define(run.phase === "completed" && run.processedChunks === 1 && run.windowCount === 1,
        JSON.stringify(run));
    }

    const coverage = await invoke("knowledge_embedding_coverage");
    define(coverage.indexedDocuments === 2 && coverage.missingDocuments === 1, "Wrong partial coverage");
    // This paraphrase and passage are covered by the BGE relevance calibration.
    const dense = await query("semantic", "How often do commuter trains leave?", [1, 2]);
    define(dense.mode === "semantic" && dense.hits[0]?.documentName === "Rail"
      && dense.hits[0].source.startLine === 1, "Wrong semantic rank or reference");
    const hybrid = await query("hybrid", "regional trains");
    define(hybrid.mode === "hybrid" && hybrid.hits[0]?.documentName === "Rail"
      && hybrid.hits[0].source.startLine === 1, "Wrong hybrid rank or reference");
    for (const mode of ["semantic", "hybrid"]) {
      const irrelevant = await query(mode, "Explain quantum entanglement simply", [1, 2]);
      define(irrelevant.hits.length === 0, `Unrelated ${mode} query supplied evidence`);
    }
    await invoke("knowledge_delete_document", { documentId: 1 });
    const filtered = await query("semantic", "rail service", [1]);
    define(filtered.hits.length === 0, "Deleted document still retrieved");
    await invoke("knowledge_unload_embedding_model");
    define(!(await status()).available, "Embedding model stayed available after unload");
    define((await query("keyword", "lemon")).hits.length === 1, "Keyword search failed after unload");
    document.documentElement.dataset.semanticIpcResult = JSON.stringify({ pass: true });
  } catch (error) {
    document.documentElement.dataset.semanticIpcResult =
      JSON.stringify({ pass: false, error: error?.message ?? String(error) });
  }
})();
