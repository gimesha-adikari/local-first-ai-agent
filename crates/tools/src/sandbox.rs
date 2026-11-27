//! A contained, single-process subprocess for untrusted input. Bubblewrap
//! unshares every namespace (so there is no network), the only filesystem is
//! read-only system libraries, the executable, explicitly listed data and an
//! in-memory input file. A seccomp filter denies fork, clone and sockets, and
//! `prlimit` bounds address space, CPU time, file size and descriptors. The
//! input reaches the child through a pipe; nothing is written to disk.

use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::Duration,
};

pub(crate) struct Limits {
    pub address_space: u64,
    pub cpu_seconds: u32,
    pub file_size: u64,
    pub files: u32,
}

pub(crate) struct Spec<'a> {
    pub executable: &'a str,
    /// Paths bound read-only at the same location when they exist.
    pub read_only: &'a [&'a str],
    /// Where the input bytes appear inside the sandbox.
    pub input_path: &'a str,
    pub environment: &'a [(&'a str, &'a str)],
    pub limits: Limits,
}

pub(crate) struct Outcome {
    pub status: ExitStatus,
    pub output: Vec<u8>,
    pub diagnostics: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum Failure<E> {
    /// The sandbox could not be set up or the child could not run.
    Unavailable,
    /// Output exceeded the caller's limit; the child was killed.
    OutputLimit,
    /// The caller's checkpoint stopped the run; the child was killed.
    Stopped(E),
}

pub(crate) fn run<E>(
    spec: &Spec<'_>,
    arguments: &[&str],
    input: &[u8],
    output_limit: usize,
    diagnostics_limit: usize,
    check: &dyn Fn() -> Result<(), E>,
) -> Result<Outcome, Failure<E>> {
    check().map_err(Failure::Stopped)?;
    let filter = filter().map_err(|_| Failure::Unavailable)?;
    let filter_fd = filter.as_raw_fd();
    let filter_argument = filter_fd.to_string();
    let limits = [
        format!("--as={0}:{0}", spec.limits.address_space),
        format!("--cpu={0}:{0}", spec.limits.cpu_seconds),
        format!("--fsize={0}:{0}", spec.limits.file_size),
        format!("--nofile={0}:{0}", spec.limits.files),
        "--core=0:0".to_owned(),
    ];
    let mut command = Command::new("/usr/bin/prlimit");
    command.args(&limits).args([
        "--",
        "/usr/bin/bwrap",
        "--unshare-user",
        "--unshare-all",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
    ]);
    for (name, value) in spec.environment {
        command.args(["--setenv", name, value]);
    }
    command.args([
        "--ro-bind",
        "/usr/lib",
        "/usr/lib",
        "--ro-bind-try",
        "/usr/lib64",
        "/usr/lib64",
        "--ro-bind",
        spec.executable,
        spec.executable,
    ]);
    for path in spec.read_only {
        command.args(["--ro-bind-try", path, path]);
    }
    command
        .args([
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--dev",
            "/dev",
            "--ro-bind-data",
            "0",
            spec.input_path,
            "--seccomp",
            &filter_argument,
            "--chdir",
            "/",
            "--",
            spec.executable,
        ])
        .args(arguments)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: the filter File remains live until spawn returns. In the
    // child this closure only clears CLOEXEC on that descriptor using fcntl.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(filter_fd, libc::F_SETFD, 0) < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let process = command.spawn().map_err(|_| Failure::Unavailable)?;
    let mut process = Supervised(process);
    let writer = process.0.stdin.take().ok_or(Failure::Unavailable)?;
    let mut stdout = process.0.stdout.take().ok_or(Failure::Unavailable)?;
    let mut stderr = process.0.stderr.take().ok_or(Failure::Unavailable)?;
    nonblocking(&writer)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut writer = Some(writer);
    let mut consumed = 0;
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();

    let status = loop {
        check().map_err(Failure::Stopped)?;
        if let Some(pipe) = writer.as_mut() {
            let end = input.len().min(consumed + 16 * 1024);
            match pipe.write(&input[consumed..end]) {
                Ok(n) => {
                    consumed += n;
                    if consumed == input.len() {
                        writer = None;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => writer = None,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(Failure::Unavailable),
            }
        }
        let stdout_done = drain(&mut stdout, &mut output, output_limit)?;
        let stderr_done = drain(&mut stderr, &mut diagnostics, diagnostics_limit)?;
        if let Some(status) = process.0.try_wait().map_err(|_| Failure::Unavailable)?
            && stdout_done
            && stderr_done
        {
            break status;
        }
        thread::sleep(Duration::from_millis(2));
    };
    Ok(Outcome {
        status,
        output,
        diagnostics,
    })
}

/// Applied after Bubblewrap has set up namespaces. Contained tools do not
/// need to fork or create sockets. Keeping the workload single-process
/// makes its address-space and CPU limits apply to all of it.
fn filter() -> io::Result<File> {
    #[cfg(target_arch = "x86_64")]
    let (audit_arch, denied): (u32, &[libc::c_long]) = (
        0xc000_003e,
        &[
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_fork,
            libc::SYS_vfork,
            libc::SYS_socket,
            libc::SYS_socketpair,
        ],
    );
    #[cfg(target_arch = "aarch64")]
    let (audit_arch, denied): (u32, &[libc::c_long]) = (
        0xc000_00b7,
        &[
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_socket,
            libc::SYS_socketpair,
        ],
    );
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let (audit_arch, denied): (u32, &[libc::c_long]) = (0, &[]);
    if denied.is_empty() {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }

    // Each sock_filter is {u16 code, u8 jt, u8 jf, u32 k}. seccomp_data
    // puts syscall number at byte 0 and audit architecture at byte 4.
    let mut code = Vec::new();
    let mut instruction = |op: u16, yes: u8, no: u8, value: u32| {
        code.extend(op.to_ne_bytes());
        code.extend([yes, no]);
        code.extend(value.to_ne_bytes());
    };
    instruction(0x20, 0, 0, 4); // load architecture
    instruction(0x15, 1, 0, audit_arch);
    instruction(0x06, 0, 0, libc::SECCOMP_RET_KILL_PROCESS);
    instruction(0x20, 0, 0, 0); // load syscall number
    // Exclude the x32 ABI and the reserved legacy x32 syscall range.
    instruction(0x35, 0, 1, 512);
    instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32);
    for syscall in denied {
        instruction(0x15, 0, 1, *syscall as u32);
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32);
    }
    instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW);

    // SAFETY: the name is a NUL-terminated literal. A nonnegative return
    // value is a newly owned anonymous fd and becomes File's responsibility.
    let fd = unsafe { libc::memfd_create(c"agent-sandbox-seccomp".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut filter = unsafe { File::from_raw_fd(fd) };
    filter.write_all(&code)?;
    filter.rewind()?;
    Ok(filter)
}

struct Supervised(Child);

impl Drop for Supervised {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // Bubblewrap's private PID namespace and die-with-parent kill the
            // contained process as well when this supervisor is stopped.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn nonblocking<E>(pipe: &impl AsRawFd) -> Result<(), Failure<E>> {
    // SAFETY: the borrowed pipe owns a valid descriptor for both calls.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(Failure::Unavailable);
    }
    Ok(())
}

fn drain<E>(pipe: &mut impl Read, bytes: &mut Vec<u8>, maximum: usize) -> Result<bool, Failure<E>> {
    let mut buffer = [0u8; 4096];
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(n) if bytes.len() + n <= maximum => {
            bytes.extend_from_slice(&buffer[..n]);
            Ok(false)
        }
        Ok(_) => Err(Failure::OutputLimit),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(_) => Err(Failure::Unavailable),
    }
}
