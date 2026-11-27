//! One frame from a ScreenCast PipeWire node, read into memory. libpipewire
//! is loaded at run time and only the stable C entry points are used. The
//! stream is torn down as soon as a settled frame is held. Only memory the
//! CPU can read directly (MemPtr, MemFd) is accepted; GPU (DMA-BUF) buffers
//! are rejected rather than read without synchronization.

use std::{
    ffi::{CStr, c_char, c_int, c_void},
    os::fd::{IntoRawFd, OwnedFd},
    ptr,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use agent_core::{
    inference::Cancellation,
    screen::{Frame, FrameError, SettlePolicy, Settled},
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
}

/// What the stream delivered, for the capture receipt.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamRecord {
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
}

struct Shared {
    api: &'static Api,
    thread_loop: *mut c_void,
    stream: *mut c_void,
    state: Mutex<State>,
}

unsafe extern "C" fn on_state(data: *mut c_void, old: c_int, state: c_int, _: *const c_char) {
    // SAFETY: `data` is the Shared box, alive until the stream is destroyed.
    let shared = unsafe { &*(data as *const Shared) };
    if let Ok(mut guard) = shared.state.lock() {
        if state == STREAM_STATE_ERROR
            || (state == STREAM_STATE_UNCONNECTED && old != STREAM_STATE_UNCONNECTED)
        {
            guard.error.get_or_insert(StreamError::Failed);
        }
        guard.streaming |= state == STREAM_STATE_STREAMING;
    }
    unsafe { (shared.api.loop_signal)(shared.thread_loop, false) };
}

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
        (shared.api.stream_update_params)(shared.stream, params.as_mut_ptr(), 1);
        (shared.api.loop_signal)(shared.thread_loop, false);
    }
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    let shared = unsafe { &*(data as *const Shared) };
    let buffer = unsafe { (shared.api.stream_dequeue)(shared.stream) };
    if buffer.is_null() {
        return;
    }
    // SAFETY: a dequeued pw_buffer and its spa_buffer stay valid until queued
    // again. Every length below is bounded by the buffer's own maxsize.
    let frame = unsafe { read(&*buffer, shared) };
    unsafe { (shared.api.stream_queue)(shared.stream, buffer) };
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
    let api = api()?;
    let started = Instant::now();
    // SAFETY: each pointer below is checked before use and released in
    // reverse order by `teardown`, with the loop lock held where required.
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
            stream: ptr::null_mut(),
            state: Mutex::new(State::default()),
        });
        let mut hook = Box::new(Hook([ptr::null_mut(); 6]));
        let mut result = Err(StreamError::Unavailable);
        if !core.is_null() {
            let properties = (api.properties_new)(
                c"media.type".as_ptr(),
                c"Video".as_ptr(),
                c"media.category".as_ptr(),
                c"Capture".as_ptr(),
                c"media.role".as_ptr(),
                c"Screen".as_ptr(),
                ptr::null::<c_char>(),
            );
            let stream = (api.stream_new)(core, c"agent-region-capture".as_ptr(), properties);
            if !stream.is_null() {
                shared.stream = stream;
                (api.stream_add_listener)(
                    stream,
                    &mut *hook,
                    &EVENTS,
                    &mut *shared as *mut Shared as *mut c_void,
                );
                let format = spa::enum_format();
                let mut params = [format.as_ptr()];
                let flags = FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS | FLAG_DONT_RECONNECT;
                result = if (api.stream_connect)(
                    stream,
                    DIRECTION_INPUT,
                    node,
                    flags,
                    params.as_mut_ptr(),
                    1,
                ) < 0
                {
                    Err(StreamError::Failed)
                } else {
                    wait(&shared, policy, cancellation, started)
                };
                (api.stream_disconnect)(stream);
                (api.stream_destroy)(stream);
            }
            (api.core_disconnect)(core);
        }
        (api.loop_unlock)(thread_loop);
        (api.loop_stop)(thread_loop);
        (api.context_destroy)(context);
        (api.loop_destroy)(thread_loop);
        drop(hook);
        let state = shared.state.into_inner().unwrap_or_default();
        let (settled, first_frame_ms) = result?;
        let format = state.format.ok_or(StreamError::Unsupported)?;
        let frame = state.latest.ok_or(StreamError::Timeout)?;
        Ok((
            frame,
            StreamRecord {
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
        ))
    }
}

/// Wait with the loop lock held (released while sleeping) until the frames
/// settle, fail, time out or the caller cancels.
unsafe fn wait(
    shared: &Shared,
    policy: SettlePolicy,
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
                return Err(error);
            }
            match (state.first_at, state.last_at) {
                (Some(first), Some(last)) => {
                    let now = Instant::now();
                    let decision = policy.decide(now - first, now - last);
                    if decision != Settled::Waiting {
                        return Ok((decision, (first - started).as_millis() as u64));
                    }
                }
                _ if started.elapsed() > FIRST_FRAME => {
                    return Err(if state.rejected > 0 {
                        StreamError::Unsupported
                    } else {
                        StreamError::Timeout
                    });
                }
                _ => {}
            }
        }
        let mut deadline = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            (shared.api.loop_get_time)(shared.thread_loop, &mut deadline, 25_000_000);
            (shared.api.loop_timed_wait)(shared.thread_loop, &deadline);
        }
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
