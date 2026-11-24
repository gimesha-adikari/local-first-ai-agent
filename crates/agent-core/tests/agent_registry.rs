use agent_core::agent::{
    AgentErrorCode, MAX_OBSERVATION_BYTES, MAX_PROPOSAL_BYTES, PermissionRequirement,
    ToolArguments, ToolId, ToolObservation, ToolOutput, ToolRegistry,
};

#[test]
fn registry_is_default_deny_and_rejects_duplicates() {
    let mut registry = ToolRegistry::new();
    assert_eq!(registry.validate(ToolId::Calculate, r#"{"expression":"2+2"}"#).unwrap_err().code, AgentErrorCode::UnknownTool);
    registry.register(ToolId::Calculate).unwrap();
    assert!(registry.register(ToolId::Calculate).is_err());
    assert_eq!(registry.definitions().count(), 1);
    let registry = ToolRegistry::mvp().unwrap();
    assert_eq!(registry.definitions().count(), 6);
    assert_eq!(registry.get(ToolId::AppLaunch).unwrap().permission, PermissionRequirement::ConfirmEveryLaunch);
}

#[test]
fn only_complete_registered_proposals_are_accepted() {
    let registry = ToolRegistry::mvp().unwrap();
    let call = registry.parse(r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2+2"}}"#).unwrap();
    assert_eq!(call.tool(), ToolId::Calculate);
    for source in [
        r#"{"kind":"tool_call","tool":"shell.run","arguments":{}}"#,
        r#"{"kind":"final","tool":"math.calculate","arguments":{"expression":"2"}}"#,
        r#"{"kind":"tool_call","tool":"math.calculate","tool":"device.info","arguments":{}}"#,
        r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2","expression":"3"}}"#,
        r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2","extra":true}}"#,
        r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2"}} trailing"#,
        r#"{"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2"},"approved":true}"#,
        r#"{"kind":"tool_call","tool":"device.info","arguments":[]}"#,
        r#"{"kind":"tool_call","tool":"device.info","arguments":null}"#,
        "prose",
    ] {
        assert!(registry.parse(source).is_err(), "{source}");
    }
    assert!(registry.parse(&" ".repeat(MAX_PROPOSAL_BYTES + 1)).is_err());
}

#[test]
fn filesystem_arguments_are_bounded_and_relative() {
    let registry = ToolRegistry::mvp().unwrap();
    let call = registry.validate(ToolId::FileSearch, r#"{"scope_id":"scope_1"}"#).unwrap();
    assert_eq!(call.scope_id(), Some("scope_1"));
    let ToolArguments::FileSearch(args) = call.arguments() else { panic!("wrong arguments") };
    assert_eq!((args.max_results, args.max_depth), (100, 8));
    for path in ["../secret", "/etc/passwd", "a/../b", "a//b", "a/./b", "C:\\file", "a/"] {
        let input = serde_json::json!({"scope_id":"scope_1","relative_path":path}).to_string();
        assert!(registry.validate(ToolId::FileRead, &input).is_err());
    }
    assert!(registry.validate(ToolId::FileRead, r#"{"scope_id":"scope_1","relative_path":"a/file.txt"}"#).is_ok());
    assert!(registry.validate(ToolId::FileRead, r#"{"scope_id":"scope_1"}"#).is_ok());
    assert!(registry.validate(ToolId::FileSearch, r#"{"scope_id":"scope_1","max_depth":9}"#).is_err());
    assert!(registry.validate(ToolId::FileSearch, r#"{"scope_id":"scope_1","max_results":101}"#).is_err());
    assert!(registry.validate(ToolId::FileSearch, r#"{"scope_id":"scope_1","extension":"../txt"}"#).is_err());
}

#[test]
fn launch_accepts_only_opaque_identifiers() {
    let registry = ToolRegistry::mvp().unwrap();
    assert!(registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_1"}"#).is_ok());
    assert!(registry.validate(ToolId::AppLaunch, r#"{"app_id":"/bin/sh"}"#).is_err());
    assert!(registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_1","args":["-c","anything"]}"#).is_err());
    assert!(registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_1","approved":true}"#).is_err());
}

#[test]
fn observations_require_matching_and_bounded_adapter_results() {
    let registry = ToolRegistry::mvp().unwrap();
    let call = registry.validate(ToolId::Calculate, r#"{"expression":"2+2"}"#).unwrap();
    let observation = ToolObservation::success(&call, ToolOutput::Calculate { value: 4.0 }).unwrap();
    assert_eq!(serde_json::to_value(observation).unwrap()["status"], "success");
    assert!(ToolObservation::success(&call, ToolOutput::Calculate { value: f64::NAN }).is_err());
    assert!(ToolObservation::success(&call, ToolOutput::FileSearch { paths: vec![], truncated: false }).is_err());

    let call = registry.validate(ToolId::FileRead, r#"{"scope_id":"scope_1"}"#).unwrap();
    let text = "\u{0001}".repeat(4096);
    assert!(serde_json::to_vec(&text).unwrap().len() > MAX_OBSERVATION_BYTES);
    assert!(ToolObservation::success(&call, ToolOutput::FileRead { content: text, file_bytes: 4096, truncated: false }).is_err());

    let call = registry.validate(ToolId::AppLaunch, r#"{"app_id":"app_1"}"#).unwrap();
    assert!(ToolObservation::success(&call, ToolOutput::AppLaunch { app_id: "app_2".into(), dispatched: true }).is_err());
    assert!(ToolObservation::success(&call, ToolOutput::AppLaunch { app_id: "app_1".into(), dispatched: false }).is_err());
}
