//! Restricted system desktop application discovery and launch.
//! GIO objects are created and dropped on the calling worker. The session
//! catalog stores only owned Rust data, not non-Send GObjects.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, atomic::{AtomicU64, Ordering}},
};

use agent_core::agent::{AgentError, AgentErrorCode, AgentResult, AppEntry, ApplicationArgs, ToolOutput};
use gio::prelude::*;

use crate::ToolContext;

const MAX_SCAN: usize = 1024;
const MAX_CATALOG: usize = 256;
const MAX_RESULTS: usize = 100;
const MAX_WIRE: usize = 8192 - 256;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
    mode: u32,
}

impl Stamp {
    fn from_metadata(m: &fs::Metadata) -> Self {
        Self {
            dev: m.dev(), ino: m.ino(), size: m.size(), mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(), ctime: m.ctime(), ctime_ns: m.ctime_nsec(), mode: m.mode(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    name: String,
    desktop_path: PathBuf,
    desktop_stamp: Stamp,
    executable: PathBuf,
    executable_stamp: Stamp,
}

/// Trusted host approval display data. Do not put executable paths into
/// model-visible tool results. The UI must show this exact target and bind
/// the confirmation to the existing operation ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTarget {
    pub app_id: String,
    pub display_name: String,
    pub desktop_path: PathBuf,
    pub executable: PathBuf,
}

#[derive(Default)]
pub struct ApplicationCatalog {
    entries: Mutex<BTreeMap<String, Candidate>>,
}

impl ApplicationCatalog {
    /// Only immutable catalog entries can be selected. Recheck the trusted
    /// registration before showing an approval and again before dispatch.
    pub fn approval_target(&self, app_id: &str) -> AgentResult<ApplicationTarget> {
        let candidate = self.lookup(app_id)?;
        self.revalidate(&candidate)?;
        Ok(ApplicationTarget {
            app_id: app_id.to_owned(),
            display_name: candidate.name,
            desktop_path: candidate.desktop_path,
            executable: candidate.executable,
        })
    }

    fn lookup(&self, id: &str) -> AgentResult<Candidate> {
        self.entries.lock().map_err(|_| unavailable())?.get(id).cloned().ok_or_else(denied)
    }

    fn revalidate(&self, previous: &Candidate) -> AgentResult<()> {
        // Re-open the precise trusted registration, not a desktop ID
        // resolved using the current XDG search path.
        let app = gio::DesktopAppInfo::from_filename(&previous.desktop_path).ok_or_else(denied)?;
        let current = candidate(&app).ok_or_else(denied)?;
        if &current != previous {
            return Err(AgentError::new(AgentErrorCode::PermissionDenied, "The application registration changed; discover it again."));
        }
        Ok(())
    }

    pub(crate) fn list(&self, context: &ToolContext<'_>) -> AgentResult<ToolOutput> {
        context.check()?;
        let all = gio::AppInfo::all();
        context.check()?;
        let mut truncated = all.len() > MAX_SCAN;
        let mut candidates = Vec::new();
        for app in all.into_iter().take(MAX_SCAN) {
            context.check()?;
            if let Ok(desktop) = app.downcast::<gio::DesktopAppInfo>() {
                if let Some(value) = candidate(&desktop) {
                    candidates.push(value);
                }
            }
        }
        candidates.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.desktop_path.cmp(&b.desktop_path)));
        candidates.dedup();
        let mut applications = Vec::new();
        let mut wire = 0;
        for value in candidates {
            context.check()?;
            if applications.len() >= MAX_RESULTS {
                truncated = true;
                break;
            }
            // All names reject controls. Six bytes per character-byte is a
            // conservative bound for JSON escaping; account for ID and keys.
            let cost = value.name.len() * 6 + 96;
            if wire + cost > MAX_WIRE {
                truncated = true;
                break;
            }
            let Some(id) = self.register(value.clone())? else {
                truncated = true;
                continue;
            };
            applications.push(AppEntry { app_id: id, display_name: value.name });
            wire += cost;
        }
        context.check()?;
        Ok(ToolOutput::AppsList { applications, truncated })
    }

    fn register(&self, candidate: Candidate) -> AgentResult<Option<String>> {
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        if let Some((id, _)) = entries.iter().find(|(_, old)| *old == &candidate) {
            return Ok(Some(id.clone()));
        }
        if entries.len() >= MAX_CATALOG {
            return Ok(None);
        }
        let number = NEXT_ID.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| unavailable())?;
        let id = format!("app_{number}");
        entries.insert(id.clone(), candidate);
        Ok(Some(id))
    }

    pub(crate) fn launch(&self, args: &ApplicationArgs, context: &ToolContext<'_>) -> AgentResult<ToolOutput> {
        context.check()?;
        let target = self.lookup(&args.app_id)?;
        self.revalidate(&target)?;
        // Create a fresh GIO launcher with ONLY the vetted absolute executable.
        // No original desktop arguments, file/URI operands, terminal wrapping,
        // D-Bus activation, or model-supplied values are forwarded.
        let launcher = gio::AppInfo::create_from_commandline(
            &target.executable,
            Some(&target.name),
            gio::AppInfoCreateFlags::empty(),
        ).map_err(|_| unavailable())?;
        context.check()?;
        context.authorization.check_dispatch()?;
        launcher.launch(&[], None::<&gio::AppLaunchContext>).map_err(|_| {
            AgentError::new(AgentErrorCode::ToolFailed, "Application launch was not confirmed; it may have been dispatched. Do not retry automatically.")
        })?;
        // Do not check cancellation after the side effect and erase its
        // outcome. The runtime must record it even if the task was stopped.
        Ok(ToolOutput::AppLaunch { app_id: args.app_id.clone(), dispatched: true })
    }
}

fn candidate(app: &gio::DesktopAppInfo) -> Option<Candidate> {
    if !app.should_show() || app.is_hidden() || app.is_nodisplay()
        || app.boolean("Terminal") || app.boolean("DBusActivatable")
        || app.has_key("Path")
        || app.string("Type").as_deref() != Some("Application")
    {
        return None;
    }
    let path = app.filename()?;
    if !path.starts_with("/usr/share/applications") || path.extension()?.to_str()? != "desktop" {
        return None;
    }
    let desktop_path = fs::canonicalize(&path).ok()?;
    if !desktop_path.starts_with("/usr/share/applications") {
        return None;
    }
    let desktop_stamp = trusted_file(&desktop_path, false)?;
    if desktop_stamp.size > 64 * 1024 {
        return None;
    }
    // Reconstruct using only a simple Exec subset; do not interpret quotes,
    // expansions or desktop action groups. GIO handles discovery/visibility.
    let exec = app.string("Exec")?;
    let executable = resolve_executable(simple_exec(&exec)?)?;
    let executable_stamp = trusted_file(&executable, true)?;
    let name = app.display_name().to_string();
    if name.trim().is_empty() || name.len() > 256
        || name.chars().any(|ch| ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
    {
        return None;
    }
    Some(Candidate { name, desktop_path, desktop_stamp, executable, executable_stamp })
}

fn simple_exec(exec: &str) -> Option<&str> {
    if exec.len() > 1024 || !exec.is_ascii() || exec.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let mut parts = exec.split_ascii_whitespace();
    let executable = parts.next()?;
    if !safe_executable_text(executable) {
        return None;
    }
    if let Some(field) = parts.next() {
        if !matches!(field, "%f" | "%F" | "%u" | "%U") || parts.next().is_some() {
            return None;
        }
    }
    Some(executable)
}

fn safe_executable_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= 512
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.' | b'+'))
        && !text.starts_with('-')
        && text.split('/').all(|s| s != "." && s != "..")
}

fn resolve_executable(exec: &str) -> Option<PathBuf> {
    let input = Path::new(exec);
    let path = if input.is_absolute() {
        if !input.starts_with("/usr/bin") && !input.starts_with("/bin") {
            return None;
        }
        input.to_owned()
    } else {
        if exec.contains('/') {
            return None;
        }
        Path::new("/usr/bin").join(input)
    };
    let canonical = fs::canonicalize(path).ok()?;
    if !canonical.starts_with("/usr/bin") && !canonical.starts_with("/usr/lib") && !canonical.starts_with("/usr/libexec") {
        return None;
    }
    if !safe_executable_text(canonical.to_str()?) {
        return None;
    }
    let basename = canonical.file_name()?.to_str()?;
    let original = input.file_name()?.to_str()?;
    if blocked_executable(basename) || blocked_executable(original) {
        return None;
    }
    Some(canonical)
}

fn blocked_executable(name: &str) -> bool {
    matches!(name, "sh" | "bash" | "dash" | "zsh" | "fish" | "ksh" | "env" | "sudo" | "su"
        | "pkexec" | "doas" | "busybox" | "flatpak" | "snap" | "xdg-open" | "gio" | "perl"
        | "ruby" | "node" | "php" | "make" | "systemctl" | "dbus-send" | "gdbus")
        || name.starts_with("python")
}

/// Check canonical path and all ancestors. This trusts the administrator,
/// package manager and mount namespace, not arbitrary user-installed apps.
/// Metadata is a change detector, not a cryptographic package signature.
fn trusted_file(path: &Path, executable: bool) -> Option<Stamp> {
    for parent in path.ancestors().skip(1) {
        let m = fs::symlink_metadata(parent).ok()?;
        if !m.is_dir() || m.uid() != 0 || m.mode() & 0o022 != 0 {
            return None;
        }
    }
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o022 != 0
        || (executable && (m.mode() & 0o111 == 0 || m.mode() & 0o6000 != 0))
    {
        return None;
    }
    Some(Stamp::from_metadata(&m))
}

fn denied() -> AgentError {
    AgentError::new(AgentErrorCode::PermissionDenied, "The application is not an eligible discovered target.")
}

fn unavailable() -> AgentError {
    AgentError::new(AgentErrorCode::ToolFailed, "Application discovery or launch is unavailable.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, ino: u64) -> Candidate {
        let stamp = Stamp { dev: 1, ino, size: 1, mtime: 0, mtime_ns: 0, ctime: 0, ctime_ns: 0, mode: 0o755 };
        Candidate {
            name: name.into(), desktop_path: "/usr/share/applications/example.desktop".into(),
            desktop_stamp: stamp.clone(), executable: "/usr/bin/example".into(), executable_stamp: stamp,
        }
    }

    #[test]
    fn accepts_only_simple_exec_with_optional_empty_operand() {
        assert_eq!(simple_exec("gnome-calculator"), Some("gnome-calculator"));
        assert_eq!(simple_exec("/usr/bin/example %U"), Some("/usr/bin/example"));
        for value in ["", "sh -c thing", "env FOO=x app", "app --option", "app %U %F",
            "app;other", "app $(other)", "\"app name\"", "app\n", "app %i", "../app", "-app"] {
            assert!(simple_exec(value).is_none(), "{value}");
        }
    }

    #[test]
    fn direct_shell_and_helper_names_are_blocked() {
        for value in ["sh", "bash", "dash", "env", "sudo", "python3.12", "xdg-open", "flatpak", "snap"] {
            assert!(blocked_executable(value));
        }
        assert!(!blocked_executable("gnome-calculator"));
    }

    #[test]
    fn catalog_never_rebinds_a_changed_registration() {
        let catalog = ApplicationCatalog::default();
        let original = sample("Example", 1);
        let id = catalog.register(original.clone()).unwrap().unwrap();
        assert_eq!(catalog.register(original.clone()).unwrap(), Some(id.clone()));
        let changed = sample("Example", 2);
        let new = catalog.register(changed.clone()).unwrap().unwrap();
        assert_ne!(id, new);
        assert_eq!(catalog.lookup(&id).unwrap(), original);
        assert_eq!(catalog.lookup(&new).unwrap(), changed);
        assert!(catalog.lookup("missing").is_err());
    }

    #[test]
    fn catalog_capacity_fails_closed() {
        let catalog = ApplicationCatalog::default();
        for i in 0..MAX_CATALOG {
            assert!(catalog.register(sample("Example", i as u64)).unwrap().is_some());
        }
        assert!(catalog.register(sample("New", 9999)).unwrap().is_none());
    }

    #[test]
    fn arbitrary_user_writable_file_is_not_trusted() {
        let path = std::env::temp_dir().join(format!("agent-app-policy-{}", std::process::id()));
        use std::io::Write;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        file.write_all(b"fixture").unwrap();
        let result = trusted_file(&path, false);
        drop(file);
        fs::remove_file(path).unwrap();
        assert!(result.is_none());
    }
}
