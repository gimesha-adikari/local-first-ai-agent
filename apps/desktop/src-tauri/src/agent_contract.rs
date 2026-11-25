use super::*;
use agent_core::agent::{ExecutionStatus, ScopeState, ToolOutput};
use serde_json::json;

#[test]
fn approval_broker_rejects_expiry_and_replay_before_sending() {
    let session = AgentSession::new().unwrap();
    let task = TaskId::new(1).unwrap();
    session
        .activate(task, Cancellation::default(), None, 300_000)
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    session
        .add_approval(
            task,
            "expired",
            sender,
            Instant::now(),
            None,
            ToolId::AppLaunch,
        )
        .unwrap();
    let error = session.respond_approval(task, "expired", true).unwrap_err();
    assert_eq!(error.code, AgentErrorCode::ApprovalExpired);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let (sender, receiver) = mpsc::sync_channel(1);
    session
        .add_approval(
            task,
            "one-shot",
            sender,
            Instant::now() + Duration::from_secs(30),
            None,
            ToolId::AppLaunch,
        )
        .unwrap();
    session.respond_approval(task, "one-shot", true).unwrap();
    assert!(matches!(receiver.try_recv(), Ok(ApprovalChoice::AllowOnce)));
    let error = session
        .respond_approval(task, "one-shot", true)
        .unwrap_err();
    assert_eq!(error.code, AgentErrorCode::PermissionDenied);
}

#[test]
fn all_agent_events_keep_kind_and_camel_case_fields() {
    let task_id = || "9007199254740993".to_owned();
    let record = ExecutionRecord {
        operation_id: "op_1".into(),
        tool: ToolId::Calculate,
        status: ExecutionStatus::Succeeded,
        error: None,
    };
    let call = ToolRegistry::mvp()
        .unwrap()
        .validate(ToolId::Calculate, r#"{"expression":"2+2"}"#)
        .unwrap();
    let observation =
        ToolObservation::success(&call, ToolOutput::Calculate { value: 4.0 }).unwrap();
    let error = AgentError::new(AgentErrorCode::Cancelled, "Stopped.");
    let samples = [
        (
            AgentEvent::Started {
                task_id: task_id(),
                task_timeout_ms: 300_000,
            },
            json!({"kind":"started","taskTimeoutMs":300000}),
        ),
        (
            AgentEvent::StateChanged {
                task_id: task_id(),
                state: TaskState::AwaitingAuthorization,
            },
            json!({"kind":"state_changed","state":"awaiting_authorization"}),
        ),
        (
            AgentEvent::ToolProposed {
                task_id: task_id(),
                tool: ToolId::Calculate,
            },
            json!({"kind":"tool_proposed","tool":"math.calculate"}),
        ),
        (
            AgentEvent::ProposalRejected {
                task_id: task_id(),
                code: AgentErrorCode::InvalidProposal,
            },
            json!({"kind":"proposal_rejected","code":"invalid_proposal"}),
        ),
        (
            AgentEvent::DispatchStarted {
                task_id: task_id(),
                operation_id: "op_1".into(),
                tool: ToolId::Calculate,
            },
            json!({"kind":"dispatch_started","operationId":"op_1","tool":"math.calculate"}),
        ),
        (
            AgentEvent::ApprovalRequired {
                task_id: task_id(),
                operation_id: "op_2".into(),
                tool: ToolId::AppLaunch,
                expires_in_ms: 5000,
                target: Some(ApprovalTarget {
                    display_name: "KCalc".into(),
                    desktop_file: "/usr/share/applications/org.kde.kcalc.desktop".into(),
                    executable: "/usr/bin/kcalc".into(),
                }),
            },
            json!({"kind":"approval_required","operationId":"op_2","tool":"apps.launch","expiresInMs":5000,"target":{"displayName":"KCalc","desktopFile":"/usr/share/applications/org.kde.kcalc.desktop","executable":"/usr/bin/kcalc"}}),
        ),
        (
            AgentEvent::Execution {
                task_id: task_id(),
                record: record.clone(),
            },
            json!({"kind":"execution","record":{"operationId":"op_1","tool":"math.calculate","status":"succeeded","error":null}}),
        ),
        (
            AgentEvent::Observation {
                task_id: task_id(),
                record: record.clone(),
                observation: observation.clone(),
            },
            json!({"kind":"observation","record":{"operationId":"op_1","tool":"math.calculate","status":"succeeded","error":null},"observation":{"tool":"math.calculate","status":"success","result":{"value":4.0}}}),
        ),
        (
            AgentEvent::Finished {
                task_id: task_id(),
                state: TaskState::Cancelled,
                usage: BudgetUsage {
                    tool_calls: 1,
                    model_calls: 2,
                    retries: 0,
                    generated_tokens: 14,
                },
                answer: None,
                error: Some(error.clone()),
                executions: vec![record.clone()],
                native_results: vec![NativeResult {
                    record,
                    observation,
                }],
                journal_incomplete: false,
            },
            json!({"kind":"finished","state":"cancelled","usage":{"toolCalls":1,"modelCalls":2,"retries":0,"generatedTokens":14},"answer":null,"error":{"code":"cancelled","message":"Stopped."},"executions":[{"operationId":"op_1","tool":"math.calculate","status":"succeeded","error":null}],"nativeResults":[{"record":{"operationId":"op_1","tool":"math.calculate","status":"succeeded","error":null},"observation":{"tool":"math.calculate","status":"success","result":{"value":4.0}}}],"journalIncomplete":false}),
        ),
        (
            AgentEvent::Failed {
                task_id: task_id(),
                error,
            },
            json!({"kind":"failed","error":{"code":"cancelled","message":"Stopped."}}),
        ),
    ];
    for (event, mut expected) in samples {
        expected["taskId"] = json!(task_id());
        assert_eq!(serde_json::to_value(event).unwrap(), expected);
    }
}

#[test]
fn history_and_scope_contracts_preserve_string_identifiers() {
    let value = 9_007_199_254_740_993;
    let id = parse_task_id("9007199254740993").unwrap();
    assert_eq!(id.get(), value);
    assert!(parse_task_id("0").is_err());
    assert!(parse_task_id("9e15").is_err());
    let history = AgentTaskDetail::from(StoredTaskDetail {
        task: StoredTask {
            id,
            state: TaskState::Completed,
            created_at: 1,
            updated_at: 2,
            finalized: true,
            usage: None,
            error: None,
            conversation_id: Some(42),
            turn_id: Some(9_007_199_254_740_995),
            excluded_turns: Some(3),
        },
        events: vec![StoredTaskEvent {
            sequence: 1,
            created_at: 1,
            event: json!({"kind":"transition","transition":{"taskId":value,"to":"completed"}}),
        }],
        executions: vec![StoredExecution {
            operation_id: "op_1".into(),
            tool: "math.calculate".into(),
            status: "succeeded".into(),
            error: None,
            created_at: 1,
            updated_at: 2,
        }],
        audit: vec![StoredAudit {
            namespace: "test".into(),
            sequence: 1,
            created_at: 1,
            record: json!({"taskId":value,"operationId":"op_1","scopeId":"internal_scope_1"}),
        }],
    });
    let payload = serde_json::to_value(history).unwrap();
    for path in [
        "/task/id",
        "/events/0/event/transition/taskId",
        "/audit/0/record/taskId",
    ] {
        assert_eq!(payload.pointer(path).unwrap(), "9007199254740993");
    }
    assert_eq!(payload["executions"][0]["operationId"], "op_1");
    assert_eq!(payload["task"]["conversationId"], 42);
    assert_eq!(payload["task"]["turnId"], "9007199254740995");
    assert_eq!(payload["task"]["excludedTurns"], 3);
    assert!(payload.pointer("/audit/0/record/scopeId").is_none());
    assert_eq!(payload["nativeResults"], json!([]));
    assert_eq!(
        serde_json::to_value(AgentScopeDisplay::new(
            ScopeSummary {
                id: "internal_scope_1".into(),
                kind: ScopeKind::Directory,
                display_name: Some("Synthetic folder".into()),
                state: ScopeState::Active,
                remaining_ms: 999
            },
            "grant-1".into()
        ))
        .unwrap(),
        json!({"id":"grant-1","kind":"directory","displayName":"Synthetic folder","access":"read_only","state":"active","remainingMs":999})
    );
}

#[test]
fn active_agent_metadata_keeps_the_exact_task_and_approval_owner() {
    use super::agent_conversation::active_agent_turn;
    use agent_core::agent::TaskTransition;

    let session = AgentSession::new().unwrap();
    let task_id = TaskId::new(9_007_199_254_740_993).unwrap();
    let wrong_id = TaskId::new(2).unwrap();
    session
        .activate(
            task_id,
            Cancellation::default(),
            Some(AgentTurnLink {
                conversation_id: 42,
                turn_id: 9_007_199_254_740_995,
                assistant_id: 9_007_199_254_740_996,
                task_id,
            }),
            240_000,
        )
        .unwrap();

    let payload = || serde_json::to_value(active_agent_turn(&session).unwrap()).unwrap();
    assert_eq!(
        payload(),
        json!({
            "conversationId":42,"turnId":"9007199254740995","taskId":"9007199254740993",
            "tool":null,"startedAt":null,"taskTimeoutMs":240000,"approval":null,
        })
    );
    session
        .record_progress(task_id, &RuntimeEvent::Started)
        .unwrap();
    let started = payload()["startedAt"].as_i64().unwrap();
    assert!(started > 0);
    session
        .record_progress(
            task_id,
            &RuntimeEvent::Proposed {
                tool: ToolId::AppLaunch,
            },
        )
        .unwrap();
    session
        .record_progress(
            wrong_id,
            &RuntimeEvent::Proposed {
                tool: ToolId::DeviceInfo,
            },
        )
        .unwrap();

    let (sender, receiver) = mpsc::sync_channel(1);
    session
        .add_approval(
            task_id,
            "active-op",
            sender,
            Instant::now() + Duration::from_secs(30),
            None,
            ToolId::AppLaunch,
        )
        .unwrap();
    let active = payload();
    assert_eq!(active["startedAt"], started);
    assert_eq!(active["tool"], "apps.launch");
    assert_eq!(active["approval"]["operationId"], "active-op");
    assert_eq!(active["approval"]["tool"], "apps.launch");
    assert!(
        session
            .respond_approval(wrong_id, "active-op", true)
            .is_err()
    );
    assert!(session.respond_approval(task_id, "wrong-op", true).is_err());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    session
        .respond_approval(task_id, "active-op", false)
        .unwrap();
    assert!(matches!(receiver.try_recv(), Ok(ApprovalChoice::Deny)));
    session
        .record_progress(
            task_id,
            &RuntimeEvent::Transition {
                transition: TaskTransition {
                    task_id,
                    sequence: 1,
                    from: TaskState::AwaitingAuthorization,
                    to: TaskState::Denied,
                },
            },
        )
        .unwrap();
    assert!(payload()["approval"].is_null());
    assert!(payload()["tool"].is_null());
    session.clear_active(task_id);
    assert!(active_agent_turn(&session).unwrap().is_none());
}

#[test]
fn saved_errors_use_fixed_copy_and_omit_unknown_payloads() {
    let (code, message) = saved_task_error(TaskState::Denied, Some("permission_denied".into()));
    assert_eq!(code.as_deref(), Some("permission_denied"));
    assert!(message.unwrap().starts_with("Access was denied."));
    let (code, message) =
        saved_task_error(TaskState::Failed, Some("/private/synthetic.pdf".into()));
    assert_eq!(code.as_deref(), Some("unknown"));
    assert_eq!(message, Some("The Agent task could not finish."));
    let (code, message) = saved_task_error(TaskState::Interrupted, None);
    assert_eq!(code, None);
    assert!(message.unwrap().contains("interrupted"));
}
