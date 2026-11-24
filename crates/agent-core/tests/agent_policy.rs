use std::time::Duration;

use agent_core::{
    agent::{
        AuditAction, AuthorizationDecision, DenialReason, PermissionEngine, ScopeKind, ScopeState,
        TaskId, ToolId, ToolRegistry,
    },
    inference::Cancellation,
};

const WAIT: Duration = Duration::from_secs(20);

fn setup() -> (PermissionEngine, ToolRegistry, TaskId, Cancellation) {
    let mut policy = PermissionEngine::new("test_session").unwrap();
    let registry = ToolRegistry::mvp().unwrap();
    let task = TaskId::new(1).unwrap();
    let cancellation = Cancellation::default();
    policy.begin_task(task, WAIT, cancellation.clone()).unwrap();
    (policy, registry, task, cancellation)
}

#[test]
fn automatic_authorization_is_single_use_and_task_bound() {
    let (mut policy, registry, task, _) = setup();
    let call = registry.validate(ToolId::Calculate, r#"{"expression":"2+2"}"#).unwrap();
    let AuthorizationDecision::Allow { operation_id } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected allow");
    };
    assert!(policy.consume(&registry, TaskId::new(2).unwrap(), &operation_id, &call).is_err());
    let permit = policy.consume(&registry, task, &operation_id, &call).unwrap();
    assert_eq!(permit.call(), &call);
    assert!(permit.check_live().is_ok());
    assert!(policy.consume(&registry, task, &operation_id, &call).is_err());
    assert!(policy.end_task(task));
    assert!(permit.check_live().is_err());
    assert!(policy.begin_task(task, WAIT, Cancellation::default()).is_err());
}

#[test]
fn launch_requires_exact_single_use_confirmation() {
    let (mut policy, registry, task, _) = setup();
    let call = registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_1"}"#).unwrap();
    let other = registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_2"}"#).unwrap();
    let AuthorizationDecision::Confirm { request } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected confirmation");
    };
    assert_eq!(request.call, call);
    assert!(policy.respond(task, "wrong_id", true).is_err());
    assert!(matches!(policy.respond(task, &request.operation_id, true).unwrap(), AuthorizationDecision::Allow { .. }));
    assert!(policy.respond(task, &request.operation_id, true).is_err());
    assert!(policy.consume(&registry, task, &request.operation_id, &other).is_err());
    assert!(policy.consume(&registry, task, &request.operation_id, &call).is_err());

    let AuthorizationDecision::Confirm { request } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("each launch requires confirmation");
    };
    assert!(matches!(policy.respond(task, &request.operation_id, false).unwrap(), AuthorizationDecision::Deny { reason: DenialReason::UserRejected }));
    assert!(policy.consume(&registry, task, &request.operation_id, &call).is_err());

    let AuthorizationDecision::Confirm { request } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected confirmation");
    };
    assert!(policy.consume(&registry, task, &request.operation_id, &call).is_err());
    assert!(policy.respond(task, &request.operation_id, true).is_err());

    let AuthorizationDecision::Confirm { request } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected confirmation");
    };
    policy.respond(task, &request.operation_id, true).unwrap();
    assert!(policy.consume(&registry, task, &request.operation_id, &call).is_ok());
    assert!(policy.consume(&registry, task, &request.operation_id, &call).is_err());
}

#[test]
fn resource_grants_enforce_kind_and_revocation() {
    let (mut policy, registry, task, _) = setup();
    let file = policy.grant_scope(ScopeKind::File, WAIT).unwrap();
    let directory = policy.grant_scope(ScopeKind::Directory, WAIT).unwrap();
    let validate = |id, scope: &str, path: Option<&str>| {
        let mut args = serde_json::json!({"scope_id":scope});
        if let Some(path) = path {
            args["relative_path"] = serde_json::json!(path);
        }
        registry.validate(id, &args.to_string()).unwrap()
    };
    for call in [
        validate(ToolId::FileSearch, &file.id, None),
        validate(ToolId::FileRead, &file.id, Some("other.txt")),
        validate(ToolId::FileRead, &directory.id, None),
    ] {
        assert!(matches!(policy.authorize(&registry, task, &call, WAIT).unwrap(),
            AuthorizationDecision::Deny { reason: DenialReason::ScopeKindMismatch }));
    }
    let unknown = validate(ToolId::FileRead, "missing", None);
    assert!(matches!(policy.authorize(&registry, task, &unknown, WAIT).unwrap(),
        AuthorizationDecision::Deny { reason: DenialReason::ScopeMissing }));

    let call = validate(ToolId::FileRead, &directory.id, Some("fixture.txt"));
    let AuthorizationDecision::Allow { operation_id } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected allow");
    };
    let permit = policy.consume(&registry, task, &operation_id, &call).unwrap();
    assert!(permit.check_live().is_ok());
    assert!(policy.revoke_scope(&directory.id));
    assert!(permit.check_live().is_err());
    assert!(matches!(policy.authorize(&registry, task, &call, WAIT).unwrap(),
        AuthorizationDecision::Deny { reason: DenialReason::ScopeRevoked }));
    assert!(policy.scopes().iter().any(|scope| scope.id == directory.id && scope.state == ScopeState::Revoked));

    let call = validate(ToolId::FileRead, &file.id, None);
    let AuthorizationDecision::Allow { operation_id } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected allow");
    };
    policy.revoke_scope(&file.id);
    assert!(policy.consume(&registry, task, &operation_id, &call).is_err());
}

#[test]
fn cancellation_discard_and_registry_checks_fail_closed() {
    let (mut policy, registry, task, cancellation) = setup();
    let call = registry.validate(ToolId::DeviceInfo, "{}").unwrap();
    let empty = ToolRegistry::new();
    assert!(matches!(policy.authorize(&empty, task, &call, WAIT).unwrap(),
        AuthorizationDecision::Deny { reason: DenialReason::UnknownTool }));
    let AuthorizationDecision::Allow { operation_id } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected allow");
    };
    assert!(policy.discard(task, &operation_id));
    assert!(policy.consume(&registry, task, &operation_id, &call).is_err());
    let AuthorizationDecision::Allow { operation_id } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected allow");
    };
    cancellation.cancel();
    assert!(policy.consume(&registry, task, &operation_id, &call).is_err());
    assert!(policy.end_task(task));
}

#[test]
fn audit_records_exclude_sensitive_arguments_and_engine_drop_revokes_permits() {
    let (mut policy, registry, task, _) = setup();
    let call = registry.validate(ToolId::Calculate, r#"{"expression":"123456789+7"}"#).unwrap();
    let AuthorizationDecision::Allow { operation_id } = policy.authorize(&registry, task, &call, WAIT).unwrap() else {
        panic!("expected allow");
    };
    let permit = policy.consume(&registry, task, &operation_id, &call).unwrap();
    let audit = policy.drain_audit();
    assert_eq!(audit.omitted, 0);
    assert_eq!(audit.records.last().unwrap().action, AuditAction::AuthorizationConsumed);
    assert!(!serde_json::to_string(&audit).unwrap().contains("123456789"));
    assert!(audit.records.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
    drop(policy);
    assert!(permit.check_live().is_err());
}
