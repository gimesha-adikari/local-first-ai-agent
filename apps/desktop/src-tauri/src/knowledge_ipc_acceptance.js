// Opt-in desktop fixture. Use the native chooser and confirmation dialogs.
void (async () => {
  const invoke = (name, args) => window.__TAURI_INTERNALS__.invoke(name, args);
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const list = () => invoke("knowledge_list_documents", { limit: 10 });
  const search = (query, documentIds) => invoke("knowledge_search", {
    request: { query, documentIds, limit: 10 },
  });
  const status = (jobId) => invoke("knowledge_import_status", { jobId });
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  async function terminal(jobId) {
    for (let attempt = 0; attempt < 800; attempt += 1) {
      const result = await status(jobId);
      if (["completed", "failed", "cancelled"].includes(result?.state.phase)) {
        return result.state;
      }
      await wait(50);
    }
    throw new Error("Import did not finish");
  }
  const start = (args) => invoke("knowledge_start_import", args);
  try {
    assert((await list()).length === 0, "Fixture profile should start empty");
    const attempt = await start({ path: "/synthetic/unapproved.txt" });
    assert((await status(attempt)).state.phase === "selecting", "Expected native picker for IPC path attempt");
    assert((await list()).length === 0, "IPC opened an unapproved path");
    assert((await terminal(attempt)).phase === "cancelled", "Picker cancellation was lost");

    // Native driver selects manual.md and declines retention.
    assert((await terminal(await start())).phase === "cancelled", "Consent refusal was lost");
    assert((await list()).length === 0, "Declined text was retained");

    // Select full.txt and approve, then repeat the same selection.
    const imported = await terminal(await start());
    assert(imported.outcome?.kind === "imported", JSON.stringify(imported));
    const id = imported.outcome.documentId;
    const matches = await search("endmarker", [id]);
    assert(matches.length === 1 && matches[0].source.endLine === 2, "Missing text or line reference past the Agent preview");
    const duplicate = await terminal(await start());
    assert(duplicate.outcome?.kind === "duplicate" && duplicate.outcome.documentId === id, "Duplicate was retained");

    // Replace the existing record with manual.md, then import orchard.pdf.
    const replaced = await terminal(await invoke("knowledge_reimport", { documentId: id }));
    assert(replaced.outcome?.kind === "replaced" && replaced.outcome.documentId === id, JSON.stringify(replaced));
    const details = await invoke("knowledge_get_document", { documentId: id });
    assert(details.name === "manual.md" && details.indexingStatus === "ready", "Replaced metadata is wrong");
    assert((await search("endmarker")).length === 0, "Old index still matches");
    assert((await search("MarkdownLedger", [id])).length === 1, "Markdown index is missing");
    const pdf = await terminal(await start());
    assert(pdf.outcome?.kind === "imported", JSON.stringify(pdf));
    const pdfId = pdf.outcome.documentId;
    const pages = await search("Monday", [pdfId]);
    assert(pages.length === 1 && pages[0].source.page === 2, "Wrong PDF page");
    assert((await search("Monday", [id])).length === 0, "Document filter leaked results");

    await invoke("knowledge_delete_document", { documentId: pdfId });
    assert((await search("Monday")).length === 0, "Deleted index still matches");
    assert((await list()).length === 1, "Deleted PDF is still listed");
    assert((await invoke("list_agent_scopes")).length === 0, "Import created an Agent grant");
    assert(await invoke("knowledge_cancel_import", { jobId: attempt }) === false, "An obsolete job could cancel work");
    try {
      await search('" ***');
      throw new Error("Malformed search was accepted");
    } catch (error) {
      assert(error?.code === "invalid_arguments", "Wrong malformed-search error");
    }
    document.documentElement.dataset.knowledgeIpcResult = JSON.stringify({ pass: true });
  } catch (error) {
    document.documentElement.dataset.knowledgeIpcResult =
      JSON.stringify({ pass: false, error: error?.message ?? String(error) });
  }
})();
