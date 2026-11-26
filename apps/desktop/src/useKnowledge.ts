import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { commandError } from "./chat";
import {
  createLatest, importOutcomeNotice, importTerminal, indexTerminal,
  knowledgeErrorCopy,
  type DocumentIndexState, type EmbeddingStatus, type ImportStatus, type IndexProgress, type LibraryDocument,
  type KnowledgeDocument,
} from "./knowledge";

export type KnowledgeNotice = { tone: "accent" | "error" | "neutral"; title: string; detail?: string };

const pollMs = 250;

function pollJob<T>(read: () => Promise<T>, receive: (value: T) => boolean) {
  let active = true;
  let timer: number | undefined;
  async function poll() {
    try {
      const value = await read();
      if (!active || receive(value)) return;
    } catch {
      if (!active) return;
    }
    timer = window.setTimeout(poll, pollMs);
  }
  void poll();
  return () => { active = false; window.clearTimeout(timer); };
}

/**
 * Library state shared by the Knowledge view and the composer. Lives in App so
 * import and index jobs keep polling while the person is elsewhere.
 */
export function useKnowledge({ enabled }: { enabled: boolean }) {
  const [documents, setDocuments] = useState<LibraryDocument[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const [embedding, setEmbedding] = useState<EmbeddingStatus | null>(null);
  const indexStates = useMemo<Record<number, DocumentIndexState>>(() => Object.fromEntries(
    documents.map(({ id, embeddingState }) => [id, embeddingState === "ready" ? "indexed" : embeddingState]),
  ), [documents]);
  const [importJob, setImportJob] = useState<ImportStatus | null>(null);
  const [indexJob, setIndexJob] = useState<IndexProgress | null>(null);
  const [modelAction, setModelAction] = useState<"choosing" | "unloading" | null>(null);
  const [notice, setNotice] = useState<KnowledgeNotice | null>(null);
  /** Increments whenever documents are added, replaced, deleted or reindexed. */
  const [revision, setRevision] = useState(0);
  const listGate = useRef(createLatest()).current;
  const [sourceRevision, setSourceRevision] = useState(0);
  const documentsRef = useRef<KnowledgeDocument[]>([]);
  documentsRef.current = documents;

  const fail = useCallback((failure: unknown, fallback?: string) => {
    const copy = knowledgeErrorCopy(commandError(failure));
    setNotice({ tone: "error", title: fallback ?? copy.title, detail: copy.detail });
  }, []);

  const refresh = useCallback(async () => {
    const token = listGate.next();
    try {
      const all = await invoke<LibraryDocument[]>("knowledge_library");
      if (!listGate.isCurrent(token)) return;
      setDocuments(all);
      setLoaded(true);
      setListError(null);
    } catch (failure) {
      if (listGate.isCurrent(token)) setListError(knowledgeErrorCopy(commandError(failure)).detail);
    }
  }, [listGate]);

  const refreshEmbedding = useCallback(async () => {
    try {
      setEmbedding(await invoke<EmbeddingStatus>("knowledge_embedding_status"));
    } catch {
      setEmbedding(null);
    }
  }, []);

  // The host restores the remembered model at launch; poll until it settles.
  useEffect(() => {
    if (!enabled || embedding?.restore !== "pending") return;
    const timer = window.setTimeout(() => void refreshEmbedding(), 500);
    return () => window.clearTimeout(timer);
  }, [enabled, embedding, refreshEmbedding]);

  // Initial load, and pick up jobs that are still running in this process.
  useEffect(() => {
    if (!enabled) return;
    void refresh();
    void refreshEmbedding();
    void invoke<ImportStatus | null>("knowledge_import_status").then((status) => {
      if (status && !importTerminal(status.state)) setImportJob((current) => !current || status.jobId > current.jobId ? status : current);
    }).catch(() => {});
    void invoke<IndexProgress | null>("knowledge_index_progress").then((progress) => {
      if (progress && !indexTerminal(progress.phase)) setIndexJob((current) => !current || progress.jobId > current.jobId ? progress : current);
    }).catch(() => {});
  }, [enabled, refresh, refreshEmbedding]);

  const changed = useCallback((textChanged: boolean) => {
    if (textChanged) setSourceRevision((value) => value + 1);
    setRevision((value) => value + 1);
    void refresh();
  }, [refresh]);

  // Poll the import job until it settles.
  const importId = importJob && !importTerminal(importJob.state) ? importJob.jobId : null;
  useEffect(() => {
    if (importId === null) return;
    return pollJob(
      () => invoke<ImportStatus | null>("knowledge_import_status", { jobId: importId }),
      (status) => {
        if (!status) {
          setImportJob((current) => current?.jobId === importId ? null : current);
          return true;
        }
        setImportJob((current) => current?.jobId === importId && current.state.phase !== status.state.phase ? status : current);
        const state = status.state;
        if (!importTerminal(state)) return false;
        if (state.phase === "completed") {
          setNotice({ tone: state.outcome.kind === "duplicate" ? "neutral" : "accent", title: importOutcomeNotice(state.outcome, null) });
          if (state.outcome.kind !== "duplicate") changed(true);
        } else if (state.phase === "failed") {
          fail(state.error);
          if (state.error.code === "knowledge_not_found") changed(true);
        } else {
          setNotice({ tone: "neutral", title: "Import cancelled. Nothing was added." });
        }
        return true;
      },
    );
  }, [importId, changed, fail]);

  const indexId = indexJob && !indexTerminal(indexJob.phase) ? indexJob.jobId : null;
  useEffect(() => {
    if (indexId === null) return;
    return pollJob(
      () => invoke<IndexProgress | null>("knowledge_index_progress", { jobId: indexId }),
      (progress) => {
        if (!progress) {
          setIndexJob((current) => current?.jobId === indexId ? null : current);
          return true;
        }
        setIndexJob((current) => current?.jobId === indexId &&
          (current.phase !== progress.phase || current.processedChunks !== progress.processedChunks || current.totalChunks !== progress.totalChunks)
          ? progress : current);
        if (!indexTerminal(progress.phase)) return false;
        const name = documentsRef.current.find((document) => document.id === progress.documentId)?.name;
        const label = name ? `“${name}”` : "the document";
        if (progress.phase === "completed") {
          setNotice({ tone: "accent", title: `Semantic index ready for ${label}.` });
          changed(false);
        } else if (progress.phase === "cancelled") {
          setNotice({ tone: "neutral", title: `Indexing cancelled. Any earlier index of ${label} is unchanged.` });
        } else if (progress.error) {
          fail(progress.error);
          if (["knowledge_not_found", "knowledge_revision_changed"].includes(progress.error.code)) changed(true);
        }
        return true;
      },
    );
  }, [indexId, changed, fail]);

  async function startImport(documentId?: number) {
    setNotice(null);
    try {
      const jobId = await invoke<number>(documentId === undefined ? "knowledge_start_import" : "knowledge_reimport",
        documentId === undefined ? undefined : { documentId });
      setImportJob({ jobId, state: { phase: "selecting" } });
    } catch (failure) {
      fail(failure);
      if (commandError(failure).code === "knowledge_not_found") void refresh();
    }
  }

  async function cancelImport() {
    if (!importJob) return;
    try {
      await invoke<boolean>("knowledge_cancel_import", { jobId: importJob.jobId });
    } catch (failure) {
      fail(failure);
    }
  }

  async function deleteDocument(documentId: number) {
    try {
      await invoke("knowledge_delete_document", { documentId });
      const name = documentsRef.current.find((document) => document.id === documentId)?.name;
      setNotice({ tone: "neutral", title: `Deleted ${name ? `“${name}”` : "the document"} from your library.` });
      changed(true);
      return true;
    } catch (failure) {
      fail(failure);
      if (commandError(failure).code === "knowledge_not_found") changed(true);
      return false;
    }
  }

  async function reindex(documentId: number) {
    setNotice(null);
    try {
      const jobId = await invoke<number>("knowledge_reindex", { documentId });
      setIndexJob({ jobId, documentId, phase: "indexing", processedChunks: 0, totalChunks: 0, windowCount: null, error: null });
    } catch (failure) {
      fail(failure);
      if (commandError(failure).code === "embedding_unavailable") void refreshEmbedding();
    }
  }

  async function cancelIndex() {
    if (!indexJob) return;
    try {
      await invoke<boolean>("knowledge_cancel_index", { jobId: indexJob.jobId });
    } catch (failure) {
      fail(failure);
    }
  }

  async function chooseModel() {
    setModelAction("choosing");
    setNotice(null);
    try {
      const status = await invoke<EmbeddingStatus | null>("knowledge_choose_embedding_model");
      if (status) {
        setEmbedding(status);
        setNotice({ tone: "accent", title: "The semantic search model is ready.", detail: "The app remembers it and verifies it again at each launch." });
      }
    } catch (failure) {
      if (commandError(failure).code === "embedding_incompatible") {
        setNotice({ tone: "error", title: "The model file couldn't be used",
          detail: "Choose the supported local BGE-small-en-v1.5 F16 GGUF, or use Keyword search." });
      } else {
        fail(failure, "The model file couldn't be used");
      }
      void refreshEmbedding();
    } finally {
      setModelAction(null);
    }
  }

  async function unloadModel() {
    setModelAction("unloading");
    setNotice(null);
    try {
      await invoke("knowledge_unload_embedding_model");
      setNotice({ tone: "neutral", title: "Semantic search model unloaded. Saved indexes are kept." });
    } catch (failure) {
      fail(failure);
    } finally {
      setModelAction(null);
      void refreshEmbedding();
    }
  }

  const importing = !!importJob && !importTerminal(importJob.state);
  const indexing = !!indexJob && !indexTerminal(indexJob.phase);

  return {
    documents, loaded, listError, embedding, indexStates, importJob, indexJob, modelAction, notice, revision, sourceRevision,
    importing, indexing,
    /** Indexing and model loading hold the shared CPU lease used by Chat and Agent. */
    holdsLease: indexing || modelAction !== null,
    setNotice, refresh, refreshEmbedding, startImport, cancelImport, deleteDocument, reindex, cancelIndex,
    chooseModel, unloadModel,
  };
}

export type KnowledgeController = ReturnType<typeof useKnowledge>;
