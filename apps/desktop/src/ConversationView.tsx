import { AgentTurnView } from "./AgentView";
import type { ContinuationStatus } from "./chat";
import { ChatTurnView, type ChatActions } from "./ChatView";
import { Spinner } from "./icons";
import { Callout } from "./ui";
import {
  agentTurnDisplay, canRegenerateLatest, sameIdentity, scopeContext, type UnifiedConversation,
} from "./unified";
import type { AgentController } from "./useAgent";

/**
 * Ordered Chat and Agent turns of one conversation. Each Agent turn renders its
 * own live state, journal detail and native results; nothing is shared across turns.
 */
export default function ConversationView({
  conversation, loadedAt, agent, busy, generating, contextLength, pendingAgentPrompt,
  chatActions, continuation, onDetails,
}: {
  conversation: UnifiedConversation;
  loadedAt: number;
  agent: AgentController;
  busy: boolean;
  generating: boolean;
  contextLength: number | null;
  pendingAgentPrompt: string | null;
  chatActions: ChatActions;
  continuation: ContinuationStatus | null;
  onDetails: (turnId: string) => void;
}) {
  const scopes = scopeContext(agent.scopes);
  const regenerable = canRegenerateLatest(conversation);
  const last = conversation.turns.at(-1);

  return (
    <>
      {conversation.turns.map((turn, index) => {
        const latest = index === conversation.turns.length - 1;
        if (turn.kind === "chat") {
          return (
            <ChatTurnView key={turn.id} turn={turn} latest={latest && regenerable} busy={busy}
              contextLength={contextLength} actions={chatActions} continuation={latest ? continuation : null} />
          );
        }
        const live = agent.turns[turn.id];
        const identity = turn.taskId ? { conversationId: conversation.id, turnId: turn.id, taskId: turn.taskId } : null;
        const isActive = sameIdentity(agent.active, identity);
        const detail = turn.taskId ? agent.details[turn.taskId] : undefined;
        const display = agentTurnDisplay(turn, live, detail, { running: isActive, loadedAt });
        return (
          <div key={turn.id} className="turn-group">
            <div className="user-turn" aria-label="You"><p>{turn.user.content}</p></div>
            <AgentTurnView turn={turn} live={live} detail={detail} display={display} agent={agent}
              scopes={scopes} isActive={isActive} onDetails={onDetails} />
          </div>
        );
      })}
      {pendingAgentPrompt && (
        <div className="turn-group">
          <div className="user-turn" aria-label="You"><p>{pendingAgentPrompt}</p></div>
          <div className="assistant-turn agent-turn">
            <p className="activity-live" role="status"><Spinner size={13} />Starting Agent turn…</p>
          </div>
        </div>
      )}
      {last?.kind === "chat" && last.assistants.length === 0 && !generating && (
        <Callout tone="neutral" title="No response yet">
          There's no saved reply to your last message. Send it again or use Regenerate when a model is loaded.
        </Callout>
      )}
    </>
  );
}
