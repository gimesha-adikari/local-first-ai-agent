//! Camera snapshots on Linux (Stage 04-E): an explicit gesture, the
//! engine's one-snapshot observation grant, the XDG Camera portal's consent,
//! one PipeWire frame read into memory and the camera released at once.
//! The frame is then previewed and optionally cropped before anything uses
//! it. Nothing is recorded, saved to disk or observed in the background,
//! and no permission or capture is ever restored or resumed automatically.
//! The portal's denial is final: there is no direct device fallback.

use std::{
    os::fd::{FromRawFd, OwnedFd},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use agent_core::{
    attachment::{AttachmentKind, AttachmentOrigin, AttachmentRetention},
    chat::ConversationId,
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    screen::{Frame, Region},
};
use gtk::{gio, prelude::*};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use zeroize::Zeroizing;

use crate::{
    capture::pipewire::{self, CameraNode, Captured, Profile, StreamError, StreamRecord, Target},
    commands::Desktop,
    portal_bus::{Dict, Failure, ROOT, call_with_fds, connect, property, request},
};

const CAMERA: &str = "org.freedesktop.portal.Camera";
/// The desktop may show its own permission dialog; the user may take time.
const CONSENT: Duration = Duration::from_secs(180);
/// A captured snapshot waiting for review is discarded after this long.
pub(crate) const PENDING_LIFETIME: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CameraError {
    /// No Camera portal or PipeWire.
    Unavailable,
    /// The desktop reports no camera.
    NoCamera,
    Denied,
    Cancelled,
    /// Another camera snapshot in this app is still running.
    Busy,
    /// The camera offered only formats this build cannot read.
    Unsupported,
    Timeout,
    /// The chosen camera went away.
    Disconnected,
    /// The camera did not start or the stream failed.
    Failed,
    /// The desktop ended the request without a decision.
    Interrupted,
}

impl CameraError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Unavailable => {
                "Camera snapshots need the desktop's Camera portal and PipeWire. Nothing was captured."
            }
            Self::NoCamera => "No camera is available. Nothing was captured.",
            Self::Denied => {
                "Camera access was not allowed. Nothing was captured. You can change this in your desktop's privacy settings."
            }
            Self::Cancelled => "The camera snapshot was cancelled. Nothing was kept.",
            Self::Busy => {
                "Another camera snapshot is still finishing. Nothing was captured. Try again in a moment."
            }
            Self::Unsupported => {
                "The camera offered only image formats this app can't read safely. Nothing was captured."
            }
            Self::Timeout => {
                "The camera did not deliver an image in time. It may be in use by another application. Nothing was captured."
            }
            Self::Disconnected => {
                "The camera was disconnected or is no longer available. Nothing was captured."
            }
            Self::Failed => {
                "The camera could not start. It may be in use by another application. Nothing was captured."
            }
            Self::Interrupted => {
                "The desktop could not open the camera. Nothing was captured. Try again."
            }
        }
    }

    pub(crate) fn error(self) -> InferenceError {
        let code = match self {
            Self::Denied => ErrorCode::PolicyDenied,
            Self::Cancelled => ErrorCode::Cancelled,
            Self::Timeout => ErrorCode::TimedOut,
            Self::Unavailable | Self::NoCamera | Self::Unsupported => {
                ErrorCode::UnsupportedCapability
            }
            Self::Busy => ErrorCode::Busy,
            Self::Disconnected => ErrorCode::NotFound,
            Self::Failed | Self::Interrupted => ErrorCode::GenerationFailed,
        };
        InferenceError::new(code, self.message())
    }
}

fn stream_error(error: StreamError) -> CameraError {
    match error {
        StreamError::Unavailable => CameraError::Unavailable,
        StreamError::Unsupported => CameraError::Unsupported,
        StreamError::Timeout => CameraError::Timeout,
        StreamError::Failed => CameraError::Failed,
        StreamError::Cancelled => CameraError::Cancelled,
        StreamError::NoCamera => CameraError::NoCamera,
        StreamError::Disconnected => CameraError::Disconnected,
    }
}

fn portal_error(failure: Failure, connected: bool) -> CameraError {
    match failure {
        Failure::Denied => CameraError::Denied,
        Failure::Cancelled => CameraError::Cancelled,
        Failure::Interrupted => CameraError::Interrupted,
        Failure::Unavailable if connected => CameraError::Interrupted,
        Failure::Unavailable => CameraError::Unavailable,
    }
}

/// System-derived facts for the receipt. No pixel data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraRecord {
    pub source: &'static str,
    pub portal_version: u32,
    pub camera: String,
    pub cameras_offered: usize,
    pub consent_ms: u64,
    pub stream: StreamRecord,
    /// From the gesture until the camera stream, remote and portal
    /// connection were gone.
    pub released_ms: u64,
    pub authorization: String,
    pub storage: &'static str,
}

/// Whether a camera snapshot holds the process's single camera slot.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Held from before consent until the stream, the PipeWire connection and
/// the portal connection are all gone, so snapshots never overlap.
struct Permit;

impl Permit {
    fn acquire() -> std::result::Result<Self, CameraError> {
        ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| CameraError::Busy)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

/// The camera stage a window shows while a snapshot runs.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    /// Waiting for the desktop's camera permission.
    Requesting,
    /// The camera stream is open.
    Active,
    /// The camera, stream and portal connection are released.
    Released,
}

struct Opened {
    bus: gio::DBusConnection,
    remote: OwnedFd,
    version: u32,
    consent_ms: u64,
}

/// Ask the Camera portal for access and open its PipeWire remote. The
/// connection is closed on every failure path.
async fn open(cancelled: &dyn Fn() -> bool) -> std::result::Result<Opened, CameraError> {
    let bus = connect().await.map_err(|f| portal_error(f, false))?;
    let opened = async {
        let version = property(&bus, CAMERA, "version")
            .await
            .map_err(|f| portal_error(f, false))?
            .get::<u32>()
            .ok_or(CameraError::Unavailable)?;
        let present = property(&bus, CAMERA, "IsCameraPresent")
            .await
            .map_err(|f| portal_error(f, false))?
            .get::<bool>()
            .unwrap_or(false);
        if !present {
            return Err(CameraError::NoCamera);
        }
        let started = Instant::now();
        request(
            &bus,
            CAMERA,
            "AccessCamera",
            |opts| (opts,).to_variant(),
            CONSENT,
            cancelled,
        )
        .await
        .map_err(|f| portal_error(f, true))?;
        let consent_ms = started.elapsed().as_millis() as u64;
        if cancelled() {
            return Err(CameraError::Cancelled);
        }
        let (reply, fds) = call_with_fds(
            &bus,
            ROOT,
            CAMERA,
            "OpenPipeWireRemote",
            &(Dict::new(),).to_variant(),
        )
        .await
        .map_err(|f| portal_error(f, true))?;
        let index = reply
            .get::<(gtk::glib::variant::Handle,)>()
            .map(|(handle,)| handle.0)
            .ok_or(CameraError::Interrupted)?;
        let raw = fds.get(index).map_err(|_| CameraError::Interrupted)?;
        // SAFETY: g_unix_fd_list_get returns a new duplicate we now own.
        let remote = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok((remote, version, consent_ms))
    }
    .await;
    match opened {
        Ok((remote, version, consent_ms)) => Ok(Opened {
            bus,
            remote,
            version,
            consent_ms,
        }),
        Err(error) => {
            let _ = bus.close_future().await;
            Err(error)
        }
    }
}

async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    task: impl FnOnce(tokio::sync::oneshot::Sender<T>) + Send + 'static,
) -> std::result::Result<T, CameraError> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || task(reply))
        .map_err(|_| CameraError::Failed)?;
    answer.await.map_err(|_| CameraError::Failed)
}

pub(crate) enum Snapshot {
    Frame {
        frame: Frame,
        record: Box<CameraRecord>,
    },
    /// Several cameras: the user chooses, then asks again.
    Choose(Vec<CameraNode>),
}

/// Take one snapshot after an explicit gesture. `observation` is the
/// engine's grant for this one snapshot; it is dropped (revoked) here once
/// the camera is released. `stage` reports progress to the window.
pub(crate) async fn snapshot(
    app: &AppHandle,
    device: Option<String>,
    observation: engine::visual::CameraObservation,
    cancellation: Cancellation,
    stage: impl Fn(Stage),
) -> std::result::Result<Snapshot, CameraError> {
    let started = Instant::now();
    let permit = Permit::acquire()?;
    if cancellation.is_cancelled() {
        return Err(CameraError::Cancelled);
    }
    let authorization = format!(
        "observe grant {} for one snapshot, revoked at release",
        observation.grant()
    );
    stage(Stage::Requesting);
    let cancel = cancellation.clone();
    let opened = on_main(app, move |reply| {
        gtk::glib::MainContext::default().spawn_local(async move {
            let _ = reply.send(open(&|| cancel.is_cancelled()).await);
        });
    })
    .await
    .and_then(|opened| opened);
    let opened = match opened {
        Ok(opened) => opened,
        Err(error) => {
            drop(observation);
            stage(Stage::Released);
            return Err(error);
        }
    };
    let Opened {
        bus,
        remote,
        version,
        consent_ms,
    } = opened;
    stage(Stage::Active);
    let reading = cancellation.clone();
    let captured = tauri::async_runtime::spawn_blocking(move || {
        pipewire::capture(
            remote,
            &Profile::camera(),
            Target::Camera(device.as_deref()),
            &reading,
        )
    })
    .await
    .map_err(|_| CameraError::Failed)
    .and_then(|result| result.map_err(stream_error));
    // The PipeWire stream and connection are already gone; now the portal
    // connection goes too.
    let _ = on_main(app, move |reply| {
        gtk::glib::MainContext::default().spawn_local(async move {
            let _ = bus.close_future().await;
            let _ = reply.send(());
        });
    })
    .await;
    let released_ms = started.elapsed().as_millis() as u64;
    drop(observation);
    drop(permit);
    stage(Stage::Released);
    match captured? {
        Captured::Choose(cameras) => Ok(Snapshot::Choose(cameras)),
        Captured::Frame {
            frame,
            record: stream,
            camera,
        } => Ok(Snapshot::Frame {
            frame,
            record: Box::new(CameraRecord {
                source: "org.freedesktop.portal.Camera",
                portal_version: version,
                camera: camera
                    .as_ref()
                    .map(|(node, _)| node.name.clone())
                    .unwrap_or_default(),
                cameras_offered: camera.map_or(0, |(_, offered)| offered),
                consent_ms,
                stream,
                released_ms,
                authorization,
                storage: "memory only until you use or discard it; never written to Pictures or disk",
            }),
        }),
    }
}

/// A captured snapshot waiting for preview, crop and confirmation. The full
/// frame lives only here and is wiped when this is dropped.
pub(crate) struct Pending {
    pub id: String,
    pub frame: Frame,
    pub png: Zeroizing<Vec<u8>>,
    pub record: CameraRecord,
    pub taken: Instant,
}

impl Pending {
    pub(crate) fn new(frame: Frame, record: CameraRecord) -> Result<Self> {
        let png = crate::panel::screen::encode(&frame)?;
        Ok(Self {
            id: crate::panel::screen::crop_id(),
            frame,
            png,
            record,
            taken: Instant::now(),
        })
    }

    pub(crate) fn expired(&self) -> bool {
        self.taken.elapsed() >= PENDING_LIFETIME
    }

    pub(crate) fn info(&self) -> PendingInfo {
        PendingInfo {
            id: self.id.clone(),
            width: self.frame.width(),
            height: self.frame.height(),
            record: self.record.clone(),
        }
    }

    /// The confirmed region (or the whole frame) as an image. Nothing else
    /// of the frame is kept once the pending snapshot is dropped.
    pub(crate) fn crop(&self, region: Option<Region>) -> Result<(Frame, Region)> {
        let region = region.unwrap_or(Region {
            x: 0,
            y: 0,
            width: self.frame.width(),
            height: self.frame.height(),
        });
        let crop = self.frame.crop(region).map_err(|_| {
            InferenceError::new(
                ErrorCode::InvalidConfig,
                "Choose an area of at least 4 × 4 pixels inside the snapshot.",
            )
        })?;
        Ok((crop, region))
    }
}

/// What a window learns about a pending snapshot. The preview itself is
/// served by the window's own protocol.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInfo {
    id: String,
    width: u32,
    height: u32,
    record: CameraRecord,
}

/// The outcome of a snapshot gesture.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Taken {
    Snapshot {
        snapshot: Box<PendingInfo>,
    },
    /// Several cameras: nothing was streamed; choose one and ask again.
    Choose {
        cameras: Vec<CameraNode>,
    },
}

/// A camera key from the window, checked before use.
pub(crate) fn valid_device(device: &Option<String>) -> Result<()> {
    if device
        .as_ref()
        .is_some_and(|key| key.is_empty() || key.len() > 120 || key.chars().any(char::is_control))
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "Choose a listed camera.",
        ));
    }
    Ok(())
}

/// Take a snapshot for a window and keep it pending. Shared by the main
/// window and the contextual panel.
pub(crate) async fn take(
    window: &WebviewWindow,
    engine: &engine::Engine,
    device: Option<String>,
    cancellation: Cancellation,
) -> Result<std::result::Result<Pending, Vec<CameraNode>>> {
    valid_device(&device)?;
    let observation = engine.authorize_camera_snapshot(cancellation.clone())?;
    let app = window.app_handle().clone();
    let target = window.clone();
    #[cfg(test)]
    if let Some(frame) = FIXTURE.lock().ok().and_then(|mut queue| queue.pop_front()) {
        drop(observation);
        return fixture_snapshot(frame, &target).map(Ok);
    }
    let snapshot = snapshot(&app, device, observation, cancellation, move |stage| {
        let _ = target.emit_to(target.label(), "camera-stage", stage);
    })
    .await
    .map_err(CameraError::error)?;
    match snapshot {
        Snapshot::Choose(cameras) => Ok(Err(cameras)),
        Snapshot::Frame { frame, record } => {
            let encoded =
                tauri::async_runtime::spawn_blocking(move || Pending::new(frame, *record))
                    .await
                    .map_err(|_| CameraError::Failed.error())??;
            Ok(Ok(encoded))
        }
    }
}

/// Synthetic frames for the desktop WebView fixture, which has no portal
/// session (test builds only). The real portal path is exercised by the
/// headless-session camera test.
#[cfg(test)]
pub(crate) static FIXTURE: Mutex<std::collections::VecDeque<Frame>> =
    Mutex::new(std::collections::VecDeque::new());

#[cfg(test)]
fn fixture_snapshot(frame: Frame, window: &WebviewWindow) -> Result<Pending> {
    for stage in [Stage::Requesting, Stage::Active, Stage::Released] {
        let _ = window.emit_to(window.label(), "camera-stage", stage);
    }
    let (width, height) = (frame.width(), frame.height());
    Pending::new(
        frame,
        CameraRecord {
            source: "desktop test fixture (synthetic frame, no portal)",
            portal_version: 0,
            camera: "fixture".into(),
            cameras_offered: 1,
            consent_ms: 0,
            stream: StreamRecord {
                library: "none".into(),
                format: "RGBA".into(),
                width,
                height,
                frames: 1,
                rejected_buffers: 0,
                settled: agent_core::screen::Settled::Deadline,
                first_frame_ms: 0,
                waited_ms: 0,
            },
            released_ms: 0,
            authorization: "fixture".into(),
            storage: "memory only until you use or discard it; never written to Pictures or disk",
        },
    )
}

/// The main window's camera state: at most one running snapshot and one
/// snapshot waiting for review.
#[derive(Default)]
pub struct CameraDesk {
    operation: Mutex<Option<Cancellation>>,
    pending: Mutex<Option<Pending>>,
}

impl CameraDesk {
    fn pending(&self) -> Result<std::sync::MutexGuard<'_, Option<Pending>>> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| CameraError::Failed.error())?;
        if pending.as_ref().is_some_and(Pending::expired) {
            *pending = None;
        }
        Ok(pending)
    }
}

fn main_only(window: &WebviewWindow) -> Result<()> {
    if window.label() != "main" {
        return Err(InferenceError::new(
            ErrorCode::PolicyDenied,
            "Camera snapshots for Chat are available only in the main window.",
        ));
    }
    Ok(())
}

/// Explicit gesture in Chat: take one snapshot and keep it for review.
#[tauri::command]
pub async fn camera_capture(
    window: WebviewWindow,
    desk: State<'_, CameraDesk>,
    desktop: State<'_, Desktop>,
    device: Option<String>,
) -> Result<Taken> {
    main_only(&window)?;
    let cancellation = Cancellation::default();
    {
        let mut operation = desk
            .operation
            .lock()
            .map_err(|_| CameraError::Failed.error())?;
        if operation.is_some() {
            return Err(CameraError::Busy.error());
        }
        *operation = Some(cancellation.clone());
    }
    // A new snapshot replaces any earlier one waiting for review.
    *desk.pending()? = None;
    let result = take(&window, &desktop.engine, device, cancellation).await;
    if let Ok(mut operation) = desk.operation.lock() {
        *operation = None;
    }
    match result? {
        Err(cameras) => Ok(Taken::Choose { cameras }),
        Ok(pending) => {
            let info = Box::new(pending.info());
            *desk.pending()? = Some(pending);
            Ok(Taken::Snapshot { snapshot: info })
        }
    }
}

#[tauri::command]
pub fn camera_cancel(window: WebviewWindow, desk: State<'_, CameraDesk>) -> Result<()> {
    main_only(&window)?;
    if let Some(operation) = desk
        .operation
        .lock()
        .map_err(|_| CameraError::Failed.error())?
        .as_ref()
    {
        operation.cancel();
    }
    Ok(())
}

/// Discard the snapshot waiting for review.
#[tauri::command]
pub fn camera_discard(
    window: WebviewWindow,
    desk: State<'_, CameraDesk>,
    id: String,
) -> Result<()> {
    main_only(&window)?;
    let mut pending = desk.pending()?;
    if pending.as_ref().is_some_and(|item| item.id == id) {
        *pending = None;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachRequest {
    id: String,
    region: Option<Region>,
    conversation_id: Option<ConversationId>,
    retention: AttachmentRetention,
}

/// Attach the reviewed snapshot (or its confirmed crop) to the Chat draft
/// or conversation. The full frame is dropped here either way; only the
/// crop continues, as a temporary attachment unless the user chose to keep
/// it with the conversation.
#[tauri::command]
pub async fn camera_attach(
    window: WebviewWindow,
    desk: State<'_, CameraDesk>,
    desktop: State<'_, Desktop>,
    request: AttachRequest,
) -> Result<Vec<engine::attachments::AttachmentView>> {
    main_only(&window)?;
    if request
        .conversation_id
        .is_some_and(|id| !(1..=agent_core::knowledge::MAX_DOCUMENT_ID).contains(&id))
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "The conversation identifier is invalid.",
        ));
    }
    let pending = desk
        .pending()?
        .take()
        .filter(|pending| pending.id == request.id)
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::InvalidConfig,
                "That snapshot is no longer available. Take a new one.",
            )
        })?;
    let (crop, _) = pending.crop(request.region)?;
    let taken = pending.taken;
    drop(pending);
    let png = tauri::async_runtime::spawn_blocking(move || crate::panel::screen::encode(&crop))
        .await
        .map_err(|_| CameraError::Failed.error())??;
    let recognizer = crate::panel::screen::recognizer(&desktop.engine.visual_settings()?);
    let name = snapshot_name(taken);
    let file = crate::attachments::CameraSnapshot::new(name, png, recognizer);
    debug_assert_eq!(
        (AttachmentKind::Png, AttachmentOrigin::Camera),
        (
            engine::attachments::AttachmentFile::kind(&file),
            engine::attachments::AttachmentFile::origin(&file)
        )
    );
    desktop.engine.attach_files(
        request.conversation_id,
        request.retention,
        vec![Box::new(file)],
    )
}

/// The shared visual settings and the local recognition state, for Chat's
/// attachment and camera controls.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualState {
    settings: agent_core::visual::VisualSettings,
    ocr: crate::panel::screen::OcrState,
}

#[tauri::command]
pub fn visual_settings(window: WebviewWindow, desktop: State<'_, Desktop>) -> Result<VisualState> {
    main_only(&window)?;
    let settings = desktop.engine.visual_settings()?;
    Ok(VisualState {
        ocr: crate::panel::screen::ocr_state(&settings),
        settings,
    })
}

#[tauri::command]
pub fn visual_settings_save(
    window: WebviewWindow,
    desktop: State<'_, Desktop>,
    settings: agent_core::visual::VisualSettings,
) -> Result<VisualState> {
    main_only(&window)?;
    desktop.engine.save_visual_settings(&settings)?;
    visual_settings(window, desktop)
}

/// "Camera snapshot 14:05" in local time, without any device detail.
fn snapshot_name(taken: Instant) -> String {
    let ago = taken.elapsed().as_secs() as libc::time_t;
    // SAFETY: localtime_r writes into the provided struct only.
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) } - ago;
    let ok = !unsafe { libc::localtime_r(&now, &mut parts) }.is_null();
    if ok {
        format!("Camera snapshot {:02}:{:02}", parts.tm_hour, parts.tm_min)
    } else {
        "Camera snapshot".to_owned()
    }
}

/// `camera-preview://localhost/<id>.png`: the snapshot waiting for review,
/// served only to the main window and never cached.
pub fn preview_protocol(
    context: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let refuse = |status: u16| {
        tauri::http::Response::builder()
            .status(status)
            .header("Cache-Control", "no-store")
            .body(Vec::new())
            .unwrap_or_default()
    };
    if context.webview_label() != "main" || request.method() != tauri::http::Method::GET {
        return refuse(403);
    }
    let Some(id) = request
        .uri()
        .path()
        .strip_prefix('/')
        .and_then(|path| path.strip_suffix(".png"))
    else {
        return refuse(404);
    };
    let desk = context.app_handle().state::<CameraDesk>();
    let Ok(pending) = desk.pending() else {
        return refuse(404);
    };
    match pending.as_ref().filter(|pending| pending.id == id) {
        Some(pending) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", "image/png")
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(pending.png.to_vec())
            .unwrap_or_else(|_| refuse(500)),
        None => refuse(404),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::screen::PixelFormat;

    fn frame(width: u32, height: u32) -> Frame {
        let raw = vec![128u8; (width * height * 4) as usize];
        Frame::from_raw(
            &raw,
            width,
            height,
            width as usize * 4,
            0,
            PixelFormat::Rgba,
        )
        .unwrap()
    }

    fn record() -> CameraRecord {
        CameraRecord {
            source: "test",
            portal_version: 1,
            camera: "test".into(),
            cameras_offered: 1,
            consent_ms: 0,
            stream: StreamRecord {
                library: "none".into(),
                format: "YUY2".into(),
                width: 64,
                height: 48,
                frames: 3,
                rejected_buffers: 0,
                settled: agent_core::screen::Settled::Deadline,
                first_frame_ms: 0,
                waited_ms: 0,
            },
            released_ms: 0,
            authorization: "test".into(),
            storage: "memory",
        }
    }

    fn descriptors() -> usize {
        std::fs::read_dir("/proc/self/fd").map_or(0, |entries| entries.count())
    }

    fn descriptor_targets() -> Vec<String> {
        let mut targets: Vec<String> = std::fs::read_dir("/proc/self/fd")
            .map(|entries| {
                entries
                    .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
                    .map(|target| target.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        targets.sort();
        targets
    }

    fn threads() -> usize {
        std::fs::read_dir("/proc/self/task").map_or(0, |entries| entries.count())
    }

    /// Opt-in: the production Camera portal and PipeWire capture path in a
    /// disposable headless session (docs/verification-04-e). Synthetic
    /// cameras are GStreamer `pipewiresink` nodes with the Camera role,
    /// started and stopped here; the permission store is set with `gdbus`.
    /// No physical camera is touched.
    #[test]
    #[ignore = "needs a disposable headless session with portals, PipeWire and synthetic cameras"]
    fn portal_camera_snapshots_in_a_headless_session() {
        use std::{path::PathBuf, process::Command};
        let var = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
        let (source, fixtures, out) = (
            var("AGENT_CAMERA_SOURCE"),
            var("AGENT_CAMERA_FIXTURES"),
            var("AGENT_CAMERA_OUT"),
        );
        let context = gtk::glib::MainContext::default();
        let permission = |value: Option<&str>| {
            let store = [
                "call",
                "--session",
                "--dest",
                "org.freedesktop.impl.portal.PermissionStore",
                "--object-path",
                "/org/freedesktop/impl/portal/PermissionStore",
                "--method",
            ];
            let mut command = Command::new("gdbus");
            command.args(store);
            match value {
                Some(value) => command.args([
                    "org.freedesktop.impl.portal.PermissionStore.SetPermission",
                    "devices",
                    "true",
                    "camera",
                    crate::portal_bus::APP_ID,
                    &format!("['{value}']"),
                ]),
                None => command.args([
                    "org.freedesktop.impl.portal.PermissionStore.DeletePermission",
                    "devices",
                    "camera",
                    crate::portal_bus::APP_ID,
                ]),
            };
            let _ = command.output();
        };
        /// Stops its synthetic camera however the test ends.
        struct Source(std::process::Child);
        impl Source {
            fn stop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        impl Drop for Source {
            fn drop(&mut self) {
                self.stop();
            }
        }
        let start = |name: &str, image: Option<&str>, format: &str| {
            let mut command = Command::new("sh");
            command
                .arg(&source)
                .arg(name)
                .arg(image.map(|i| fixtures.join(i)).unwrap_or_default())
                .arg(format)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // The script execs GStreamer, so killing the child stops it.
            Source(command.spawn().expect("synthetic camera"))
        };
        let open_now = || context.block_on(open(&|| false));
        // The portal tracks cameras asynchronously; wait until it agrees.
        let open_when = |want_present: bool| {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let opened = open_now();
                let present = !matches!(opened, Err(CameraError::NoCamera));
                if present == want_present || Instant::now() > deadline {
                    return opened;
                }
                if let Ok(opened) = opened {
                    context.block_on(opened.bus.close_future()).ok();
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        };
        let close = |bus: gio::DBusConnection| {
            context.block_on(bus.close_future()).ok();
        };
        let snapshot = |target: Target<'_>, cancellation: &Cancellation| {
            let opened = open_now().map_err(|error| format!("{error:?}"))?;
            let captured =
                pipewire::capture(opened.remote, &Profile::camera(), target, cancellation);
            close(opened.bus);
            captured.map_err(|error| format!("{error:?}"))
        };
        let ocr = |frame: &Frame, name: &str| {
            let png = crate::panel::screen::encode(frame).unwrap();
            std::fs::write(out.join(name), &*png).unwrap();
            agent_tools::ocr::Tesseract::discover()
                .expect("Tesseract")
                .recognize(&png, &["eng".to_owned()], &|| Ok(()))
                .expect("OCR")
                .text
        };
        let pixel = |frame: &Frame, x: u32, y: u32| {
            let at = ((y * frame.width() + x) * 4) as usize;
            let bgra = &frame.bgra()[at..at + 4];
            (bgra[2], bgra[1], bgra[0])
        };
        let mut report = serde_json::Map::new();

        // No camera: the portal says so before any permission request.
        permission(Some("yes"));
        let none = open_when(false);
        assert_eq!(none.err(), Some(CameraError::NoCamera));
        report.insert("no_camera".into(), "NoCamera".into());

        let mut first = start("agent-test-camera", Some("camera-card.png"), "YUY2");
        // A denial in the permission store is final.
        permission(Some("no"));
        let denied = open_when(true);
        assert_eq!(denied.err(), Some(CameraError::Denied), "denied");
        report.insert("denied".into(), "Denied".into());

        // Allowed: one camera, one YUY2 snapshot, read and recognized.
        permission(Some("yes"));
        let _ = open_when(true).map(|opened| close(opened.bus));
        let captured = snapshot(Target::Camera(None), &Cancellation::default()).expect("snapshot");
        let Captured::Frame {
            frame,
            record,
            camera,
        } = captured
        else {
            let Captured::Choose(list) = captured else {
                unreachable!()
            };
            panic!("one camera needs no choice: {list:?}");
        };
        assert_eq!((frame.width(), frame.height()), (640, 480));
        assert_eq!(record.format, "YUY2");
        let (node, offered) = camera.expect("camera");
        assert_eq!((node.name.as_str(), offered), ("agent-test-camera", 1));
        let text = ocr(&frame, "camera-a.png");
        assert!(text.contains("CAMERA TEST 42"), "{text}");
        let red = pixel(&frame, 110, 370);
        let blue = pixel(&frame, 540, 370);
        assert!(red.0 > 160 && red.1 < 80 && red.2 < 80, "red {red:?}");
        assert!(blue.2 > 160 && blue.0 < 80, "blue {blue:?}");
        report.insert(
            "snapshot".into(),
            serde_json::json!({"format": record.format, "frames": record.frames,
                "firstFrameMs": record.first_frame_ms, "waitedMs": record.waited_ms,
                "settled": record.settled, "text": text.trim()}),
        );
        let baseline = (descriptors(), threads());
        let baseline_targets = descriptor_targets();

        // Two cameras: the user chooses; nothing is streamed meanwhile.
        let mut second = start("agent-second-camera", Some("second-card.png"), "BGRx");
        std::thread::sleep(Duration::from_secs(2));
        let Captured::Choose(cameras) =
            snapshot(Target::Camera(None), &Cancellation::default()).expect("list")
        else {
            panic!("two cameras need a choice");
        };
        let mut names: Vec<_> = cameras.iter().map(|c| c.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["agent-second-camera", "agent-test-camera"]);
        let key = cameras
            .iter()
            .find(|c| c.name == "agent-second-camera")
            .unwrap()
            .key
            .clone();
        let Captured::Frame { frame, record, .. } =
            snapshot(Target::Camera(Some(&key)), &Cancellation::default()).expect("chosen")
        else {
            panic!("chosen camera");
        };
        assert_eq!(record.format, "BGRX");
        let text = ocr(&frame, "camera-b.png");
        assert!(text.contains("SECOND CAMERA"), "{text}");
        report.insert(
            "chosen".into(),
            serde_json::json!({"cameras": names, "format": record.format}),
        );

        // A camera that is no longer there.
        assert_eq!(
            snapshot(
                Target::Camera(Some("agent-missing-camera")),
                &Cancellation::default()
            )
            .err(),
            Some("Disconnected".to_owned())
        );

        // Cancelled before a frame: nothing is returned.
        let stop = Cancellation::default();
        stop.cancel();
        assert_eq!(
            snapshot(Target::Camera(Some(&key)), &stop).err(),
            Some("Cancelled".to_owned())
        );

        // Disconnected while capturing.
        let capture_key = key.clone();
        let running = std::thread::spawn(move || {
            let opened = gtk::glib::MainContext::new().block_on(open(&|| false));
            let opened = opened.expect("open for disconnect");
            let result = pipewire::capture(
                opened.remote,
                &Profile::camera(),
                Target::Camera(Some(&capture_key)),
                &Cancellation::default(),
            );
            let _ = gtk::glib::MainContext::new().block_on(opened.bus.close_future());
            result.err()
        });
        std::thread::sleep(Duration::from_millis(450));
        second.stop();
        let disconnected = running.join().unwrap();
        report.insert("disconnected".into(), format!("{disconnected:?}").into());
        assert!(
            matches!(disconnected, Some(StreamError::Disconnected)),
            "{disconnected:?}"
        );

        // Only formats this build cannot read.
        let mut planar = start("agent-planar-camera", None, "I420");
        std::thread::sleep(Duration::from_secs(2));
        let unsupported = snapshot(
            Target::Camera(Some("agent-planar-camera")),
            &Cancellation::default(),
        );
        report.insert(
            "unsupported".into(),
            format!("{:?}", unsupported.as_ref().err()).into(),
        );
        assert_eq!(unsupported.err(), Some("Unsupported".to_owned()));
        planar.stop();

        // Cancelling while the desktop asks for permission closes the request
        // and its dialog. The Shell's Access dialog needs a focused window.
        let mut focus = Source(
            Command::new("python3")
                .arg("-IB")
                .arg(var("AGENT_CAMERA_FOCUS"))
                .arg(&out)
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("focus window"),
        );
        let stage = |name: &str| out.join(format!("stage-{name}"));
        let waited = |name: &str| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !stage(name).exists() {
                assert!(Instant::now() < deadline, "helper did not reach {name}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        waited("focused");
        permission(None);
        std::fs::write(stage("asking"), b"").unwrap();
        let asked = Instant::now();
        let shot = stage("shot");
        let cancelled = context.block_on(open(&|| shot.exists()));
        assert_eq!(cancelled.err(), Some(CameraError::Cancelled));
        report.insert(
            "consent_cancelled_ms".into(),
            (asked.elapsed().as_millis() as u64).into(),
        );
        focus.stop();
        permission(Some("yes"));

        // Repeated snapshots release everything they opened: no growth from
        // one snapshot to the next.
        std::thread::sleep(Duration::from_secs(1));
        let mut counts = Vec::new();
        for _ in 0..5 {
            assert!(matches!(
                snapshot(
                    Target::Camera(Some("agent-test-camera")),
                    &Cancellation::default()
                ),
                Ok(Captured::Frame { .. })
            ));
            counts.push((descriptors(), threads()));
        }
        let end_targets = descriptor_targets();
        let added: Vec<_> = end_targets
            .iter()
            .filter(|target| !baseline_targets.contains(target))
            .collect();
        report.insert(
            "resources".into(),
            serde_json::json!({"descriptorsAfterFirst": baseline.0, "threadsAfterFirst": baseline.1,
                "perSnapshot": counts, "addedSinceFirst": added}),
        );
        assert!(
            counts.windows(2).all(|pair| pair[0] == pair[1]),
            "per-snapshot growth: {counts:?}"
        );
        assert!(counts[4].1 <= baseline.1, "thread leak: {counts:?}");

        // With every camera gone the portal reports none again.
        first.stop();
        assert_eq!(open_when(false).err(), Some(CameraError::NoCamera));
        println!("CAMERA-REPORT {}", serde_json::Value::Object(report));
    }

    #[test]
    fn one_camera_snapshot_at_a_time_and_the_slot_frees_on_release() {
        let first = Permit::acquire().unwrap();
        assert_eq!(Permit::acquire().err(), Some(CameraError::Busy));
        drop(first);
        drop(Permit::acquire().unwrap());
    }

    #[test]
    fn a_pending_snapshot_crops_within_bounds_and_expires() {
        let pending = Pending::new(frame(64, 48), record()).unwrap();
        assert!(pending.png.starts_with(b"\x89PNG"));
        let (whole, region) = pending.crop(None).unwrap();
        assert_eq!((whole.width(), whole.height()), (64, 48));
        assert_eq!((region.width, region.height), (64, 48));
        let (crop, _) = pending
            .crop(Some(Region {
                x: 8,
                y: 4,
                width: 20,
                height: 10,
            }))
            .unwrap();
        assert_eq!((crop.width(), crop.height()), (20, 10));
        for outside in [
            Region {
                x: 60,
                y: 0,
                width: 10,
                height: 10,
            },
            Region {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        ] {
            assert_eq!(
                pending.crop(Some(outside)).unwrap_err().code,
                ErrorCode::InvalidConfig
            );
        }
        assert!(!pending.expired());
        // The receipt names the camera path and storage, never pixels.
        let info = serde_json::to_string(&pending.info()).unwrap();
        assert!(info.contains("memory") && info.len() < 1024);
    }

    #[test]
    fn camera_failures_are_reported_accurately() {
        assert_eq!(stream_error(StreamError::NoCamera), CameraError::NoCamera);
        assert_eq!(
            stream_error(StreamError::Disconnected),
            CameraError::Disconnected
        );
        assert_eq!(portal_error(Failure::Denied, true), CameraError::Denied);
        assert_eq!(
            portal_error(Failure::Unavailable, false),
            CameraError::Unavailable
        );
        assert_eq!(
            portal_error(Failure::Unavailable, true),
            CameraError::Interrupted
        );
        assert_eq!(CameraError::Denied.error().code, ErrorCode::PolicyDenied);
        for error in [
            CameraError::Unavailable,
            CameraError::NoCamera,
            CameraError::Denied,
            CameraError::Cancelled,
            CameraError::Busy,
            CameraError::Unsupported,
            CameraError::Timeout,
            CameraError::Disconnected,
            CameraError::Failed,
            CameraError::Interrupted,
        ] {
            assert!(
                error.message().contains("Nothing was") || error.message().contains("Nothing"),
                "{error:?}"
            );
        }
        assert!(valid_device(&None).is_ok());
        assert!(valid_device(&Some("agent-test-camera".into())).is_ok());
        for bad in ["", "a\u{0}", &"x".repeat(121)] {
            assert!(valid_device(&Some(bad.to_owned())).is_err());
        }
    }
}
