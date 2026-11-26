// Included only by the opt-in Rust desktop test. It uses the actual Tauri IPC.
void (async () => {
  const ipc = window.__TAURI_INTERNALS__;
  const events = [];
  const legacy = [];
  const subscribe = (event, received) => ipc.invoke("plugin:event|listen", {
    event,
    target: { kind: "Any" },
    handler: ipc.transformCallback(({ payload }) => received.push(payload)),
  });
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const terminal = async (taskId) => {
    for (let attempt = 0; attempt < 160; attempt += 1) {
      const match = events.find(({ event }) => event.taskId === taskId
        && (event.kind === "finished" || event.kind === "failed"));
      if (match) return match;
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    throw new Error("No scoped terminal event");
  };
  const chatChannel = (received) => `__CHANNEL__:${ipc.transformCallback((item) => {
    if ("message" in item) received.push(item.message);
  })}`;
  const delay = () => new Promise((resolve) => setTimeout(resolve, 25));
  async function checkContinuation() {
    const thread = await ipc.invoke("create_conversation");
    const recommendation = await ipc.invoke("recommended_chat_settings");
    assert(recommendation.trainedContext === 4096 && recommendation.settings.maxTokens === 2048, "Recommendation ignored trained context");
    assert((await ipc.invoke("bootstrap")).models[0].settings.maxTokens === 768, "Recommendation modified settings");
    const started = [];
    const initial = await ipc.invoke("send_message", {
      id: thread.id, content: "Fixture Chat continuation",
      options: { maxTokens: 1 }, events: chatChannel(started),
    });
    assert(initial.content === "First " && initial.state === "truncated", "Incorrect initial Chat segment");
    const args = { id: thread.id, messageId: initial.id };
    assert(await ipc.invoke("chat_continuation_remaining", args) === 3, "Incorrect initial continuation cap");
    let exhausted = false;
    try {
      await ipc.invoke("continue_chat", { ...args, maxTokens: 4, events: chatChannel([]) });
    } catch (error) {
      exhausted = error.code === "context_limit";
    }
    assert(exhausted, "Missing context-limit error");
    const restored = await ipc.invoke("get_conversation", { id: thread.id });
    assert(restored.messages[1].content === initial.content && restored.messages[1].state === "truncated",
      "Context failure did not restore the saved Chat prefix");
    const continuation = [];
    const second = await ipc.invoke("continue_chat", { ...args, maxTokens: 2, events: chatChannel(continuation) });
    assert(second.id === initial.id && second.content === "First second " && second.stats.generatedTokens === 2,
      "Continuation did not append to the same attempt");
    const progress = [];
    const pending = ipc.invoke("continue_chat", { ...args, maxTokens: 3, events: chatChannel(progress) });
    for (let wait = 0; !progress.some((event) => event.kind === "delta") && wait < 80; wait += 1) await delay();
    assert(progress.some((event) => event.kind === "delta"), "Continuation did not stream a fresh suffix");
    assert(await ipc.invoke("stop_generation") === true, "Stop did not cancel continuation");
    const stopped = await pending;
    assert(stopped.id === initial.id && stopped.state === "cancelled" && stopped.content === "First second stopped.",
      "Cancellation changed the attempt or discarded the prefix");
    const saved = await ipc.invoke("get_conversation", { id: thread.id });
    assert(saved.messages.length === 2 && saved.messages[1].content === stopped.content,
      "Unexpected Chat messages after three continuation requests");
    assert(continuation.some((event) => event.kind === "started" && event.user === null && event.assistant.content === initial.content),
      "Missing continuation start with existing prefix");
    assert(!JSON.stringify(second).includes("assistantReplay"), "Native replay data leaked over IPC");
    await ipc.invoke("delete_conversation", { id: thread.id });
  }
  const save = (result) => {
    document.documentElement.dataset.unifiedIpcResult = JSON.stringify(result);
  };
  try {
    await subscribe("agent-turn-event", events);
    await subscribe("agent-event", legacy);
    const conversation = await ipc.invoke("create_conversation");
    const start = (prompt) => ipc.invoke("start_agent_turn", { conversationId: conversation.id, prompt });
    const first = await start("Fixture calculator");
    const completed = await terminal(first.taskId);
    assert(typeof first.taskId === "string" && typeof first.turnId === "string", "Identifiers lost precision safety");
    assert(completed.conversationId === conversation.id && completed.turnId === first.turnId, "Wrong turn owner");
    assert(completed.event.nativeResults.length === 1 && completed.event.state === "completed", "Missing native observation");
    assert(events.some(({ event }) => event.kind === "observation" && event.taskId === first.taskId), "Missing live observation");
    const second = await start("Fixture calculator follow-up");
    const followUp = await terminal(second.taskId);
    assert(followUp.event.answer === "The previous result was 14." && followUp.event.nativeResults.length === 0, "Bad follow-up result");
    const third = await start("Fixture cancellation");
    let active = await ipc.invoke("get_active_agent_turn");
    for (let attempt = 0; active && active.startedAt === null && attempt < 40; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 25));
      active = await ipc.invoke("get_active_agent_turn");
    }
    assert(active.taskId === third.taskId && active.turnId === third.turnId && active.approval === null, "Wrong active execution");
    assert(active.taskTimeoutMs === 300000 && active.tool === null && active.startedAt > 0, "Missing active timing or tool");
    const search = (mode) => ipc.invoke("knowledge_retrieve", {
      mode, request: { query: "fixture", documentIds: null, limit: 1 },
    });
    let denseError;
    try {
      await search("semantic");
    } catch (error) {
      denseError = error;
    }
    assert(denseError?.code === "busy", "Semantic retrieval did not honor the Agent CPU lease");
    assert((await search("keyword")).hits.length === 0, "Agent blocked independent keyword retrieval");

    assert(await ipc.invoke("cancel_agent_task", { taskId: "9999999999" }) === false, "Wrong task cancelled");
    assert(await ipc.invoke("cancel_agent_task", { taskId: third.taskId }) === true, "Active cancellation rejected");
    assert((await terminal(third.taskId)).event.state === "cancelled", "Cancellation was not terminal");
    assert(legacy.some((event) => event.taskId === first.taskId && event.kind === "finished"), "Missing compatibility event");
    const saved = await ipc.invoke("get_unified_conversation", { id: conversation.id });
    assert(saved.turns.length === 3 && saved.turns.every(({ kind }) => kind === "agent"), "Unexpected persisted turns");
    assert(saved.turns[1].assistants[0].message.content === "The previous result was 14.", "Follow-up text did not persist");
    assert(saved.turns[2].assistants[0].message.state === "cancelled", "Cancelled state did not persist");
    assert(saved.turns[0].nativeResultsAvailable && saved.turns[0].taskTimeoutMs === 300000, "Missing session result or budget");
    assert(saved.turns[0].taskState === "completed" && saved.turns[0].taskError === null
      && saved.turns[0].excludedTurns === 0, "Missing inline completed task status or context count");
    assert(saved.turns[2].taskState === "cancelled" && saved.turns[2].taskError === "cancelled"
      && typeof saved.turns[2].taskErrorMessage === "string", "Missing inline saved cancellation");
    const journal = await ipc.invoke("list_agent_tasks", { before: null, limit: 30 });
    const linked = journal.find((task) => task.id === first.taskId);
    assert(linked?.conversationId === conversation.id && linked.turnId === first.turnId
      && linked.excludedTurns === 0, "Journal summary did not carry the conversation link");
    await ipc.invoke("delete_conversation", { id: conversation.id });
    const remaining = await ipc.invoke("list_conversations", { query: "" });
    assert(remaining.length === 0, "Deleted conversation still listed");
    await checkContinuation();
    save({ pass: true, turns: saved.turns.length, events: events.length, legacyEvents: legacy.length, chatContinuation: true });
  } catch (error) {
    save({ pass: false, error: String(error), events: events.length, legacyEvents: legacy.length });
  }
})();
