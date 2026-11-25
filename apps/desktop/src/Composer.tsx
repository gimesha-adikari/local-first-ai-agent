import { useId, useLayoutEffect, type KeyboardEvent, type RefObject } from "react";
import { maxTaskTimeoutSeconds, scopeSummary, validTimeout } from "./agent";
import type { AgentScopeDisplay } from "./agentTypes";
import { lowOutputLimit, shortOutputLimit, type ModelReference } from "./chat";
import {
  AgentIcon, ArrowUpIcon, ChatIcon, ChevronDownIcon, CloseIcon, CubeIcon, FileIcon, FolderIcon, LockIcon,
  PlusIcon, SlidersIcon, Spinner, StopIcon, ThinkIcon,
} from "./icons";
import { modelLabel } from "./models";
import { Menu, MenuItem, MenuSeparator, Popover } from "./ui";

export type ComposerMode = "chat" | "agent";

export { modelLabel };

export default function Composer({
  inputRef, mode, onModeChange, agentAvailable, agentUnavailable,
  draft, onDraftChange, onSubmit, onStop, canSend, inputDisabled, showStop, stopLabel, canStop, stopping, enterToSend,
  models, activeModelId, modelBusy, modelLocked, onSelectModel, onManageModels,
  thinkingSupported, thinking, onThinkingChange,
  savedMaxTokens, outputOverride, onOutputOverride, contextLength, onOpenModelSettings,
  timeout, onTimeoutChange,
  scopes, scopeRemaining, scopeAction, scopeError, onGrant, onRevoke, onManageAccess, grantDisabled,
  lockMenus = false, placeholder, status,
}: {
  /** While an approval is pending, close and disable menus so nothing covers its controls. */
  lockMenus?: boolean;
  inputRef: RefObject<HTMLTextAreaElement | null>;
  mode: ComposerMode;
  onModeChange: (mode: ComposerMode) => void;
  agentAvailable: boolean;
  agentUnavailable: string | null;
  draft: string;
  onDraftChange: (value: string) => void;
  onSubmit: () => void;
  onStop: () => void;
  canSend: boolean;
  inputDisabled: boolean;
  showStop: boolean;
  stopLabel: string;
  canStop: boolean;
  stopping: boolean;
  enterToSend: boolean;
  models: ModelReference[];
  activeModelId: number | null;
  modelBusy: string | null;
  modelLocked: boolean;
  onSelectModel: (id: number) => void;
  onManageModels: () => void;
  thinkingSupported: boolean;
  thinking: boolean;
  onThinkingChange: (value: boolean) => void;
  savedMaxTokens: number | null;
  outputOverride: string;
  onOutputOverride: (value: string) => void;
  contextLength: number | null;
  onOpenModelSettings: () => void;
  timeout: string;
  onTimeoutChange: (value: string) => void;
  scopes: AgentScopeDisplay[];
  scopeRemaining: (scope: AgentScopeDisplay) => number;
  scopeAction: string | null;
  scopeError: string | null;
  onGrant: (kind: "file" | "directory") => void;
  onRevoke: (scope: AgentScopeDisplay) => void;
  onManageAccess: () => void;
  grantDisabled: boolean;
  placeholder: string;
  status: string;
}) {
  const outputId = useId();
  const timeoutId = useId();
  const statusId = useId();
  const textarea = inputRef;
  const active = models.find((model) => model.id === activeModelId) ?? null;
  const activeScopes = scopes.filter((scope) => scope.state === "active");
  const overrideValue = Number(outputOverride);
  const overrideInvalid = outputOverride.trim() !== "" && (!Number.isInteger(overrideValue) || overrideValue < 1
    || overrideValue > 4096 || (contextLength !== null && overrideValue >= contextLength));
  const timeoutInvalid = !validTimeout(Number(timeout));
  const optionsAttention = mode === "chat" ? overrideInvalid || outputOverride.trim() !== "" : timeoutInvalid;

  useLayoutEffect(() => {
    const node = textarea.current;
    if (!node) return;
    node.style.height = "auto";
    const next = Math.min(node.scrollHeight, 240);
    node.style.height = `${next}px`;
  }, [draft, textarea]);

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (!enterToSend || event.nativeEvent.isComposing || event.key !== "Enter"
      || event.shiftKey || event.ctrlKey || event.altKey || event.metaKey) return;
    event.preventDefault();
    if (canSend) onSubmit();
  }

  return (
    <div className="composer-wrap">
      <form className={`composer${inputDisabled ? " composer-disabled" : ""}`}
        onSubmit={(event) => { event.preventDefault(); if (canSend) onSubmit(); }}>
        {mode === "agent" && activeScopes.length > 0 && (
          <ul className="chips" aria-label="Selected files and folders">
            {activeScopes.map((scope) => (
              <li key={scope.id} className="chip" title={scopeSummary(scope, scopeRemaining(scope))}>
                {scope.kind === "directory" ? <FolderIcon size={14} /> : <FileIcon size={14} />}
                <span className="chip-name">{scope.displayName}</span>
                <span className="chip-meta"><LockIcon size={11} />Read-only</span>
                <button type="button" className="chip-remove" disabled={scopeAction !== null}
                  aria-label={`Revoke access to ${scope.displayName}`} title="Revoke access"
                  onClick={() => onRevoke(scope)}>
                  {scopeAction === scope.id ? <Spinner size={11} /> : <CloseIcon size={12} />}
                </button>
              </li>
            ))}
          </ul>
        )}
        <label className="sr-only" htmlFor="composer-input">{mode === "agent" ? "Describe a task" : "Message"}</label>
        <textarea id="composer-input" ref={textarea} rows={1} maxLength={mode === "agent" ? 8192 : 65536}
          value={draft} disabled={inputDisabled} placeholder={placeholder} aria-describedby={statusId}
          onChange={(event) => onDraftChange(event.currentTarget.value)} onKeyDown={onKeyDown} />
        <div className="composer-toolbar" key={lockMenus ? "menus-locked" : "menus-open"}>
          {mode === "agent" && (
            <Menu label="Add files or folders" placement="above" trigger={<PlusIcon size={17} />} className="composer-icon-menu" disabled={lockMenus}>
              {(close) => (
                <>
                  <MenuItem icon={<FileIcon size={15} />} disabled={grantDisabled || scopeAction !== null}
                    hint="Read-only, this session" onSelect={() => { close(); onGrant("file"); }}>
                    {scopeAction === "file" ? "Choosing file…" : "Select a file…"}
                  </MenuItem>
                  <MenuItem icon={<FolderIcon size={15} />} disabled={grantDisabled || scopeAction !== null}
                    hint="Read-only, includes subfolders" onSelect={() => { close(); onGrant("directory"); }}>
                    {scopeAction === "directory" ? "Choosing folder…" : "Select a folder…"}
                  </MenuItem>
                  <MenuSeparator />
                  <MenuItem icon={<LockIcon size={15} />} onSelect={() => { close(); onManageAccess(); }}>Manage access…</MenuItem>
                </>
              )}
            </Menu>
          )}

          <Menu label={`Mode: ${mode === "agent" ? "Agent" : "Chat"}`} placement="above" className="composer-pill-menu" disabled={lockMenus}
            trigger={<>{mode === "agent" ? <AgentIcon size={14} /> : <ChatIcon size={14} />}<span>{mode === "agent" ? "Agent" : "Chat"}</span><ChevronDownIcon size={13} /></>}>
            {(close) => (
              <>
                <MenuItem checked={mode === "chat"} icon={<ChatIcon size={15} />} hint="Talk with the local model"
                  onSelect={() => { close(); onModeChange("chat"); }}>Chat</MenuItem>
                <MenuItem checked={mode === "agent"} icon={<AgentIcon size={15} />} disabled={!agentAvailable}
                  hint={agentAvailable ? "Use approved tools on this device" : agentUnavailable ?? "Unavailable"}
                  onSelect={() => { close(); onModeChange("agent"); }}>Agent</MenuItem>
              </>
            )}
          </Menu>

          <Menu label="Model" placement="above" className="composer-pill-menu" disabled={lockMenus}
            trigger={<>
              {modelBusy ? <Spinner size={12} /> : <CubeIcon size={14} />}
              <span className="pill-text">{modelBusy ?? (active ? modelLabel(active.fileName) : "No model")}</span>
              <ChevronDownIcon size={13} />
            </>}>
            {(close) => (
              <>
                {models.length === 0 && <p className="menu-empty">No models imported yet.</p>}
                {models.map((model) => (
                  <MenuItem key={model.id} checked={model.id === activeModelId} disabled={modelBusy !== null || modelLocked || inputDisabled}
                    icon={<CubeIcon size={15} />} hint={model.id === activeModelId ? "Loaded" : "Load this model"}
                    onSelect={() => { close(); if (model.id !== activeModelId) onSelectModel(model.id); }}>
                    {modelLabel(model.fileName)}
                  </MenuItem>
                ))}
                <MenuSeparator />
                <MenuItem icon={<SlidersIcon size={15} />} onSelect={() => { close(); onManageModels(); }}>Manage models…</MenuItem>
              </>
            )}
          </Menu>

          {mode === "chat" && thinkingSupported && (
            <button type="button" className="composer-pill" aria-pressed={thinking}
              title={thinking ? "Thinking is on for your next message" : "Direct answer. Turn on to let the model think first"}
              onClick={() => onThinkingChange(!thinking)}>
              <ThinkIcon size={14} /><span>Thinking</span>
            </button>
          )}

          <Popover label="Response options" placement="above" disabled={lockMenus} triggerClassName={`icon-button${optionsAttention ? " has-dot" : ""}`}
            trigger={<SlidersIcon size={16} />}>
            {mode === "chat" ? (
              <div className="options-panel">
                <label className="field" htmlFor={outputId}>
                  <span>Output limit for the next reply</span>
                  <input id={outputId} type="number" inputMode="numeric" min={1} max={4096} step={1}
                    placeholder={savedMaxTokens ? `${savedMaxTokens} (saved)` : "Saved limit"}
                    value={outputOverride} aria-invalid={overrideInvalid}
                    onChange={(event) => onOutputOverride(event.currentTarget.value)} />
                </label>
                <p className="hint">
                  Leave blank to use the model's saved limit{savedMaxTokens ? ` of ${savedMaxTokens} tokens` : ""}.
                  Must be 1–4096{contextLength ? ` and below the ${contextLength}-token context` : ""}.
                </p>
                {overrideInvalid && <p className="error-text">Enter a whole number in the allowed range.</p>}
                {savedMaxTokens !== null && lowOutputLimit({ maxTokens: savedMaxTokens }) && (
                  <p className="hint">The saved limit is below {shortOutputLimit} tokens, so longer replies may be cut off.</p>
                )}
                <button type="button" className="link-button" onClick={onOpenModelSettings}>Model settings…</button>
              </div>
            ) : (
              <div className="options-panel">
                <label className="field" htmlFor={timeoutId}>
                  <span>Task time limit (seconds)</span>
                  <input id={timeoutId} type="number" inputMode="numeric" min={1} max={maxTaskTimeoutSeconds} step={1}
                    value={timeout} aria-invalid={timeoutInvalid}
                    onChange={(event) => onTimeoutChange(event.currentTarget.value)} />
                </label>
                <p className="hint">Covers planning, tools and your approval. 1–{maxTaskTimeoutSeconds} seconds; the default is 300.</p>
                {timeoutInvalid && <p className="error-text">Enter a whole number from 1 to {maxTaskTimeoutSeconds}.</p>}
              </div>
            )}
          </Popover>

          <span className="spacer" />
          {showStop ? (
            <button type="button" className="send-button" disabled={!canStop} onClick={onStop}
              aria-label={stopping ? "Stopping" : stopLabel} title={stopLabel}>
              {stopping ? <Spinner size={14} /> : <StopIcon size={16} />}
            </button>
          ) : (
            <button type="submit" className="send-button" disabled={!canSend}
              aria-label={mode === "agent" ? "Start task" : "Send message"} title={mode === "agent" ? "Start task" : "Send"}>
              <ArrowUpIcon size={17} />
            </button>
          )}
        </div>
      </form>
      {scopeError && mode === "agent" && <p className="error-text composer-error" role="alert">{scopeError}</p>}
      <p id={statusId} className="composer-status">{status}</p>
    </div>
  );
}
