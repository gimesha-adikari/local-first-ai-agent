//! One frame from a ScreenCast PipeWire node, read into memory. libpipewire
//! is loaded at run time and only the stable C entry points are used. The
//! stream is torn down as soon as a settled frame is held. Only memory the
//! CPU can read directly (MemPtr, MemFd) is accepted; GPU (DMA-BUF) buffers
//! are rejected rather than read without synchronization.

use std::{
    ffi::{CStr, c_char, c_int, c_void},
    os::fd::{IntoRawFd, OwnedFd},
    ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicPtr, Ordering},
    },
    time::{Duration, Instant},
};

use agent_core::{
    inference::Cancellation,
    screen::{Frame, FrameError, PixelFormat, SettlePolicy, Settled},
};
use libloading::Library;
use serde::Serialize;

use super::spa;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamError {
    /// libpipewire is missing or could not connect to the portal's remote.
    Unavailable,
    /// The producer offered no format or buffer this build can read safely.
    Unsupported,
    /// No frame arrived in time.
    Timeout,
    /// The stream failed or the producer went away.
    Failed,
    Cancelled,
    /// The portal's remote offers no camera.
    NoCamera,
    /// The chosen camera disappeared before or during capture.
    Disconnected,
}

/// What one capture should negotiate and how long it may wait.
pub(crate) struct Profile {
    pub stream_name: &'static CStr,
    pub role: &'static CStr,
    pub formats: &'static [PixelFormat],
    pub size: (u32, u32),
    pub policy: SettlePolicy,
    pub first_frame: Duration,
}

impl Profile {
    pub(crate) fn screen(policy: SettlePolicy) -> Self {
        Self {
            stream_name: c"agent-region-capture",
            role: c"Screen",
            formats: &spa::SCREEN_FORMATS,
            size: (1920, 1080),
            policy,
            first_frame: FIRST_FRAME,
        }
    }

    /// Cameras stream continuously, so the frame is taken once exposure has
    /// had a moment to settle after the stream starts, never later.
    pub(crate) fn camera() -> Self {
        Self {
            stream_name: c"agent-camera-snapshot",
            role: c"Camera",
            formats: &spa::CAMERA_FORMATS,
            size: (1280, 720),
            policy: SettlePolicy {
                quiet: Duration::from_millis(200),
                minimum: Duration::from_millis(600),
                maximum: Duration::from_millis(900),
            },
            first_frame: Duration::from_secs(8),
        }
    }
}

/// A camera node the portal's remote exposes. `key` is the stable node
/// name used to ask for the same camera again; no device path is exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraNode {
    #[serde(skip)]
    pub id: u32,
    pub key: String,
    pub name: String,
}

pub(crate) enum Target<'a> {
    /// A ScreenCast node from the portal's Start response.
    Node(u32),
    /// A camera on the portal's remote: the one with this key, or the only
    /// one when no key is given.
    Camera(Option<&'a str>),
}

pub(crate) enum Captured {
    Frame {
        frame: Frame,
        record: StreamRecord,
        /// The camera used and how many were offered (camera targets only).
        camera: Option<(CameraNode, usize)>,
    },
    /// Several cameras exist and none was chosen; nothing was streamed.
    Choose(Vec<CameraNode>),
}

/// What the stream delivered, for the capture receipt.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamRecord {
    pub library: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub rejected_buffers: u32,
    pub settled: Settled,
    pub first_frame_ms: u64,
    pub waited_ms: u64,
}

const FIRST_FRAME: Duration = Duration::from_secs(5);

type InitFn = unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char);
type VersionFn = unsafe extern "C" fn() -> *const c_char;
type LoopNewFn = unsafe extern "C" fn(*const c_char, *const c_void) -> *mut c_void;
type LoopFn = unsafe extern "C" fn(*mut c_void);
type LoopStartFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type LoopGetFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type LoopSignalFn = unsafe extern "C" fn(*mut c_void, bool);
type LoopTimeFn = unsafe extern "C" fn(*mut c_void, *mut libc::timespec, i64) -> c_int;
type LoopWaitFn = unsafe extern "C" fn(*mut c_void, *const libc::timespec) -> c_int;
type ContextNewFn = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void;
type ConnectFdFn = unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, usize) -> *mut c_void;
type DisconnectFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type PropertiesNewFn = unsafe extern "C" fn(*const c_char, ...) -> *mut c_void;
type StreamNewFn = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void;
type AddListenerFn = unsafe extern "C" fn(*mut c_void, *mut Hook, *const StreamEvents, *mut c_void);
type StreamConnectFn =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut *const u8, u32) -> c_int;
type DequeueFn = unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer;
type QueueFn = unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int;
type UpdateParamsFn = unsafe extern "C" fn(*mut c_void, *mut *const u8, u32) -> c_int;
type ProxyDestroyFn = unsafe extern "C" fn(*mut c_void);

struct Api {
    _library: Library,
    version: String,
    loop_new: LoopNewFn,
    loop_destroy: LoopFn,
    loop_start: LoopStartFn,
    loop_stop: LoopFn,
    loop_lock: LoopFn,
    loop_unlock: LoopFn,
    loop_get_loop: LoopGetFn,
    loop_signal: LoopSignalFn,
    loop_get_time: LoopTimeFn,
    loop_timed_wait: LoopWaitFn,
    context_new: ContextNewFn,
    context_destroy: LoopFn,
    connect_fd: ConnectFdFn,
    core_disconnect: DisconnectFn,
    properties_new: PropertiesNewFn,
    stream_new: StreamNewFn,
    stream_destroy: LoopFn,
    stream_add_listener: AddListenerFn,
    stream_connect: StreamConnectFn,
    stream_disconnect: DisconnectFn,
    stream_dequeue: DequeueFn,
    stream_queue: QueueFn,
    stream_update_params: UpdateParamsFn,
    proxy_destroy: ProxyDestroyFn,
}

// SAFETY: the function pointers are plain C entry points of a library that
// stays loaded for the process lifetime.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, StreamError> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe { load().ok() })
        .as_ref()
        .ok_or(StreamError::Unavailable)
}

/// SAFETY: resolves documented libpipewire 0.3 symbols with their C types.
unsafe fn load() -> Result<Api, libloading::Error> {
    unsafe {
        let library = Library::new("libpipewire-0.3.so.0")?;
        macro_rules! symbol {
            ($name:literal) => {
                *library.get($name)?
            };
        }
        let init: InitFn = symbol!(b"pw_init\0");
        init(ptr::null_mut(), ptr::null_mut());
        let version: VersionFn = symbol!(b"pw_get_library_version\0");
        let version = CStr::from_ptr(version()).to_string_lossy().into_owned();
        Ok(Api {
            version,
            loop_new: symbol!(b"pw_thread_loop_new\0"),
            loop_destroy: symbol!(b"pw_thread_loop_destroy\0"),
            loop_start: symbol!(b"pw_thread_loop_start\0"),
            loop_stop: symbol!(b"pw_thread_loop_stop\0"),
            loop_lock: symbol!(b"pw_thread_loop_lock\0"),
            loop_unlock: symbol!(b"pw_thread_loop_unlock\0"),
            loop_get_loop: symbol!(b"pw_thread_loop_get_loop\0"),
            loop_signal: symbol!(b"pw_thread_loop_signal\0"),
            loop_get_time: symbol!(b"pw_thread_loop_get_time\0"),
            loop_timed_wait: symbol!(b"pw_thread_loop_timed_wait_full\0"),
            context_new: symbol!(b"pw_context_new\0"),
            context_destroy: symbol!(b"pw_context_destroy\0"),
            connect_fd: symbol!(b"pw_context_connect_fd\0"),
            core_disconnect: symbol!(b"pw_core_disconnect\0"),
            properties_new: symbol!(b"pw_properties_new\0"),
            stream_new: symbol!(b"pw_stream_new\0"),
            stream_destroy: symbol!(b"pw_stream_destroy\0"),
            stream_add_listener: symbol!(b"pw_stream_add_listener\0"),
            stream_connect: symbol!(b"pw_stream_connect\0"),
            stream_disconnect: symbol!(b"pw_stream_disconnect\0"),
            stream_dequeue: symbol!(b"pw_stream_dequeue_buffer\0"),
            stream_queue: symbol!(b"pw_stream_queue_buffer\0"),
            stream_update_params: symbol!(b"pw_stream_update_params\0"),
            proxy_destroy: symbol!(b"pw_proxy_destroy\0"),
            _library: library,
        })
    }
}

/// `struct spa_hook`: list link, callbacks, removed, priv.
#[repr(C)]
struct Hook([*mut c_void; 6]);

/// `struct pw_stream_events`, version 2.
#[repr(C)]
struct StreamEvents {
    version: u32,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const u8)>,
    add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    process: Option<unsafe extern "C" fn(*mut c_void)>,
    drained: Option<unsafe extern "C" fn(*mut c_void)>,
    command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    trigger_done: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// `struct spa_interface`: type, version and callbacks (methods, object).
/// The `pw_core_*` and `pw_registry_*` calls are inline header functions
/// that dispatch through these method tables, which are part of the stable
/// PipeWire 0.3 ABI.
#[repr(C)]
struct SpaInterface {
    kind: *const c_char,
    version: u32,
    funcs: *const c_void,
    data: *mut c_void,
}

/// The leading members of `struct pw_core_methods`, version 0.
#[repr(C)]
struct CoreMethods {
    version: u32,
    add_listener:
        unsafe extern "C" fn(*mut c_void, *mut Hook, *const CoreEvents, *mut c_void) -> c_int,
    hello: *const c_void,
    sync: unsafe extern "C" fn(*mut c_void, u32, c_int) -> c_int,
    pong: *const c_void,
    error: *const c_void,
    get_registry: unsafe extern "C" fn(*mut c_void, u32, usize) -> *mut c_void,
}

/// The leading member of `struct pw_registry_methods`, version 0.
#[repr(C)]
struct RegistryMethods {
    version: u32,
    add_listener:
        unsafe extern "C" fn(*mut c_void, *mut Hook, *const RegistryEvents, *mut c_void) -> c_int,
}

/// `struct pw_core_events`, version 0.
#[repr(C)]
struct CoreEvents {
    version: u32,
    info: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    done: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    ping: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    error: Option<unsafe extern "C" fn(*mut c_void, u32, c_int, c_int, *const c_char)>,
    remove_id: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    bound_id: Option<unsafe extern "C" fn(*mut c_void, u32, u32)>,
    add_mem: Option<unsafe extern "C" fn(*mut c_void, u32, u32, c_int, u32)>,
    remove_mem: Option<unsafe extern "C" fn(*mut c_void, u32)>,
}

/// `struct pw_registry_events`, version 0.
#[repr(C)]
struct RegistryEvents {
    version: u32,
    global: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_char, u32, *const SpaDict)>,
    global_remove: Option<unsafe extern "C" fn(*mut c_void, u32)>,
}

#[repr(C)]
struct SpaDict {
    flags: u32,
    n_items: u32,
    items: *const SpaDictItem,
}

#[repr(C)]
struct SpaDictItem {
    key: *const c_char,
    value: *const c_char,
}

const PW_ID_CORE: u32 = 0;
const VERSION_REGISTRY: u32 = 3;
/// The registry of a portal camera remote is small; this bounds a listing
/// the session manager never finishes.
const LISTING: Duration = Duration::from_secs(3);
const MAX_CAMERAS: usize = 16;

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut c_void,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaData {
    kind: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

const CHUNK_CORRUPTED: i32 = 1;
const STREAM_STATE_ERROR: c_int = -1;
const STREAM_STATE_UNCONNECTED: c_int = 0;
const STREAM_STATE_STREAMING: c_int = 3;
const DIRECTION_INPUT: u32 = 0;
const FLAG_AUTOCONNECT: u32 = 1 << 0;
const FLAG_MAP_BUFFERS: u32 = 1 << 2;
const FLAG_DONT_RECONNECT: u32 = 1 << 7;

#[derive(Default)]
struct State {
    format: Option<spa::VideoFormat>,
    latest: Option<Frame>,
    frames: u32,
    rejected: u32,
    first_at: Option<Instant>,
    last_at: Option<Instant>,
    streaming: bool,
    error: Option<StreamError>,
    cameras: Vec<CameraNode>,
    sync_seq: Option<c_int>,
    listed: bool,
    target: Option<u32>,
    /// The chosen camera's node was removed while in use.
    target_removed: bool,
}

struct Shared {
    api: &'static Api,
    thread_loop: *mut c_void,
    stream: AtomicPtr<c_void>,
    state: Mutex<State>,
}

unsafe extern "C" fn on_state(data: *mut c_void, old: c_int, state: c_int, message: *const c_char) {
    // SAFETY: `data` is the Shared box, alive until the stream is destroyed.
    let shared = unsafe { &*(data as *const Shared) };
    // A failed negotiation names formats ("no more input formats"): the
    // producer offers nothing this build reads.
    let format = state == STREAM_STATE_ERROR
        && !message.is_null()
        && unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .contains("format");
    if let Ok(mut guard) = shared.state.lock() {
        if state == STREAM_STATE_ERROR
            || (state == STREAM_STATE_UNCONNECTED && old != STREAM_STATE_UNCONNECTED)
        {
            let failure = if format && guard.format.is_none() {
                StreamError::Unsupported
            } else {
                StreamError::Failed
            };
            guard.error.get_or_insert(failure);
        }
        guard.streaming |= state == STREAM_STATE_STREAMING;
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

/// The value of `key` in a PipeWire property dictionary, bounded.
unsafe fn lookup(dict: *const SpaDict, key: &str) -> Option<String> {
    let dict = unsafe { dict.as_ref() }?;
    if dict.items.is_null() || dict.n_items > 512 {
        return None;
    }
    let items = unsafe { std::slice::from_raw_parts(dict.items, dict.n_items as usize) };
    items.iter().find_map(|item| {
        if item.key.is_null() || item.value.is_null() {
            return None;
        }
        let name = unsafe { CStr::from_ptr(item.key) };
        (name.to_bytes() == key.as_bytes()).then(|| {
            let value = unsafe { CStr::from_ptr(item.value) }.to_string_lossy();
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect()
        })
    })
}

unsafe extern "C" fn on_global(
    data: *mut c_void,
    id: u32,
    _permissions: u32,
    kind: *const c_char,
    _version: u32,
    props: *const SpaDict,
) {
    let shared = unsafe { &*(data as *const Shared) };
    if kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_bytes() != b"PipeWire:Interface:Node" {
        return;
    }
    let class = unsafe { lookup(props, "media.class") };
    let role = unsafe { lookup(props, "media.role") };
    if class.as_deref() != Some("Video/Source") || role.as_deref() != Some("Camera") {
        return;
    }
    let Some(key) = (unsafe { lookup(props, "node.name") }) else {
        return;
    };
    let name = unsafe { lookup(props, "node.description") }
        .or_else(|| unsafe { lookup(props, "node.nick") })
        .unwrap_or_else(|| key.clone());
    // A global is announced again when the portal's access permissions on
    // the remote change; the latest announcement replaces the earlier one.
    if let Ok(mut guard) = shared.state.lock() {
        guard.cameras.retain(|camera| camera.id != id);
        if guard.cameras.len() < MAX_CAMERAS {
            guard.cameras.push(CameraNode { id, key, name });
        }
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

unsafe extern "C" fn on_global_remove(data: *mut c_void, id: u32) {
    let shared = unsafe { &*(data as *const Shared) };
    if let Ok(mut guard) = shared.state.lock() {
        guard.cameras.retain(|camera| camera.id != id);
        if guard.target == Some(id) {
            guard.target_removed = true;
            guard.error.get_or_insert(StreamError::Disconnected);
        }
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

unsafe extern "C" fn on_done(data: *mut c_void, id: u32, seq: c_int) {
    let shared = unsafe { &*(data as *const Shared) };
    if let Ok(mut guard) = shared.state.lock()
        && id == PW_ID_CORE
        && guard.sync_seq == Some(seq)
    {
        guard.listed = true;
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

unsafe extern "C" fn on_core_error(
    data: *mut c_void,
    id: u32,
    _seq: c_int,
    _res: c_int,
    _message: *const c_char,
) {
    let shared = unsafe { &*(data as *const Shared) };
    if id == PW_ID_CORE
        && let Ok(mut guard) = shared.state.lock()
    {
        guard.error.get_or_insert(StreamError::Failed);
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

static CORE_EVENTS: CoreEvents = CoreEvents {
    version: 0,
    info: None,
    done: Some(on_done),
    ping: None,
    error: Some(on_core_error),
    remove_id: None,
    bound_id: None,
    add_mem: None,
    remove_mem: None,
};

static REGISTRY_EVENTS: RegistryEvents = RegistryEvents {
    version: 0,
    global: Some(on_global),
    global_remove: Some(on_global_remove),
};

unsafe extern "C" fn on_param(data: *mut c_void, id: u32, param: *const u8) {
    let shared = unsafe { &*(data as *const Shared) };
    if id != spa::PARAM_FORMAT || param.is_null() {
        return;
    }
    // SAFETY: a POD starts with its u32 body size; bounded before reading.
    let size = unsafe { ptr::read_unaligned(param as *const u32) } as usize;
    if size > 65_536 {
        return;
    }
    let pod = unsafe { std::slice::from_raw_parts(param, size + 8) };
    let parsed = spa::parse_format(pod);
    if let Ok(mut guard) = shared.state.lock() {
        match parsed {
            Some(format) if !format.modifier => guard.format = Some(format),
            _ => {
                guard.error.get_or_insert(StreamError::Unsupported);
            }
        }
    }
    let buffers = spa::buffers();
    let mut params = [buffers.as_ptr()];
    unsafe {
        (shared.api.stream_update_params)(
            shared.stream.load(Ordering::Acquire),
            params.as_mut_ptr(),
            1,
        );
        (shared.api.loop_signal)(shared.thread_loop, false);
    }
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    let shared = unsafe { &*(data as *const Shared) };
    let buffer = unsafe { (shared.api.stream_dequeue)(shared.stream.load(Ordering::Acquire)) };
    if buffer.is_null() {
        return;
    }
    // SAFETY: a dequeued pw_buffer and its spa_buffer stay valid until queued
    // again. Every length below is bounded by the buffer's own maxsize.
    let frame = unsafe { read(&*buffer, shared) };
    unsafe { (shared.api.stream_queue)(shared.stream.load(Ordering::Acquire), buffer) };
    if let Ok(mut guard) = shared.state.lock() {
        match frame {
            Ok(Some(frame)) => {
                let now = Instant::now();
                guard.first_at.get_or_insert(now);
                guard.last_at = Some(now);
                guard.frames += 1;
                guard.latest = Some(frame);
            }
            Ok(None) => {}
            Err(()) => guard.rejected += 1,
        }
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

/// Read one buffer. `Ok(None)` skips an empty or corrupted chunk; `Err`
/// rejects an unreadable buffer type or layout.
unsafe fn read(buffer: &PwBuffer, shared: &Shared) -> Result<Option<Frame>, ()> {
    let format = shared.state.lock().map_err(|_| ())?.format.ok_or(())?;
    let spa = unsafe { buffer.buffer.as_ref() }.ok_or(())?;
    if spa.n_datas < 1 || spa.datas.is_null() {
        return Err(());
    }
    let data = unsafe { &*spa.datas };
    // GPU memory cannot be read coherently without modifier negotiation and
    // explicit synchronization, so it is rejected, never guessed at.
    if data.kind == spa::DATA_DMA_BUF
        || !matches!(data.kind, spa::DATA_MEM_PTR | spa::DATA_MEM_FD)
        || data.data.is_null()
        || data.chunk.is_null()
    {
        return Err(());
    }
    let chunk = unsafe { &*data.chunk };
    if chunk.flags & CHUNK_CORRUPTED != 0 || chunk.size == 0 || data.maxsize == 0 {
        return Ok(None);
    }
    let memory =
        unsafe { std::slice::from_raw_parts(data.data as *const u8, data.maxsize as usize) };
    let Some(valid) = chunk_bytes(memory, chunk.offset, chunk.size) else {
        return Err(());
    };
    let Some(stride) = stride(chunk.stride, format.width, format.format.bytes_per_pixel()) else {
        return Err(());
    };
    match Frame::from_raw(valid, format.width, format.height, stride, 0, format.format) {
        Ok(frame) => Ok(Some(frame)),
        Err(FrameError::Size | FrameError::Layout | FrameError::Format | FrameError::Region) => {
            Err(())
        }
    }
}

/// The bytes the producer filled: `size` bytes at `offset` of the mapped
/// region. A chunk that does not fit inside the mapping is rejected rather
/// than wrapped or clamped, so nothing outside the mapping is read and no
/// stale bytes past the chunk are taken as pixels.
fn chunk_bytes(memory: &[u8], offset: u32, size: u32) -> Option<&[u8]> {
    let start = offset as usize;
    let end = start.checked_add(size as usize)?;
    memory.get(start..end)
}

/// Row stride in bytes. Zero means tightly packed rows; a negative stride
/// (bottom-up rows) is not supported and is rejected, never reinterpreted.
fn stride(stride: i32, width: u32, bytes_per_pixel: usize) -> Option<usize> {
    match stride {
        0 => (width as usize).checked_mul(bytes_per_pixel),
        positive if positive > 0 => Some(positive as usize),
        _ => None,
    }
}

static EVENTS: StreamEvents = StreamEvents {
    version: 2,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

/// Connect to `node` over the portal's PipeWire remote and return one
/// settled frame. Blocks the calling thread; the stream and connection are
/// gone when it returns.
pub(crate) fn single_frame(
    remote: OwnedFd,
    node: u32,
    policy: SettlePolicy,
    cancellation: &Cancellation,
) -> Result<(Frame, StreamRecord), StreamError> {
    match capture(
        remote,
        &Profile::screen(policy),
        Target::Node(node),
        cancellation,
    )? {
        Captured::Frame { frame, record, .. } => Ok((frame, record)),
        Captured::Choose(_) => Err(StreamError::Failed),
    }
}

/// List the cameras on a portal camera remote, choose one and return one
/// frame from it, or the list when the choice is the user's. Blocks the
/// calling thread; the registry, stream and connection are gone when it
/// returns.
pub(crate) fn capture(
    remote: OwnedFd,
    profile: &Profile,
    target: Target<'_>,
    cancellation: &Cancellation,
) -> Result<Captured, StreamError> {
    let api = api()?;
    let started = Instant::now();
    // SAFETY: each pointer below is checked before use and released in
    // reverse order, with the loop lock held where required. Hooks outlive
    // every proxy they are registered on: they are dropped only after the
    // core (and with it every proxy) is disconnected.
    unsafe {
        let thread_loop = (api.loop_new)(c"agent-capture".as_ptr(), ptr::null());
        if thread_loop.is_null() {
            return Err(StreamError::Unavailable);
        }
        let context = (api.context_new)((api.loop_get_loop)(thread_loop), ptr::null_mut(), 0);
        if context.is_null() {
            (api.loop_destroy)(thread_loop);
            return Err(StreamError::Unavailable);
        }
        if (api.loop_start)(thread_loop) < 0 {
            (api.context_destroy)(context);
            (api.loop_destroy)(thread_loop);
            return Err(StreamError::Unavailable);
        }
        (api.loop_lock)(thread_loop);
        // The remote fd is owned by PipeWire from here: closed on disconnect,
        // or by PipeWire's own error path if connecting fails. Only if core
        // allocation itself fails could it leak; it is never closed twice.
        let core = (api.connect_fd)(context, remote.into_raw_fd(), ptr::null_mut(), 0);
        let mut shared = Box::new(Shared {
            api,
            thread_loop,
            stream: AtomicPtr::new(ptr::null_mut()),
            state: Mutex::new(State::default()),
        });
        let data = &mut *shared as *mut Shared as *mut c_void;
        let mut hook = Box::new(Hook([ptr::null_mut(); 6]));
        let mut core_hook = Box::new(Hook([ptr::null_mut(); 6]));
        let mut registry_hook = Box::new(Hook([ptr::null_mut(); 6]));
        let mut registry = ptr::null_mut();
        let mut result = Err(StreamError::Unavailable);
        if !core.is_null() {
            let node = match target {
                Target::Node(node) => Ok(Ok(node)),
                Target::Camera(wanted) => {
                    registry =
                        list_cameras(&shared, core, &mut core_hook, &mut registry_hook, data);
                    if registry.is_null() {
                        Err(StreamError::Unavailable)
                    } else {
                        choose(&shared, wanted, cancellation)
                    }
                }
            };
            result = match node {
                Err(error) => Err(error),
                Ok(Err(cameras)) => Ok(Err(cameras)),
                Ok(Ok(node)) => stream(
                    &shared,
                    core,
                    profile,
                    node,
                    &mut hook,
                    data,
                    cancellation,
                    started,
                )
                .map(Ok),
            };
            let stream = shared.stream.swap(ptr::null_mut(), Ordering::AcqRel);
            if !stream.is_null() {
                (api.stream_disconnect)(stream);
                (api.stream_destroy)(stream);
            }
            if !registry.is_null() {
                (api.proxy_destroy)(registry);
            }
            (api.core_disconnect)(core);
        }
        (api.loop_unlock)(thread_loop);
        (api.loop_stop)(thread_loop);
        (api.context_destroy)(context);
        (api.loop_destroy)(thread_loop);
        drop((hook, core_hook, registry_hook));
        let state = shared.state.into_inner().unwrap_or_default();
        let (settled, first_frame_ms) = match result? {
            Ok(settled) => settled,
            Err(cameras) => return Ok(Captured::Choose(cameras)),
        };
        let format = state.format.ok_or(StreamError::Unsupported)?;
        let camera = state.target.and_then(|id| {
            let offered = state.cameras.len();
            state
                .cameras
                .iter()
                .find(|camera| camera.id == id)
                .map(|camera| (camera.clone(), offered))
        });
        let frame = state.latest.ok_or(StreamError::Timeout)?;
        Ok(Captured::Frame {
            frame,
            camera,
            record: StreamRecord {
                library: api.version.clone(),
                format: format!("{:?}", format.format).to_uppercase(),
                width: format.width,
                height: format.height,
                frames: state.frames,
                rejected_buffers: state.rejected,
                settled,
                first_frame_ms,
                waited_ms: started.elapsed().as_millis() as u64,
            },
        })
    }
}

/// Bind the registry, register listeners and ask the server to confirm
/// that the initial listing is complete. Returns the registry proxy, or
/// null if the core offers none. The loop lock is held.
unsafe fn list_cameras(
    shared: &Shared,
    core: *mut c_void,
    core_hook: &mut Hook,
    registry_hook: &mut Hook,
    data: *mut c_void,
) -> *mut c_void {
    unsafe {
        let interface = &*(core as *const SpaInterface);
        let methods = &*(interface.funcs as *const CoreMethods);
        (methods.add_listener)(interface.data, core_hook, &CORE_EVENTS, data);
        let registry = (methods.get_registry)(interface.data, VERSION_REGISTRY, 0);
        if registry.is_null() {
            return registry;
        }
        let registry_interface = &*(registry as *const SpaInterface);
        let registry_methods = &*(registry_interface.funcs as *const RegistryMethods);
        (registry_methods.add_listener)(
            registry_interface.data,
            registry_hook,
            &REGISTRY_EVENTS,
            data,
        );
        let seq = (methods.sync)(interface.data, PW_ID_CORE, 0);
        if let Ok(mut guard) = shared.state.lock() {
            guard.sync_seq = Some(seq);
        }
        registry
    }
}

/// Wait for the listing, then pick the camera. `Ok(Err(list))` means the
/// user has to choose among several.
unsafe fn choose(
    shared: &Shared,
    wanted: Option<&str>,
    cancellation: &Cancellation,
) -> Result<Result<u32, Vec<CameraNode>>, StreamError> {
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return Err(StreamError::Cancelled);
        }
        {
            let mut state = shared.state.lock().map_err(|_| StreamError::Failed)?;
            if let Some(error) = state.error {
                return Err(error);
            }
            if state.listed {
                let found = match wanted {
                    Some(key) => state
                        .cameras
                        .iter()
                        .find(|camera| camera.key == key)
                        .map(|camera| camera.id)
                        .ok_or(StreamError::Disconnected),
                    None => match state.cameras.as_slice() {
                        [] => Err(StreamError::NoCamera),
                        [only] => Ok(only.id),
                        several => return Ok(Err(several.to_vec())),
                    },
                };
                let id = found?;
                state.target = Some(id);
                return Ok(Ok(id));
            }
            if started.elapsed() > LISTING {
                return Err(StreamError::Timeout);
            }
        }
        unsafe { sleep(shared) };
    }
}

/// Release the loop lock for up to 25 ms or until a callback signals.
unsafe fn sleep(shared: &Shared) {
    let mut deadline = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        (shared.api.loop_get_time)(shared.thread_loop, &mut deadline, 25_000_000);
        (shared.api.loop_timed_wait)(shared.thread_loop, &deadline);
    }
}

/// Create and connect the stream to `node` and wait for a settled frame.
/// The loop lock is held. The stream is left in `shared` for teardown.
#[allow(clippy::too_many_arguments)]
unsafe fn stream(
    shared: &Shared,
    core: *mut c_void,
    profile: &Profile,
    node: u32,
    hook: &mut Hook,
    data: *mut c_void,
    cancellation: &Cancellation,
    started: Instant,
) -> Result<(Settled, u64), StreamError> {
    unsafe {
        let api = shared.api;
        let properties = (api.properties_new)(
            c"media.type".as_ptr(),
            c"Video".as_ptr(),
            c"media.category".as_ptr(),
            c"Capture".as_ptr(),
            c"media.role".as_ptr(),
            profile.role.as_ptr(),
            ptr::null::<c_char>(),
        );
        let stream = (api.stream_new)(core, profile.stream_name.as_ptr(), properties);
        if stream.is_null() {
            return Err(StreamError::Unavailable);
        }
        shared.stream.store(stream, Ordering::Release);
        (api.stream_add_listener)(stream, hook, &EVENTS, data);
        let format = spa::enum_format(profile.formats, profile.size);
        let mut params = [format.as_ptr()];
        let flags = FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS | FLAG_DONT_RECONNECT;
        if (api.stream_connect)(stream, DIRECTION_INPUT, node, flags, params.as_mut_ptr(), 1) < 0 {
            return Err(StreamError::Failed);
        }
        wait(shared, profile, cancellation, started)
    }
}

/// A camera stream that failed may have failed because its node went away;
/// the registry reports the removal shortly after the stream's state
/// change. Wait briefly for it, so a disconnection is reported as one.
unsafe fn refine(shared: &Shared) -> StreamError {
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(300) {
        if shared
            .state
            .lock()
            .map(|state| state.target_removed)
            .unwrap_or(false)
        {
            return StreamError::Disconnected;
        }
        unsafe { sleep(shared) };
    }
    StreamError::Failed
}

/// Wait with the loop lock held (released while sleeping) until the frames
/// settle, fail, time out or the caller cancels.
unsafe fn wait(
    shared: &Shared,
    profile: &Profile,
    cancellation: &Cancellation,
    started: Instant,
) -> Result<(Settled, u64), StreamError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(StreamError::Cancelled);
        }
        {
            let state = shared.state.lock().map_err(|_| StreamError::Failed)?;
            if let Some(error) = state.error {
                let camera = state.target.is_some();
                drop(state);
                return Err(if camera && error == StreamError::Failed {
                    unsafe { refine(shared) }
                } else {
                    error
                });
            }
            match (state.first_at, state.last_at) {
                (Some(first), Some(last)) => {
                    let now = Instant::now();
                    let decision = profile.policy.decide(now - first, now - last);
                    if decision != Settled::Waiting {
                        return Ok((decision, (first - started).as_millis() as u64));
                    }
                }
                _ if started.elapsed() > profile.first_frame => {
                    return Err(if state.rejected > 0 {
                        StreamError::Unsupported
                    } else {
                        StreamError::Timeout
                    });
                }
                _ => {}
            }
        }
        unsafe { sleep(shared) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_outside_the_mapping_and_negative_strides_are_rejected() {
        let memory = [7u8; 64];
        assert_eq!(chunk_bytes(&memory, 0, 64).map(<[u8]>::len), Some(64));
        assert_eq!(chunk_bytes(&memory, 16, 32).map(<[u8]>::len), Some(32));
        assert!(chunk_bytes(&memory, 60, 8).is_none());
        assert!(chunk_bytes(&memory, 65, 0).is_none());
        assert!(chunk_bytes(&memory, u32::MAX, u32::MAX).is_none());
        assert_eq!(stride(0, 10, 4), Some(40));
        assert_eq!(stride(48, 10, 4), Some(48));
        assert_eq!(stride(-40, 10, 4), None);
        assert_eq!(stride(i32::MIN, 10, 4), None);
    }

    #[test]
    fn a_short_chunk_cannot_supply_a_full_frame() {
        // 4×4 BGRx needs 64 bytes; a 48-byte chunk is refused by the frame
        // layout check instead of reading past the chunk.
        let memory = [0u8; 128];
        let valid = chunk_bytes(&memory, 0, 48).unwrap();
        assert!(
            Frame::from_raw(valid, 4, 4, 16, 0, agent_core::screen::PixelFormat::Bgrx).is_err()
        );
        let valid = chunk_bytes(&memory, 64, 64).unwrap();
        assert!(Frame::from_raw(valid, 4, 4, 16, 0, agent_core::screen::PixelFormat::Bgrx).is_ok());
    }
}
