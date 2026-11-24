//! Linux read-only scoped resources. Never resolve model-supplied absolute
//! paths or fall back to canonicalization after openat2 failure.

use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    io,
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::{FileExt, MetadataExt}},
    },
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_core::agent::{
    AgentError, AgentErrorCode, AgentResult, FileReadArgs, FileSearchArgs, PermissionEngine,
    ScopeKind, ScopeState, ScopeSummary, ToolOutput,
};

use crate::ToolContext;

const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_PREVIEW_BYTES: usize = 4096;
const MAX_WIRE_DATA: usize = 8192 - 256;
const MAX_ENTRIES: usize = 10_000;
const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const NO_LINKS: u64 = RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS;
const DESCENDANT: u64 = NO_LINKS | RESOLVE_BENEATH | RESOLVE_NO_XDEV;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

struct Resource {
    kind: ScopeKind,
    handle: File,
}

/// Native handles have session lifetime and never acquire replacement paths.
/// Lock order: host policy lock, then resource map. Dispatch only takes the
/// resource map briefly and never calls back into the policy engine.
#[derive(Default)]
pub struct FilesystemScopes {
    resources: Mutex<BTreeMap<String, Arc<Resource>>>,
}

impl FilesystemScopes {
    /// Trusted host only: call after the user selects a resource via a native
    /// picker. Do not accept this path through a model-controlled IPC command.
    /// Return only the opaque ID, never a path or raw descriptor.
    pub fn grant_selected(
        &self,
        policy: &mut PermissionEngine,
        path: &Path,
        kind: ScopeKind,
        lifetime: Duration,
    ) -> AgentResult<ScopeSummary> {
        if !path.is_absolute() || path == Path::new("/") {
            return Err(denied());
        }
        let handle = open_at(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_PATH | libc::O_CLOEXEC,
            NO_LINKS,
        ).map_err(open_error)?;
        check_filesystem(&handle)?;
        let metadata = handle.metadata().map_err(|_| unavailable())?;
        match kind {
            ScopeKind::File if metadata.is_file() && metadata.nlink() == 1 => {}
            ScopeKind::Directory if metadata.is_dir() => {}
            _ => return Err(denied()),
        }
        let mut resources = self.resources.lock().map_err(|_| unavailable())?;
        let summary = policy.grant_scope(kind, lifetime)?;
        if resources.contains_key(&summary.id) {
            policy.revoke_scope(&summary.id);
            return Err(denied());
        }
        resources.insert(summary.id.clone(), Arc::new(Resource { kind, handle }));
        Ok(summary)
    }

    /// Invalidate the policy lease first, then drop the registry's handle.
    /// An in-flight reader may still own an Arc; it checks lease liveness.
    pub fn revoke(&self, policy: &mut PermissionEngine, id: &str) -> AgentResult<bool> {
        let revoked = policy.revoke_scope(id);
        self.resources.lock().map_err(|_| unavailable())?.remove(id);
        Ok(revoked)
    }

    /// Call on scope listing/task cleanup to release expired/revoked handles.
    /// Policy records retain old IDs to prevent rebinding.
    pub fn prune(&self, policy: &PermissionEngine) -> AgentResult<()> {
        let active = policy.scopes().into_iter()
            .filter(|scope| scope.state == ScopeState::Active)
            .map(|scope| scope.id)
            .collect::<std::collections::BTreeSet<_>>();
        self.resources.lock().map_err(|_| unavailable())?
            .retain(|id, _| active.contains(id));
        Ok(())
    }

    fn resource(&self, id: &str) -> AgentResult<Arc<Resource>> {
        self.resources.lock().map_err(|_| unavailable())?
            .get(id).cloned().ok_or_else(denied)
    }

    pub(crate) fn search(
        &self, args: &FileSearchArgs, context: &ToolContext<'_>,
    ) -> AgentResult<ToolOutput> {
        context.check()?;
        let root = self.resource(&args.scope_id)?;
        if root.kind != ScopeKind::Directory {
            return Err(denied());
        }
        let mut search = Search {
            root: &root.handle,
            args,
            context,
            paths: Vec::new(),
            visited: 0,
            wire_bytes: 0,
            truncated: false,
            stopped: false,
        };
        search.walk("", 0)?;
        context.check()?;
        search.paths.sort_unstable();
        Ok(ToolOutput::FileSearch { paths: search.paths, truncated: search.truncated })
    }

    pub(crate) fn read(
        &self, args: &FileReadArgs, context: &ToolContext<'_>,
    ) -> AgentResult<ToolOutput> {
        context.check()?;
        let resource = self.resource(&args.scope_id)?;
        let child;
        let pinned = match (resource.kind, args.relative_path.as_deref()) {
            (ScopeKind::File, None) => &resource.handle,
            (ScopeKind::Directory, Some(path)) if valid_relative(path) => {
                child = open_child(&resource.handle, path, libc::O_PATH)
                    .map_err(open_error)?;
                &child
            }
            _ => return Err(denied()),
        };
        let file = reopen_regular(pinned, context)?;
        read_text(&file, context)
    }
}

/// Only local conventional filesystem types are supported. Unknown, network,
/// FUSE and pseudo-filesystems fail closed. Values are Linux UAPI magic
/// numbers; the selected mount itself is trusted, not its descendants.
fn check_filesystem(file: &File) -> AgentResult<()> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fd remains owned by file; stat points to writable aligned storage.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(unavailable());
    }
    // SAFETY: successful fstatfs initialized the structure.
    let magic = unsafe { stat.assume_init() }.f_type as u32;
    match magic {
        0xEF53 | 0x58465342 | 0x9123683E | 0x01021994 | 0x794C7630
        | 0x2011BAB0 | 0x4D44 => Ok(()), // ext, XFS, Btrfs, tmpfs, overlay, exFAT, FAT
        _ => Err(AgentError::new(
            AgentErrorCode::UnsupportedPlatform,
            "The selected filesystem is not supported for secure document access.",
        )),
    }
}

fn open_at(dir: RawFd, path: &[u8], flags: i32, resolve: u64) -> io::Result<File> {
    let path = CString::new(path).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let how = OpenHow { flags: flags as u64, mode: 0, resolve };
    // SAFETY: valid NUL-terminated path and initialized open_how for the
    // duration of the syscall. The dirfd is borrowed from a live File or
    // AT_FDCWD. A successful result is a new owned file descriptor.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2, dir, path.as_ptr(), &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returns a valid descriptor owned by us.
    Ok(unsafe { File::from_raw_fd(fd as RawFd) })
}

fn open_child(root: &File, path: &str, flags: i32) -> io::Result<File> {
    open_at(root.as_raw_fd(), path.as_bytes(), flags | libc::O_CLOEXEC, DESCENDANT)
}

fn valid_relative(path: &str) -> bool {
    !path.trim().is_empty() && path.len() <= 1024
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// First pin without opening a device/FIFO, then verify type and link count,
/// then reopen that exact descriptor through trusted Linux procfs, not the
/// original filename. The process must have a genuine /proc/self/fd mount.
fn reopen_regular(pinned: &File, context: &ToolContext<'_>) -> AgentResult<File> {
    context.check()?;
    let before = pinned.metadata().map_err(|_| unavailable())?;
    if !before.is_file() || before.nlink() != 1 || before.len() > MAX_FILE_BYTES as u64 {
        return Err(denied());
    }
    let fd_dir = File::open("/proc/self/fd").map_err(|_| unavailable())?;
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fd_dir is live and stat is valid writable storage.
    if unsafe { libc::fstatfs(fd_dir.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(unavailable());
    }
    // SAFETY: successful fstatfs initialized stat.
    if unsafe { stat.assume_init() }.f_type as u32 != 0x9FA0 {
        return Err(denied());
    }
    // Open the trusted procfs magic link using its pinned directory fd;
    // this is the only intentional magic-link resolution.
    let name = CString::new(pinned.as_raw_fd().to_string()).map_err(|_| unavailable())?;
    context.check()?;
    // SAFETY: dirfd and CString are live. No creation flags are supplied.
    let fd = unsafe {
        libc::openat(fd_dir.as_raw_fd(), name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK)
    };
    if fd < 0 {
        return Err(unavailable());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let after = file.metadata().map_err(|_| unavailable())?;
    if !after.is_file() || after.nlink() != 1
        || before.dev() != after.dev() || before.ino() != after.ino()
        || after.len() > MAX_FILE_BYTES as u64
    {
        return Err(denied());
    }
    context.check()?;
    Ok(file)
}

fn read_text(file: &File, context: &ToolContext<'_>) -> AgentResult<ToolOutput> {
    let before = file.metadata().map_err(|_| unavailable())?;
    if !before.is_file() || before.nlink() != 1 || before.len() > MAX_FILE_BYTES as u64 {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        context.check()?;
        let limit = buffer.len().min(MAX_FILE_BYTES + 1 - bytes.len());
        match file.read_at(&mut buffer[..limit], bytes.len() as u64) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() > MAX_FILE_BYTES {
                    return Err(denied());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(unavailable()),
        }
    }
    let after = file.metadata().map_err(|_| unavailable())?;
    if after.nlink() != 1 || before.len() != after.len() || after.len() != bytes.len() as u64
        || before.mtime() != after.mtime() || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(AgentError::new(AgentErrorCode::ToolFailed, "The selected file changed during reading."));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| unsupported_text())?;
    if text.chars().any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n')) {
        return Err(unsupported_text());
    }
    let mut end = 0;
    let mut wire = 0;
    for ch in text.chars() {
        let cost = escaped_len(ch);
        if end + ch.len_utf8() > MAX_PREVIEW_BYTES || wire + cost > MAX_WIRE_DATA {
            break;
        }
        end += ch.len_utf8();
        wire += cost;
    }
    context.check()?;
    Ok(ToolOutput::FileRead {
        content: text[..end].to_owned(),
        file_bytes: bytes.len() as u64,
        truncated: end < bytes.len(),
    })
}

fn escaped_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\r' | '\n' | '\t' => 2,
        ch if ch <= '\u{001f}' => 6,
        ch => ch.len_utf8(),
    }
}

struct Search<'a, 'b> {
    root: &'a File,
    args: &'a FileSearchArgs,
    context: &'a ToolContext<'b>,
    paths: Vec<String>,
    visited: usize,
    wire_bytes: usize,
    truncated: bool,
    stopped: bool,
}

impl Search<'_, '_> {
    fn walk(&mut self, prefix: &str, depth: u32) -> AgentResult<()> {
        self.context.check()?;
        let path = if prefix.is_empty() { "." } else { prefix };
        let dir = match open_child(self.root, path, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NONBLOCK) {
            Ok(dir) => dir,
            Err(error) if !prefix.is_empty() && skippable(&error) => {
                self.truncated = true;
                return Ok(());
            }
            Err(error) => return Err(open_error(error)),
        };
        let mut buffer = [0u8; 8192];
        while !self.stopped {
            self.context.check()?;
            // SAFETY: dir is a live directory descriptor. buffer is writable;
            // getdents64 writes at most its length. Parse only returned bytes.
            let count = unsafe {
                libc::syscall(libc::SYS_getdents64, dir.as_raw_fd(), buffer.as_mut_ptr(), buffer.len())
            };
            if count < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(unavailable());
            }
            if count == 0 {
                break;
            }
            let count = usize::try_from(count).map_err(|_| unavailable())?;
            if count > buffer.len() {
                return Err(unavailable());
            }
            let mut pos = 0;
            while pos < count && !self.stopped {
                self.context.check()?;
                let rest = &buffer[pos..count];
                // Linux linux_dirent64: ino(8), off(8), reclen(2), type(1),
                // then a NUL-terminated byte name. Do not trust d_type.
                if rest.len() < 20 {
                    return Err(unavailable());
                }
                let len = u16::from_ne_bytes([rest[16], rest[17]]) as usize;
                if len < 20 || len > rest.len() {
                    return Err(unavailable());
                }
                let field = &rest[19..len];
                let end = field.iter().position(|byte| *byte == 0).ok_or_else(unavailable)?;
                pos += len;
                if &field[..end] == b"." || &field[..end] == b".." {
                    continue;
                }
                self.visited += 1;
                if self.visited > MAX_ENTRIES {
                    self.truncated = true;
                    self.stopped = true;
                    break;
                }
                let name = match std::str::from_utf8(&field[..end]) {
                    Ok(name) => name,
                    Err(_) => {
                        self.truncated = true;
                        continue;
                    }
                };
                let relative = if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
                if name.contains('/') || !valid_relative(&relative) {
                    self.truncated = true;
                    continue;
                }
                // Resolve every candidate again from the original authorized
                // root, never from a directory that may have been renamed out.
                let item = match open_child(self.root, &relative, libc::O_PATH) {
                    Ok(item) => item,
                    Err(error) if skippable(&error) => {
                        self.truncated = true;
                        continue;
                    }
                    Err(error) => return Err(open_error(error)),
                };
                let metadata = item.metadata().map_err(|_| unavailable())?;
                if metadata.is_dir() {
                    if depth < self.args.max_depth {
                        self.walk(&relative, depth + 1)?;
                    } else {
                        self.truncated = true;
                    }
                } else if metadata.is_file() && metadata.nlink() == 1 && self.matches(name) {
                    let cost = relative.chars().map(escaped_len).sum::<usize>() + 3;
                    if self.paths.len() >= self.args.max_results as usize || self.wire_bytes + cost > MAX_WIRE_DATA {
                        self.truncated = true;
                        self.stopped = true;
                    } else {
                        self.wire_bytes += cost;
                        self.paths.push(relative);
                    }
                }
            }
        }
        Ok(())
    }

    fn matches(&self, name: &str) -> bool {
        self.args.name_contains.as_ref().is_none_or(|filter| {
            name.to_ascii_lowercase().contains(&filter.to_ascii_lowercase())
        }) && self.args.extension.as_ref().is_none_or(|extension| {
            Path::new(name).extension().and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
        })
    }
}

fn skippable(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ELOOP | libc::EXDEV | libc::EACCES | libc::EPERM | libc::ENOTDIR | libc::EAGAIN | libc::ESTALE))
}

fn open_error(error: io::Error) -> AgentError {
    if matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) {
        AgentError::new(AgentErrorCode::UnsupportedPlatform, "Secure Linux path resolution is unavailable.")
    } else if skippable(&error) {
        denied()
    } else {
        unavailable()
    }
}

fn denied() -> AgentError {
    AgentError::new(AgentErrorCode::PermissionDenied, "The resource cannot be accessed within the authorized scope.")
}

fn unavailable() -> AgentError {
    AgentError::new(AgentErrorCode::ToolFailed, "The selected resource is unavailable.")
}

fn unsupported_text() -> AgentError {
    AgentError::new(AgentErrorCode::InvalidArguments, "The file is not supported UTF-8 text.")
}
