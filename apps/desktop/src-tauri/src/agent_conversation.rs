//! Conversation-aware desktop contracts and bounded session observations.
use super::*;
use agent_core::{
    agent::{ToolOutcome, ToolOutput},
    chat::{CompletedTurn, ConversationId, Message, MessageId},
};
use serde_json::json;
use std::collections::VecDeque;
use storage::{AgentTurnLink, TurnKind};

#[cfg(all(test, target_os = "linux"))]
#[path = "agent_conversation_tests.rs"]
mod tests;

pub(super) const AGENT_TURN_EVENT: &str = "agent-turn-event";
const CACHED_TURNS: usize = 8;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurnIdentity {
    pub conversation_id: ConversationId,
    pub turn_id: String,
    pub task_id: String,
}

impl From<AgentTurnLink> for AgentTurnIdentity {
    fn from(turn: AgentTurnLink) -> Self {
        Self {
            conversation_id: turn.conversation_id,
            turn_id: turn.turn_id.to_string(),
            task_id: turn.task_id.get().to_string(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentTurnEvent {
    pub conversation_id: ConversationId,
    pub turn_id: String,
    pub excluded_turns: usize,
    pub event: AgentEvent,
}

pub(super) fn emit_turn(
    app: &AppHandle,
    turn: Option<AgentTurnLink>,
    excluded_turns: usize,
    event: AgentEvent,
) -> bool {
    let Some(turn) = turn else {
        return emit(app, event);
    };
    let sent = app
        .emit(
            AGENT_TURN_EVENT,
            AgentTurnEvent {
                conversation_id: turn.conversation_id,
                turn_id: turn.turn_id.to_string(),
                excluded_turns,
                event: event.clone(),
            },
        )
        .is_ok();
    let compatibility = emit(app, event);
    sent || compatibility
}

pub(super) struct CachedResult {
    pub native: NativeResult,
    pub scope_id: Option<String>,
}

pub(super) struct CachedTurn {
    pub link: AgentTurnLink,
    pub results: Vec<CachedResult>,
}

#[derive(Default)]
pub(super) struct SessionResults(VecDeque<CachedTurn>);

fn usable(result: &CachedResult, scopes: &[ScopeSummary]) -> bool {
    result.scope_id.as_ref().is_none_or(|id| {
        scopes
            .iter()
            .any(|scope| &scope.id == id && scope.state == ScopeState::Active)
    })
}

impl SessionResults {
    pub fn remember(&mut self, turn: CachedTurn) {
        if turn.results.is_empty() {
            return;
        }
        if self.0.len() >= CACHED_TURNS {
            self.0.pop_front();
        }
        self.0.push_back(turn);
    }

    pub fn forget_conversation(&mut self, id: ConversationId) {
        self.0.retain(|turn| turn.link.conversation_id != id);
    }

    pub fn revoke(&mut self, id: &str) {
        for turn in &mut self.0 {
            turn.results
                .retain(|result| result.scope_id.as_deref() != Some(id));
        }
    }

    pub fn results(&self, id: TaskId, scopes: &[ScopeSummary]) -> Vec<NativeResult> {
        self.0
            .iter()
            .find(|turn| turn.link.task_id == id)
            .into_iter()
            .flat_map(|turn| &turn.results)
            .filter(|result| usable(result, scopes))
            .map(|result| result.native.clone())
            .collect()
    }

    pub fn for_model(
        &self,
        id: ConversationId,
        turn: MessageId,
        scopes: &[ScopeSummary],
    ) -> Vec<Value> {
        self.0
            .iter()
            .find(|entry| entry.link.conversation_id == id && entry.link.turn_id == turn)
            .into_iter()
            .flat_map(|entry| &entry.results)
            .filter(|result| usable(result, scopes))
            .filter_map(model_result)
            .collect()
    }
}

fn model_result(result: &CachedResult) -> Option<Value> {
    let observation = &result.native.observation;
    let ToolOutcome::Success { result: output } = observation.outcome() else {
        return None;
    };
    let value = match output {
        ToolOutput::FileSearch {
            paths,
            truncated,
            extension,
        } => json!({
            "matching_paths_in_native_order":paths.iter().take(8).collect::<Vec<_>>(),
            "returned_path_count":paths.len(), "truncated":*truncated || paths.len()>8,
            "extension":extension,
            "current_directory_scope":result.scope_id,
        }),
        ToolOutput::FileRead {
            content, truncated, ..
        } => {
            let bound = content.floor_char_boundary(content.len().min(1024));
            json!({
                "file_preview":&content[..bound], "truncated":*truncated || bound < content.len(),
                "current_read_scope":result.scope_id,
            })
        }
        ToolOutput::PdfRead {
            pages,
            page_count,
            first_page,
            last_page,
            truncated,
            ..
        } => {
            let mut available = 1024;
            let mut clipped = *truncated;
            let previews = pages
                .iter()
                .map(|page| {
                    let bound = page
                        .text
                        .floor_char_boundary(page.text.len().min(available));
                    available -= bound;
                    let page_truncated = page.truncated || bound < page.text.len();
                    clipped |= page_truncated;
                    json!({"page_number":page.page_number,"text":&page.text[..bound],
                    "truncated":page_truncated})
                })
                .collect::<Vec<_>>();
            json!({
                "pages":previews, "page_count":page_count, "first_page":first_page,
                "last_page":last_page, "truncated":clipped,
                "current_read_scope":result.scope_id,
            })
        }
        ToolOutput::AppsList { .. } => return None,
        ToolOutput::AppLaunch { dispatched, .. } => json!({"recorded_dispatch":dispatched}),
        _ => serde_json::to_value(output).ok()?,
    };
    Some(json!({"tool":observation.tool(),"result":value}))
}

pub(super) type PreviousTurns = Vec<(MessageId, CompletedTurn)>;

pub(super) fn apply_context(
    request: &mut agent_core::agent::DecisionRequest,
    turn: AgentTurnLink,
    history: &PreviousTurns,
    scopes: &[ScopeSummary],
    cache: &SessionResults,
) {
    request.update_scopes(scopes);
    request.set_conversation_context(history.iter().map(|(id, previous)| {
        (
            previous.clone(),
            cache.for_model(turn.conversation_id, *id, scopes),
        )
    }));
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedConversation {
    id: ConversationId,
    title: String,
    system_prompt: String,
    created_at: i64,
    updated_at: i64,
    turns: Vec<UnifiedTurn>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnifiedTurn {
    id: String,
    kind: TurnKind,
    user: Message,
    assistants: Vec<AssistantAttempt>,
    task_id: Option<String>,
    task_timeout_ms: Option<u32>,
    task_state: Option<TaskState>,
    task_error: Option<String>,
    task_error_message: Option<&'static str>,
    excluded_turns: Option<usize>,
    native_results_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistantAttempt {
    message: Message,
    model_name: Option<String>,
}

#[tauri::command]
pub fn get_unified_conversation(
    state: State<'_, Desktop>,
    id: ConversationId,
) -> agent_core::inference::Result<UnifiedConversation> {
    let (conversation, turns) = state
        .engine
        .store()
        .lock()
        .map_err(|_| context_unavailable())?
        .unified_conversation(id)?;
    let scopes = state
        .agent
        .policy
        .lock()
        .map_err(|_| context_unavailable())?
        .scopes();
    let cache = state
        .agent
        .conversation_results
        .lock()
        .map_err(|_| context_unavailable())?;
    Ok(UnifiedConversation {
        id,
        title: conversation.title,
        system_prompt: conversation.system_prompt,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
        turns: turns
            .into_iter()
            .map(|turn| {
                let kind = turn.kind();
                let (task_error, task_error_message) =
                    turn.task_state.map_or((None, None), |state| {
                        saved_task_error(state, turn.task_error)
                    });
                UnifiedTurn {
                    id: turn.user.id.to_string(),
                    kind,
                    user: turn.user,
                    assistants: turn
                        .assistants
                        .into_iter()
                        .map(|(message, model_name)| AssistantAttempt {
                            message,
                            model_name,
                        })
                        .collect(),
                    task_id: turn.agent_task.map(|id| id.get().to_string()),
                    task_timeout_ms: turn.task_timeout_ms,
                    task_state: turn.task_state,
                    task_error,
                    task_error_message,
                    excluded_turns: turn.excluded_turns,
                    native_results_available: turn
                        .agent_task
                        .is_some_and(|id| !cache.results(id, &scopes).is_empty()),
                }
            })
            .collect(),
    })
}

pub(super) fn context_unavailable() -> InferenceError {
    InferenceError::new(ErrorCode::Storage, "Conversation context is unavailable.")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAgentTurn {
    #[serde(flatten)]
    identity: AgentTurnIdentity,
    approval: Option<ActiveApproval>,
    started_at: Option<i64>,
    task_timeout_ms: u64,
    tool: Option<ToolId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveApproval {
    operation_id: String,
    expires_in_ms: u64,
    target: Option<ApprovalTarget>,
    tool: ToolId,
}

#[tauri::command]
pub fn get_active_agent_turn(state: State<'_, Desktop>) -> AgentResult<Option<ActiveAgentTurn>> {
    active_agent_turn(&state.agent)
}

pub(super) fn active_agent_turn(session: &AgentSession) -> AgentResult<Option<ActiveAgentTurn>> {
    let active = session.active.lock().map_err(|_| storage_error())?;
    Ok(active.as_ref().and_then(|task| {
        let identity = task.turn?.into();
        let now = Instant::now();
        let approval = task.approvals.iter().find_map(|(operation_id, approval)| {
            let remaining = approval.deadline.saturating_duration_since(now);
            (!remaining.is_zero()).then(|| ActiveApproval {
                operation_id: operation_id.clone(),
                expires_in_ms: remaining.as_millis() as u64,
                target: approval.target.clone(),
                tool: approval.tool,
            })
        });
        Some(ActiveAgentTurn {
            identity,
            approval,
            started_at: task.started_at,
            task_timeout_ms: task.task_timeout_ms,
            tool: task.tool,
        })
    }))
}
