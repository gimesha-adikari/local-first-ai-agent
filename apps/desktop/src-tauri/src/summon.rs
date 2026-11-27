//! Linux activation. The command forwarder exits before Tauri opens the profile.
//! The only messages accepted on the private socket are window activation requests.
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
        net::UnixDatagram,
    },
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use gtk::{gio, glib, prelude::*};
use serde::Serialize;
use tauri::{AppHandle, Manager};

#[path = "summon_portal.rs"]
mod portal;

const APP_ID: &str = "io.github.gimesha-adikari.localfirstaiagent";
const MAX_TOKEN: usize = 1024;
const REPEAT_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq)]
struct Activation {
    panel: bool,
    token: Option<String>,
}

fn token(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= MAX_TOKEN && value.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| value.to_owned())
}

impl Activation {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![if self.panel { b'P' } else { b'W' }];
        if let Some(token) = &self.token {
            bytes.extend(token.as_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (kind, value) = bytes.split_first()?;
        let panel = match kind {
            b'P' => true,
            b'W' => false,
            _ => return None,
        };
        let token = if value.is_empty() {
            None
        } else {
            Some(token(std::str::from_utf8(value).ok()?)?)
        };
        Some(Self { panel, token })
    }
}

pub struct Instance {
    _lock: File,
    socket: UnixDatagram,
    initial: Option<Activation>,
}

pub fn claim() -> io::Result<Option<Instance>> {
    let request = Activation {
        panel: std::env::args_os().skip(1).any(|arg| arg == "--summon"),
        // Wayland launchers pass XDG_ACTIVATION_TOKEN; X11 startup
        // notification uses DESKTOP_STARTUP_ID.
        token: ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"]
            .into_iter()
            .find_map(|name| std::env::var(name).ok().as_deref().and_then(token)),
    };
    claim_in(&glib::user_runtime_dir(), request)
}

fn claim_in(directory: &Path, request: Activation) -> io::Result<Option<Instance>> {
    let meta = directory.symlink_metadata()?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != fs::metadata("/proc/self")?.uid()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "A private user runtime directory is required.",
        ));
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(directory.join(format!("{APP_ID}.lock")))?;
    let socket_path = directory.join(format!("{APP_ID}.sock"));
    match lock.try_lock() {
        Ok(()) => {
            match fs::remove_file(&socket_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let socket = UnixDatagram::bind(socket_path)?;
            socket.set_nonblocking(true)?;
            let initial = request.panel.then_some(request);
            Ok(Some(Instance {
                _lock: lock,
                socket,
                initial,
            }))
        }
        Err(TryLockError::WouldBlock) => {
            let socket = UnixDatagram::unbound()?;
            socket.set_write_timeout(Some(Duration::from_millis(150)))?;
            let packet = request.encode();
            // The primary claims the lock just before binding its socket.
            for _ in 0..25 {
                match socket.send_to(&packet, &socket_path) {
                    Ok(_) => return Ok(None),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "The running instance could not be reached.",
            ))
        }
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutStatus {
    state: &'static str,
    trigger: Option<String>,
    can_reconfigure: bool,
}

impl ShortcutStatus {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            trigger: None,
            can_reconfigure: false,
        }
    }
}

struct PlatformState {
    last_activation: Mutex<Option<Instant>>,
    status: Mutex<ShortcutStatus>,
    session: Mutex<Option<portal::Session>>,
}

fn repeated(last: &mut Option<Instant>, now: Instant) -> bool {
    // Update even for suppressed repeats, so holding a shortcut cannot keep
    // opening a panel that was dismissed while the key was down.
    last.replace(now)
        .is_some_and(|previous| now.duration_since(previous) < REPEAT_DELAY)
}

fn activate(app: &AppHandle, activation: Activation) {
    if activation.panel {
        let platform = app.state::<PlatformState>();
        if repeated(
            &mut platform.last_activation.lock().unwrap(),
            Instant::now(),
        ) {
            return;
        }
        let _ = crate::panel::present(app, activation.token.as_deref());
    } else if let Some(window) = app.get_webview_window("main")
        && let Ok(native) = window.gtk_window()
    {
        if let Some(token) = activation.token {
            native.set_startup_id(&token);
        }
        native.show();
        native.present();
    }
}

pub fn install(app: &AppHandle, instance: Option<Instance>) {
    app.manage(PlatformState {
        last_activation: Mutex::default(),
        status: Mutex::new(ShortcutStatus::new("available")),
        session: Mutex::default(),
    });
    let Some(mut instance) = instance else { return };
    let app = app.clone();
    if let Some(request) = instance.initial.take() {
        let app = app.clone();
        glib::idle_add_local_once(move || activate(&app, request));
    }
    let listener_app = app.clone();
    let fd = instance.socket.as_raw_fd();
    glib::source::unix_fd_add_local(fd, glib::IOCondition::IN, move |_, _| {
        // Keep the instance lock and bound socket for the lifetime of this source.
        let held = &instance;
        let mut bytes = [0u8; MAX_TOKEN + 2];
        if let Ok(n) = held.socket.recv(&mut bytes)
            && let Some(request) = Activation::decode(&bytes[..n])
        {
            activate(&listener_app, request);
        }
        glib::ControlFlow::Continue
    });
    portal::start(&app, false, None);
}

#[tauri::command]
pub fn shortcut_status(app: AppHandle) -> ShortcutStatus {
    app.state::<PlatformState>().status.lock().unwrap().clone()
}

#[tauri::command]
pub async fn configure_shortcut(app: AppHandle) -> Result<ShortcutStatus, String> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    let on_main = app.clone();
    app.run_on_main_thread(move || portal::start(&on_main, true, Some(reply)))
        .map_err(|_| "The shortcut settings could not open.".to_owned())?;
    answer
        .await
        .map_err(|_| "The desktop stopped configuring the shortcut.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn repeated_activation_extends_debounce_and_invalid_packets_are_ignored() {
        let mut last = None;
        let mut now = Instant::now();
        assert!(!repeated(&mut last, now));
        for _ in 0..25 {
            now += Duration::from_millis(52);
            assert!(repeated(&mut last, now));
        }
        assert!(!repeated(&mut last, now + REPEAT_DELAY));
        for invalid in [b"anything".as_slice(), b"", b"Pline\nbreak", b"W\x00"] {
            assert!(Activation::decode(invalid).is_none());
        }
        assert!(Activation::decode(&vec![b'P'; MAX_TOKEN + 2]).is_none());
    }

    #[test]
    fn single_instance_secondary_fixture() {
        let Some(directory) = std::env::var_os("AGENT_TEST_SUMMON_RUNTIME") else {
            return;
        };
        let forwarded = claim_in(
            Path::new(&directory),
            Activation {
                panel: true,
                token: Some("synthetic-activation".into()),
            },
        )
        .unwrap();
        assert!(forwarded.is_none());
    }

    #[test]
    fn fallback_forwards_across_processes_and_reclaims_a_stale_socket() {
        let directory =
            std::env::temp_dir().join(format!("agent-summon-test-{}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let first = || {
            claim_in(
                &directory,
                Activation {
                    panel: false,
                    token: None,
                },
            )
            .unwrap()
            .unwrap()
        };
        let instance = first();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "summon::tests::single_instance_secondary_fixture",
            ])
            .env("AGENT_TEST_SUMMON_RUNTIME", &directory)
            .output()
            .unwrap();
        assert!(child.status.success());
        let mut bytes = [0; MAX_TOKEN + 2];
        let n = instance.socket.recv(&mut bytes).unwrap();
        assert_eq!(
            Activation::decode(&bytes[..n]),
            Some(Activation {
                panel: true,
                token: Some("synthetic-activation".into()),
            })
        );
        drop(instance);
        let _restarted = first();
    }
}
