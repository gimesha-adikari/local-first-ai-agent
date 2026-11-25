use std::time::Duration;

use agent_core::{
    agent::{
        AuthorizationDecision, AuthorizedCall, PermissionEngine, TaskId, ToolId, ToolOutcome,
        ToolOutput, ToolRegistry,
    },
    inference::Cancellation,
};
use agent_tools::{execute, supported_registry};

fn authorize(
    policy: &mut PermissionEngine,
    registry: &ToolRegistry,
    task: TaskId,
    tool: ToolId,
    args: &str,
) -> AuthorizedCall {
    let call = registry.validate(tool, args).unwrap();
    let AuthorizationDecision::Allow { operation_id } = policy
        .authorize(registry, task, &call, Duration::from_secs(10))
        .unwrap()
    else {
        panic!("expected authorization")
    };
    policy
        .consume(registry, task, &operation_id, &call)
        .unwrap()
}

#[test]
fn registry_exposes_only_implemented_tools() {
    let registry = supported_registry().unwrap();
    assert!(registry.get(ToolId::Calculate).is_some());
    assert_eq!(
        registry.get(ToolId::DeviceInfo).is_some(),
        cfg!(target_os = "linux")
    );
    assert_eq!(
        registry.get(ToolId::FileSearch).is_some(),
        cfg!(target_os = "linux")
    );
    assert_eq!(
        registry.get(ToolId::FileRead).is_some(),
        cfg!(target_os = "linux")
    );
    assert_eq!(
        registry.get(ToolId::AppsList).is_some(),
        cfg!(target_os = "linux")
    );
    assert_eq!(
        registry.get(ToolId::AppLaunch).is_some(),
        cfg!(target_os = "linux")
    );
}

#[test]
fn authorized_calculator_returns_typed_observation() {
    let registry = supported_registry().unwrap();
    let mut policy = PermissionEngine::new("test_session").unwrap();
    let task = TaskId::new(1).unwrap();
    policy
        .begin_task(task, Duration::from_secs(20), Cancellation::default())
        .unwrap();
    let permit = authorize(
        &mut policy,
        &registry,
        task,
        ToolId::Calculate,
        r#"{"expression":"2+3*4"}"#,
    );
    let observation = execute(&registry, permit).unwrap();
    match observation.outcome() {
        ToolOutcome::Success {
            result: ToolOutput::Calculate { value },
        } => assert_eq!(*value, 14.0),
        other => panic!("unexpected observation: {other:?}"),
    }
    let permit = authorize(
        &mut policy,
        &registry,
        task,
        ToolId::Calculate,
        r#"{"expression":"1/0"}"#,
    );
    assert!(matches!(
        execute(&registry, permit).unwrap().outcome(),
        ToolOutcome::Failure { .. }
    ));
}

#[test]
fn cancellation_prevents_dispatch() {
    let registry = supported_registry().unwrap();
    let mut policy = PermissionEngine::new("test_session").unwrap();
    let task = TaskId::new(1).unwrap();
    let cancellation = Cancellation::default();
    policy
        .begin_task(task, Duration::from_secs(20), cancellation.clone())
        .unwrap();
    let permit = authorize(
        &mut policy,
        &registry,
        task,
        ToolId::Calculate,
        r#"{"expression":"2+2"}"#,
    );
    cancellation.cancel();
    assert!(execute(&registry, permit).is_err());
}

#[test]
fn full_contract_registry_does_not_create_missing_native_handles() {
    let registry = ToolRegistry::mvp().unwrap();
    let mut policy = PermissionEngine::new("test_session").unwrap();
    let task = TaskId::new(1).unwrap();
    policy
        .begin_task(task, Duration::from_secs(20), Cancellation::default())
        .unwrap();
    let scope = policy
        .grant_scope(agent_core::agent::ScopeKind::File, Duration::from_secs(10))
        .unwrap();
    let args = format!(r#"{{"scope_id":"{}"}}"#, scope.id);
    let permit = authorize(&mut policy, &registry, task, ToolId::FileRead, &args);
    assert!(matches!(
        execute(&registry, permit).unwrap().outcome(),
        ToolOutcome::Failure { .. }
    ));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires real Linux procfs and root filesystem; run during deferred verification"]
fn authorized_linux_device_snapshot() {
    let registry = supported_registry().unwrap();
    let mut policy = PermissionEngine::new("test_session").unwrap();
    let task = TaskId::new(1).unwrap();
    policy
        .begin_task(task, Duration::from_secs(20), Cancellation::default())
        .unwrap();
    let permit = authorize(&mut policy, &registry, task, ToolId::DeviceInfo, "{}");
    let observation = execute(&registry, permit).unwrap();
    match observation.outcome() {
        ToolOutcome::Success {
            result: ToolOutput::DeviceInfo(info),
        } => {
            assert_eq!(info.os, "Linux");
            assert!(info.logical_cpus > 0);
            assert!(info.memory_total_bytes > 0);
            assert!(
                info.memory_available_bytes
                    .is_none_or(|value| value <= info.memory_total_bytes)
            );
        }
        other => panic!("unexpected observation: {other:?}"),
    }
}
