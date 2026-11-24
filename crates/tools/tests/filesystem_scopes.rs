#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::{fs::{symlink, DirBuilderExt}, net::UnixListener},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use agent_core::{
    agent::{AuthorizationDecision, AuthorizedCall, PermissionEngine, ScopeKind, TaskId, ToolId, ToolObservation, ToolOutcome, ToolOutput, ToolRegistry},
    inference::Cancellation,
};
use agent_tools::{NativeTools, supported_registry};

const WAIT: Duration = Duration::from_secs(30);
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("agent-fs-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    tools: NativeTools,
    policy: PermissionEngine,
    registry: ToolRegistry,
    task: TaskId,
    cancellation: Cancellation,
}

impl Harness {
    fn new() -> Self {
        let mut policy = PermissionEngine::new("filesystem_test").unwrap();
        let task = TaskId::new(1).unwrap();
        let cancellation = Cancellation::default();
        policy.begin_task(task, WAIT, cancellation.clone()).unwrap();
        Self { tools: NativeTools::new(), policy, registry: supported_registry().unwrap(), task, cancellation }
    }

    fn grant(&mut self, path: &std::path::Path, kind: ScopeKind) -> String {
        self.tools.filesystem().grant_selected(&mut self.policy, path, kind, WAIT).unwrap().id
    }

    fn permit(&mut self, tool: ToolId, arguments: &str) -> AuthorizedCall {
        let call = self.registry.validate(tool, arguments).unwrap();
        let AuthorizationDecision::Allow { operation_id } =
            self.policy.authorize(&self.registry, self.task, &call, WAIT).unwrap()
        else { panic!("expected allow") };
        self.policy.consume(&self.registry, self.task, &operation_id, &call).unwrap()
    }

    fn run(&mut self, tool: ToolId, arguments: &str) -> ToolObservation {
        let permit = self.permit(tool, arguments);
        self.tools.execute(&self.registry, permit).unwrap()
    }
}

fn read_args(scope: &str, relative: &str) -> String {
    format!(r#"{{"scope_id":"{scope}","relative_path":"{relative}"}}"#)
}

#[test]
fn search_respects_depth_filter_and_result_limit() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("sub")).unwrap();
    fs::write(fixture.0.join("One.TXT"), "one").unwrap();
    fs::write(fixture.0.join("sub/two.txt"), "two").unwrap();
    fs::write(fixture.0.join("ignore.md"), "ignore").unwrap();
    let mut h = Harness::new();
    let scope = h.grant(&fixture.0, ScopeKind::Directory);
    let args = format!(r#"{{"scope_id":"{scope}","extension":"txt","max_depth":0}}"#);
    let result = h.run(ToolId::FileSearch, &args);
    match result.outcome() {
        ToolOutcome::Success { result: ToolOutput::FileSearch { paths, truncated } } => {
            assert_eq!(paths, &["One.TXT".to_owned()]);
            assert!(*truncated);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    let args = format!(r#"{{"scope_id":"{scope}","extension":"txt","max_results":1}}"#);
    match h.run(ToolId::FileSearch, &args).outcome() {
        ToolOutcome::Success { result: ToolOutput::FileSearch { paths, truncated } } => {
            assert_eq!(paths.len(), 1);
            assert!(*truncated);
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn symlinks_hardlinks_special_files_and_traversal_are_rejected() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("private.txt"), "private").unwrap();
    symlink(outside.0.join("private.txt"), fixture.0.join("link.txt")).unwrap();
    symlink(&outside.0, fixture.0.join("linkdir")).unwrap();
    fs::hard_link(outside.0.join("private.txt"), fixture.0.join("hard.txt")).unwrap();
    let _socket = UnixListener::bind(fixture.0.join("socket.txt")).unwrap();
    let mut h = Harness::new();
    let scope = h.grant(&fixture.0, ScopeKind::Directory);
    for path in ["link.txt", "linkdir/private.txt", "hard.txt", "socket.txt"] {
        assert!(matches!(h.run(ToolId::FileRead, &read_args(&scope, path)).outcome(), ToolOutcome::Failure { .. }));
    }
    assert!(h.registry.validate(ToolId::FileRead, &read_args(&scope, "../private.txt")).is_err());
    assert!(h.tools.filesystem().grant_selected(&mut h.policy, &fixture.0.join("link.txt"), ScopeKind::File, WAIT).is_err());
    let args = format!(r#"{{"scope_id":"{scope}","extension":"txt"}}"#);
    match h.run(ToolId::FileSearch, &args).outcome() {
        ToolOutcome::Success { result: ToolOutput::FileSearch { paths, .. } } => assert!(paths.is_empty()),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn selected_file_is_pinned_not_replaced_by_renamed_path() {
    let fixture = Fixture::new();
    let path = fixture.0.join("selected.txt");
    fs::write(&path, "original").unwrap();
    let mut h = Harness::new();
    let scope = h.grant(&path, ScopeKind::File);
    fs::rename(&path, fixture.0.join("moved.txt")).unwrap();
    fs::write(&path, "replacement").unwrap();
    let args = format!(r#"{{"scope_id":"{scope}"}}"#);
    match h.run(ToolId::FileRead, &args).outcome() {
        ToolOutcome::Success { result: ToolOutput::FileRead { content, .. } } => assert_eq!(content, "original"),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn file_read_bounds_utf8_and_rejects_binary_or_oversized_files() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("text.txt"), "你好".repeat(1000)).unwrap();
    fs::write(fixture.0.join("escape.txt"), "\n".repeat(4096)).unwrap();
    fs::write(fixture.0.join("invalid.txt"), [0xff, 0xfe]).unwrap();
    fs::write(fixture.0.join("binary.txt"), b"abc\0def").unwrap();
    fs::write(fixture.0.join("huge.txt"), vec![b'x'; 65537]).unwrap();
    let mut h = Harness::new();
    let scope = h.grant(&fixture.0, ScopeKind::Directory);
    for path in ["text.txt", "escape.txt"] {
        match h.run(ToolId::FileRead, &read_args(&scope, path)).outcome() {
            ToolOutcome::Success { result: ToolOutput::FileRead { content, truncated, file_bytes } } => {
                assert!(content.len() <= 4096);
                assert!(*truncated);
                assert!(*file_bytes > content.len() as u64);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
    for path in ["invalid.txt", "binary.txt", "huge.txt"] {
        assert!(matches!(h.run(ToolId::FileRead, &read_args(&scope, path)).outcome(), ToolOutcome::Failure { .. }));
    }
}

#[test]
fn revoke_and_cancel_prevent_dispatch() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file.txt"), "fixture").unwrap();
    let mut h = Harness::new();
    let scope = h.grant(&fixture.0, ScopeKind::Directory);
    let permit = h.permit(ToolId::FileRead, &read_args(&scope, "file.txt"));
    assert!(h.tools.filesystem().revoke(&mut h.policy, &scope).unwrap());
    assert!(h.tools.execute(&h.registry, permit).is_err());
    let scope = h.grant(&fixture.0, ScopeKind::Directory);
    let permit = h.permit(ToolId::FileRead, &read_args(&scope, "file.txt"));
    h.cancellation.cancel();
    assert!(h.tools.execute(&h.registry, permit).is_err());
}

#[test]
fn missing_native_handle_fails_closed_even_with_policy_grant() {
    let mut h = Harness::new();
    let scope = h.policy.grant_scope(ScopeKind::File, WAIT).unwrap();
    let args = format!(r#"{{"scope_id":"{}"}}"#, scope.id);
    assert!(matches!(h.run(ToolId::FileRead, &args).outcome(), ToolOutcome::Failure { .. }));
}
