//! PDF text-layer extraction in a bounded Linux subprocess. The authorized
//! file's bytes are the only document input. No document path reaches Poppler.

use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::Duration,
};

use agent_core::agent::{
    AgentError, AgentErrorCode, AgentResult, MAX_PDF_PAGES, MAX_PDF_RANGE_PAGES,
    MAX_PDF_TEXT_BYTES, PdfPageRange, PdfPageText, ToolOutput,
};

use crate::{ToolContext, filesystem::escaped_len};

const METADATA_LIMIT: usize = 16 * 1024;
const EXTRACTED_LIMIT: usize = 64 * 1024;
const DIAGNOSTICS_LIMIT: usize = 8 * 1024;
// Leave room below MAX_OBSERVATION_BYTES for page and observation metadata.
const ESCAPED_TEXT_LIMIT: usize = 7 * 1024;

pub(super) fn limit() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfLimit,
        "The PDF exceeds the supported size, page, text, or parser resource limit.",
    )
}

fn unavailable() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfUnavailable,
        "Local PDF extraction requires Poppler, Bubblewrap, and Linux namespace isolation.",
    )
}

fn invalid() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfInvalid,
        "The PDF is invalid or corrupted.",
    )
}

fn encrypted() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfEncrypted,
        "Encrypted PDFs are unsupported. Select an authorized, unencrypted copy.",
    )
}

fn unsupported() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfUnsupported,
        "The PDF uses a document feature or text encoding this extractor cannot read.",
    )
}

fn no_text() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfNoText,
        "The selected PDF pages have no extractable text layer. OCR is unavailable.",
    )
}

pub(super) fn read(
    input: &[u8],
    range: Option<PdfPageRange>,
    context: &ToolContext<'_>,
) -> AgentResult<ToolOutput> {
    if !input.starts_with(b"%PDF-") {
        return Err(invalid());
    }
    let info = run_parser(
        "/usr/bin/pdfinfo",
        &["/document.pdf"],
        input,
        METADATA_LIMIT,
        context,
    )?;
    let info = std::str::from_utf8(&info).map_err(|_| unsupported())?;
    if field(info, "Encrypted:")? != "no" {
        return Err(encrypted());
    }
    if field(info, "Form:")? == "XFA" {
        return Err(unsupported());
    }
    let page_count = field(info, "Pages:")?
        .parse::<u32>()
        .map_err(|_| invalid())?;
    if page_count == 0 {
        return Err(invalid());
    }
    if page_count > MAX_PDF_PAGES {
        return Err(limit());
    }
    let PdfPageRange {
        first_page,
        last_page,
    } = range.unwrap_or(PdfPageRange {
        first_page: 1,
        last_page: page_count.min(MAX_PDF_RANGE_PAGES),
    });
    if last_page > page_count {
        return Err(AgentError::new(
            AgentErrorCode::InvalidArguments,
            "The requested page range extends beyond the end of the PDF.",
        ));
    }

    let output = run_parser(
        "/usr/bin/pdftotext",
        &[
            "-f",
            &first_page.to_string(),
            "-l",
            &last_page.to_string(),
            "-enc",
            "UTF-8",
            "-eol",
            "unix",
            "/document.pdf",
            "-",
        ],
        input,
        EXTRACTED_LIMIT,
        context,
    )?;
    let text = std::str::from_utf8(&output).map_err(|_| unsupported())?;
    let pages = page_previews(text, first_page, last_page)?;
    context.check()?;
    let truncated =
        first_page != 1 || last_page != page_count || pages.iter().any(|page| page.truncated);
    Ok(ToolOutput::PdfRead {
        pages,
        page_count,
        first_page,
        last_page,
        file_bytes: input.len() as u64,
        truncated,
    })
}

fn field<'a>(info: &'a str, label: &str) -> AgentResult<&'a str> {
    let mut matches = info.lines().filter_map(|line| line.strip_prefix(label));
    let value = matches.next().ok_or_else(invalid)?.trim();
    // A malicious metadata field must not shadow a real summary field.
    if matches.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}

fn page_previews(text: &str, first_page: u32, last_page: u32) -> AgentResult<Vec<PdfPageText>> {
    // A trailing form feed is produced by pdftotext for every processed page.
    // Require exact boundary count before attributing text to page numbers.
    let body = text.strip_suffix('\x0c').ok_or_else(unsupported)?;
    let extracted = body.split('\x0c');
    if extracted.clone().count() != (last_page - first_page + 1) as usize {
        return Err(unsupported());
    }
    let mut bytes = 0;
    let mut escaped_bytes = 0;
    let mut pages = Vec::new();
    for (page_number, raw) in (first_page..=last_page).zip(extracted) {
        let mut text = String::new();
        let mut truncated = false;
        for ch in raw
            .trim()
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\t' | '\n'))
        {
            let next_bytes = bytes + ch.len_utf8();
            let next_escaped = escaped_bytes + escaped_len(ch);
            if next_bytes > MAX_PDF_TEXT_BYTES || next_escaped > ESCAPED_TEXT_LIMIT {
                truncated = true;
                break;
            }
            text.push(ch);
            bytes = next_bytes;
            escaped_bytes = next_escaped;
        }
        pages.push(PdfPageText {
            page_number,
            text,
            truncated,
        });
    }
    if pages.iter().all(|page| page.text.trim().is_empty()) {
        return Err(no_text());
    }
    Ok(pages)
}

fn run_parser(
    executable: &'static str,
    arguments: &[&str],
    input: &[u8],
    output_limit: usize,
    context: &ToolContext<'_>,
) -> AgentResult<Vec<u8>> {
    context.check()?;
    let filter = parser_filter()?;
    let filter_fd = filter.as_raw_fd();
    let filter_argument = filter_fd.to_string();
    // Each invocation has independent address-space and CPU ceilings.
    // The authorization owner's six-second ToolContext bounds combined wall time.
    let mut command = Command::new("/usr/bin/prlimit");
    command
        .args([
            "--as=268435456:268435456",
            "--cpu=2:2",
            "--fsize=8388608:8388608",
            "--nofile=64:64",
            "--core=0:0",
        ])
        .args([
            "--",
            "/usr/bin/bwrap",
            "--unshare-user",
            "--unshare-all",
            "--disable-userns",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--setenv",
            "LC_ALL",
            "C",
            "--ro-bind",
            "/usr/lib",
            "/usr/lib",
            "--ro-bind-try",
            "/usr/lib64",
            "/usr/lib64",
            "--ro-bind",
            executable,
            executable,
            "--ro-bind-try",
            "/usr/share/poppler",
            "/usr/share/poppler",
            "--ro-bind-try",
            "/usr/share/fonts",
            "/usr/share/fonts",
            "--ro-bind-try",
            "/etc/fonts",
            "/etc/fonts",
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
            "/document.pdf",
            "--seccomp",
            &filter_argument,
            "--chdir",
            "/",
            "--",
            executable,
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
    let process = command.spawn().map_err(|_| unavailable())?;
    let mut process = ParserChild(process);
    let writer = process.0.stdin.take().ok_or_else(unavailable)?;
    let mut stdout = process.0.stdout.take().ok_or_else(unavailable)?;
    let mut stderr = process.0.stderr.take().ok_or_else(unavailable)?;
    nonblocking(&writer)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut writer = Some(writer);
    let mut consumed = 0;
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();

    let status = loop {
        context.check()?;
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
                Err(_) => return Err(unavailable()),
            }
        }
        let stdout_done = drain(&mut stdout, &mut output, output_limit)?;
        let stderr_done = drain(&mut stderr, &mut diagnostics, DIAGNOSTICS_LIMIT)?;
        if let Some(status) = process.0.try_wait().map_err(|_| unavailable())?
            && stdout_done
            && stderr_done
        {
            break status;
        }
        thread::sleep(Duration::from_millis(2));
    };
    context.check()?;
    classify(status, &diagnostics)?;
    Ok(output)
}

/// Apply after Bubblewrap has set up namespaces. Poppler does not need to
/// fork or create sockets. Keeping the parser single-process makes its
/// address-space and CPU limits apply to the whole untrusted workload.
fn parser_filter() -> AgentResult<File> {
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
        return Err(unavailable());
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
    let fd = unsafe { libc::memfd_create(c"agent-pdf-seccomp".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(unavailable());
    }
    let mut filter = unsafe { File::from_raw_fd(fd) };
    filter.write_all(&code).map_err(|_| unavailable())?;
    filter.rewind().map_err(|_| unavailable())?;
    Ok(filter)
}

struct ParserChild(Child);

impl Drop for ParserChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // Bubblewrap's private PID namespace and die-with-parent kill the
            // parser as well when this supervisor is stopped.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> AgentResult<()> {
    // SAFETY: the borrowed pipe owns a valid descriptor for both calls.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(unavailable());
    }
    Ok(())
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>, maximum: usize) -> AgentResult<bool> {
    let mut buffer = [0u8; 4096];
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(n) if bytes.len() + n <= maximum => {
            bytes.extend_from_slice(&buffer[..n]);
            Ok(false)
        }
        Ok(_) => Err(limit()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(_) => Err(unavailable()),
    }
}

fn classify(status: ExitStatus, diagnostics: &[u8]) -> AgentResult<()> {
    let diagnostics = String::from_utf8_lossy(diagnostics).to_ascii_lowercase();
    if diagnostics.contains("incorrect password") {
        return Err(encrypted());
    }
    if diagnostics.starts_with("bwrap:")
        || diagnostics.starts_with("prlimit:")
        || diagnostics.contains("error while loading shared libraries")
    {
        return Err(unavailable());
    }
    if matches!(status.signal(), Some(libc::SIGXCPU | libc::SIGKILL))
        || matches!(status.code(), Some(137 | 152))
        || diagnostics.contains("bad_alloc")
        || diagnostics.contains("out of memory")
    {
        return Err(limit());
    }
    if diagnostics.contains("unsupported") || diagnostics.contains("not supported") {
        return Err(unsupported());
    }
    if !status.success() || diagnostics.contains("syntax error:") {
        return Err(invalid());
    }
    Ok(())
}
