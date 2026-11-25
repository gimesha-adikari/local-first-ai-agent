import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { mergeNativeResults, type LiveTask } from "./agent";
import type { AgentScopeDisplay, AgentTaskDetail, AgentTaskSummary, IpcError } from "./agentTypes";
import { errorMessage } from "./inference";
import {
  applyTurnEvent, createLiveTurn, isTerminalEvent, ownsEvent,
  type ActiveAgentTurn, type AgentTurnEvent, type AgentTurnIdentity, type LiveTurn,
} from "./unified";

function failureOf(value: unknown): IpcError {
  if (value && typeof value === "object" && "code" in value && "message" in value
    && typeof value.code === "string" && typeof value.message === "string") {
    return { code: value.code, message: value.message };
  }
  return { code: "unknown", message: errorMessage(value) };
}

/**
 * The single Agent controller for unified conversations. It listens only to
 * `agent-turn-event` (never the compatibility `agent-event`), owns at most one
 * active linked turn, and never starts standalone journal tasks.
 */
export function useAgent({ onTurnSettled }: { onTurnSettled: (identity: AgentTurnIdentity) => void }) {
  const [turns, setTurns] = useState<Record<string, LiveTurn>>({});
  const [details, setDetails] = useState<Record<string, AgentTaskDetail>>({});
  const [tasks, setTasks] = useState<AgentTaskSummary[]>([]);
  const [active, setActive] = useState<AgentTurnIdentity | null>(null);
  const [starting, setStarting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [approvalBusy, setApprovalBusy] = useState(false);
  const [eventReady, setEventReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => performance.now());
  const [scopes, setScopes] = useState<AgentScopeDisplay[]>([]);
  const [scopesAt, setScopesAt] = useState(() => performance.now());
  const [scopeAction, setScopeAction] = useState<string | null>(null);
  const [scopeError, setScopeError] = useState<string | null>(null);

  const activeRef = useRef<AgentTurnIdentity | null>(null);
  const reconcilingRef = useRef<AgentTurnIdentity | null>(null);
  const startingRef = useRef(false);
  const queued = useRef<AgentTurnEvent[]>([]);
  const listenerAvailable = useRef(false);
  const listenerReady = useRef<Promise<UnlistenFn>>(Promise.resolve(() => {}));
  const receiveRef = useRef<(payload: AgentTurnEvent) => void>(() => {});
  const settledRef = useRef(onTurnSettled);
  const eventKey = useRef(0);
  settledRef.current = onTurnSettled;

  const setOwner = (identity: AgentTurnIdentity | null) => {
    activeRef.current = identity;
    setActive(identity);
  };

  /** Read-only journal list. Used for legacy standalone records; never creates tasks. */
  const refreshTasks = useCallback(async () => {
    try {
      setTasks(await invoke<AgentTaskSummary[]>("list_agent_tasks", { before: null, limit: 50 }));
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }, []);

  const refreshScopes = useCallback(async () => {
    try {
      const next = await invoke<AgentScopeDisplay[]>("list_agent_scopes");
      setScopes(next);
      setScopesAt(performance.now());
      setScopeError(null);
    } catch (failure) {
      setScopeError(errorMessage(failure));
    }
  }, []);

  /** Journal status, executions and any session results still held for a task. */
  const loadDetail = useCallback(async (taskId: string) => {
    try {
      const detail = await invoke<AgentTaskDetail>("get_agent_task", { taskId });
      setDetails((current) => ({ ...current, [taskId]: detail }));
      if (detail.nativeResults?.length) {
        setTurns((current) => {
          const entry = Object.values(current).find((turn) => turn.id === taskId);
          return entry
            ? { ...current, [entry.turnId]: { ...entry, nativeResults: mergeNativeResults(entry.nativeResults, detail.nativeResults) } }
            : current;
        });
      }
      return detail;
    } catch (failure) {
      setError(errorMessage(failure));
      return null;
    }
  }, []);

  function receive(payload: AgentTurnEvent) {
    const entry = { key: ++eventKey.current, at: Date.now(), perf: performance.now() };
    setTurns((current) => {
      const turn = current[payload.turnId];
      return turn ? { ...current, [payload.turnId]: applyTurnEvent(turn, payload, entry) } : current;
    });
    if (!isTerminalEvent(payload.event)) return;
    const identity = { conversationId: payload.conversationId, turnId: payload.turnId, taskId: payload.event.taskId };
    if (ownsEvent(activeRef.current, payload)) {
      setOwner(null);
      setCancelling(false);
    }
    if (ownsEvent(reconcilingRef.current, payload)) reconcilingRef.current = null;
    settledRef.current(identity);
    void loadDetail(identity.taskId);
    void refreshScopes();
  }

  receiveRef.current = (payload) => {
    if (!payload?.event || typeof payload.event.taskId !== "string") return;
    if (ownsEvent(activeRef.current, payload)) {
      receive(payload);
      return;
    }
    // The host releases ownership before its terminal event; accept that event once.
    if (ownsEvent(reconcilingRef.current, payload) && isTerminalEvent(payload.event)) {
      receive(payload);
      return;
    }
    if (startingRef.current && activeRef.current === null) queued.current.push(payload);
  };

  async function recoverActive() {
    try {
      const found = await invoke<ActiveAgentTurn | null>("get_active_agent_turn");
      if (!found || activeRef.current) return;
      const identity = { conversationId: found.conversationId, turnId: found.turnId, taskId: found.taskId };
      const perf = performance.now();
      const now = Date.now();
      setOwner(identity);
      setTurns((current) => {
        const submittedAt = found.startedAt ?? now;
        const startedPerf = perf - Math.max(0, now - submittedAt);
        const turn = current[identity.turnId] ?? createLiveTurn(identity, "", found.taskTimeoutMs, submittedAt, startedPerf);
        const approval = found.approval
          ? {
            kind: "approval_required" as const, taskId: identity.taskId, operationId: found.approval.operationId,
            tool: found.approval.tool, expiresInMs: found.approval.expiresInMs, target: found.approval.target,
            expiresAt: perf + found.approval.expiresInMs,
          }
          : turn.approval;
        const targets = found.approval?.target
          ? { ...turn.targets, [found.approval.operationId]: found.approval.target } : turn.targets;
        return { ...current, [identity.turnId]: {
          ...turn, approval, targets, startedPerf: turn.startedPerf ?? startedPerf,
        } };
      });
      const detail = await loadDetail(identity.taskId);
      if (detail && found.startedAt === null) {
        // The snapshot can arrive before the worker records Started.
        const startedAgo = Math.max(0, Date.now() - detail.task.createdAt);
        setTurns((current) => current[identity.turnId]
          ? { ...current, [identity.turnId]: { ...current[identity.turnId], submittedAt: detail.task.createdAt, startedPerf: performance.now() - startedAgo } }
          : current);
      }
      // A null or finished task can race with the terminal event; reconcile with the journal.
      if (detail?.task.finalized && sameOwner(identity)) {
        setOwner(null);
        settledRef.current(identity);
      }
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }

  const sameOwner = (identity: AgentTurnIdentity) => {
    const owner = activeRef.current;
    return !!owner && owner.taskId === identity.taskId && owner.turnId === identity.turnId;
  };

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    listenerReady.current = listen<AgentTurnEvent>("agent-turn-event", ({ payload }) => receiveRef.current(payload))
      .then((stop) => {
        listenerAvailable.current = true;
        setEventReady(true);
        unlisten = stop;
        if (disposed) stop();
        else void recoverActive();
        return stop;
      })
      .catch((failure: unknown) => {
        listenerAvailable.current = false;
        setEventReady(false);
        setError(errorMessage(failure));
        return () => {};
      });
    void refreshTasks();
    void refreshScopes();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refreshTasks, refreshScopes]);

  const running = active !== null;

  // A fast clock only while a turn runs (elapsed time and approval countdowns).
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => setNow(performance.now()), 250);
    return () => window.clearInterval(timer);
  }, [running]);

  // A slow clock and an expiry refresh for active grants.
  const activeScopes = scopes.filter((scope) => scope.state === "active");
  const nextExpiry = activeScopes.length ? Math.min(...activeScopes.map((scope) => scope.remainingMs)) : null;
  useEffect(() => {
    if (nextExpiry === null) return;
    const elapsed = performance.now() - scopesAt;
    const expiry = window.setTimeout(() => void refreshScopes(), Math.max(250, nextExpiry - elapsed + 250));
    const tick = window.setInterval(() => setNow(performance.now()), 15_000);
    return () => {
      window.clearTimeout(expiry);
      window.clearInterval(tick);
    };
  }, [nextExpiry, scopesAt, refreshScopes]);

  /** Start a linked Agent turn inside an existing conversation. */
  async function start(conversationId: number, prompt: string, timeoutSeconds: number): Promise<AgentTurnIdentity | null> {
    const text = prompt.trim();
    if (!text || startingRef.current || activeRef.current) return null;
    setError(null);
    setStarting(true);
    startingRef.current = true;
    queued.current = [];
    try {
      await listenerReady.current;
      if (!listenerAvailable.current) throw new Error("Agent event delivery is unavailable. Restart the app and try again.");
      const identity = await invoke<AgentTurnIdentity>("start_agent_turn", {
        conversationId, prompt: text, taskTimeoutSeconds: timeoutSeconds,
      });
      setOwner(identity);
      setTurns((current) => ({
        ...current,
        [identity.turnId]: createLiveTurn(identity, text, timeoutSeconds * 1000, Date.now(), performance.now()),
      }));
      setNow(performance.now());
      const pending = queued.current.splice(0);
      for (const payload of pending) if (ownsEvent(identity, payload)) receive(payload);
      return identity;
    } catch (failure) {
      setError(failureOf(failure).message);
      queued.current = [];
      return null;
    } finally {
      startingRef.current = false;
      setStarting(false);
    }
  }

  /** Cancel the exact active turn, wherever the user is in the app. */
  async function cancel() {
    const owner = activeRef.current;
    if (!owner || cancelling) return;
    setCancelling(true);
    try {
      const accepted = await invoke<boolean>("cancel_agent_task", { taskId: owner.taskId });
      if (!accepted && sameOwner(owner)) {
        reconcilingRef.current = owner;
        setOwner(null);
        setCancelling(false);
        settledRef.current(owner);
        await loadDetail(owner.taskId);
      }
    } catch (failure) {
      setError(errorMessage(failure));
      setCancelling(false);
    }
  }

  /** Answer the live approval of the active turn with its exact operation ID. */
  async function respond(approve: boolean) {
    const owner = activeRef.current;
    const request = owner ? turns[owner.turnId]?.approval : null;
    if (!owner || !request || approvalBusy || cancelling) return;
    setApprovalBusy(true);
    setError(null);
    try {
      await invoke("respond_agent_approval", { taskId: owner.taskId, operationId: request.operationId, approve });
      setTurns((current) => current[owner.turnId]?.approval?.operationId === request.operationId
        ? { ...current, [owner.turnId]: { ...current[owner.turnId], approval: null } }
        : current);
    } catch (failure) {
      setError(failureOf(failure).message);
    } finally {
      setApprovalBusy(false);
    }
  }

  /** Drop session state for a deleted conversation. */
  function forgetConversation(conversationId: number) {
    setTurns((current) => Object.fromEntries(Object.entries(current).filter(([, turn]) => turn.conversationId !== conversationId)));
  }

  async function grant(kind: "file" | "directory") {
    if (scopeAction) return;
    setScopeAction(kind);
    setScopeError(null);
    try {
      await invoke<AgentScopeDisplay | null>(kind === "file" ? "choose_agent_file" : "choose_agent_directory");
      await refreshScopes();
    } catch (failure) {
      setScopeError(errorMessage(failure));
    } finally {
      setScopeAction(null);
    }
  }

  async function revoke(scope: AgentScopeDisplay) {
    if (scope.state !== "active" || scopeAction) return false;
    setScopeAction(scope.id);
    setScopeError(null);
    try {
      await invoke<boolean>("revoke_agent_scope", { scopeId: scope.id });
      await refreshScopes();
      return true;
    } catch (failure) {
      setScopeError(errorMessage(failure));
      return false;
    } finally {
      setScopeAction(null);
    }
  }

  /** Remaining grant time, extrapolated from the last list response. */
  const scopeRemaining = (scope: AgentScopeDisplay) =>
    scope.state === "active" ? Math.max(0, scope.remainingMs - (now - scopesAt)) : 0;

  const activeApproval = active ? turns[active.turnId]?.approval ?? null : null;

  return {
    turns, details, tasks, active, activeApproval, running, starting, cancelling, approvalBusy, eventReady, error, now,
    scopes, scopeAction, scopeError, scopeRemaining,
    setError, refreshTasks, refreshScopes, loadDetail, start, cancel, respond, grant, revoke, forgetConversation,
  };
}

export type AgentController = ReturnType<typeof useAgent>;
export type { LiveTask, LiveTurn };
