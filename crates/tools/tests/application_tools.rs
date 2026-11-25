#![cfg(target_os = "linux")]

use std::time::Duration;

use agent_core::{
    agent::{AuthorizationDecision, PermissionEngine, TaskId, ToolId, ToolOutcome, ToolRegistry},
    inference::Cancellation,
};
use agent_tools::{NativeTools, supported_registry};

#[test]
fn launch_requires_confirmation_and_unknown_target_fails_without_dispatch() {
    let tools = NativeTools::new();
    let registry = supported_registry().unwrap();
    let mut policy = PermissionEngine::new("apps_test").unwrap();
    let task = TaskId::new(1).unwrap();
    let duration = Duration::from_secs(20);
    policy
        .begin_task(task, duration, Cancellation::default())
        .unwrap();
    let call = registry
        .validate(ToolId::AppLaunch, r#"{"app_id":"app_missing"}"#)
        .unwrap();
    assert!(tools.applications().approval_target("app_missing").is_err());
    let AuthorizationDecision::Confirm { request } =
        policy.authorize(&registry, task, &call, duration).unwrap()
    else {
        panic!("expected confirmation")
    };
    assert!(
        policy
            .consume(&registry, task, &request.operation_id, &call)
            .is_err()
    );
    let AuthorizationDecision::Confirm { request } =
        policy.authorize(&registry, task, &call, duration).unwrap()
    else {
        panic!("expected confirmation")
    };
    policy.respond(task, &request.operation_id, true).unwrap();
    let permit = policy
        .consume(&registry, task, &request.operation_id, &call)
        .unwrap();
    let observation = tools.execute(&registry, permit).unwrap();
    assert!(matches!(observation.outcome(), ToolOutcome::Failure { .. }));
}

#[test]
fn application_registry_does_not_accept_commands_or_uris() {
    let registry = ToolRegistry::mvp().unwrap();
    for args in [
        r#"{"app_id":"/usr/bin/bash"}"#,
        r#"{"app_id":"app_1","args":["--help"]}"#,
        r#"{"app_id":"app_1","uri":"file:///etc/passwd"}"#,
    ] {
        assert!(registry.validate(ToolId::AppLaunch, args).is_err());
    }
}

#[test]
#[ignore = "requires installed Linux desktop applications; discovery only"]
fn real_discovery_returns_revalidatable_targets() {
    let tools = NativeTools::new();
    let registry = supported_registry().unwrap();
    let mut policy = PermissionEngine::new("apps_test").unwrap();
    let task = TaskId::new(1).unwrap();
    let duration = Duration::from_secs(20);
    policy
        .begin_task(task, duration, Cancellation::default())
        .unwrap();
    let call = registry.validate(ToolId::AppsList, "{}").unwrap();
    let AuthorizationDecision::Allow { operation_id } =
        policy.authorize(&registry, task, &call, duration).unwrap()
    else {
        panic!("expected allow")
    };
    let permit = policy
        .consume(&registry, task, &operation_id, &call)
        .unwrap();
    let observation = tools.execute(&registry, permit).unwrap();
    match observation.outcome() {
        ToolOutcome::Success {
            result: agent_core::agent::ToolOutput::AppsList { applications, .. },
        } => {
            assert!(
                !applications.is_empty(),
                "no eligible system desktop application"
            );
            for app in applications {
                let target = tools.applications().approval_target(&app.app_id).unwrap();
                assert_eq!(target.display_name, app.display_name);
                assert!(target.executable.is_absolute());
            }
        }
        other => panic!("unexpected observation: {other:?}"),
    }
}
