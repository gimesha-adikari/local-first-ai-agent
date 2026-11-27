//! PDF text-layer extraction in a bounded Linux subprocess. The authorized
//! file's bytes are the only document input. No document path reaches Poppler.

use std::{os::unix::process::ExitStatusExt, process::ExitStatus};

use agent_core::agent::{
    AgentError, AgentErrorCode, AgentResult, MAX_PDF_PAGES, MAX_PDF_RANGE_PAGES,
    MAX_PDF_TEXT_BYTES, PdfPageRange, PdfPageText, ToolOutput,
};

use crate::{filesystem::escaped_len, sandbox};

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

pub(super) fn no_text() -> AgentError {
    AgentError::new(
        AgentErrorCode::PdfNoText,
        "The selected PDF pages have no extractable text layer. OCR is unavailable.",
    )
}

pub(super) fn read(
    input: &[u8],
    range: Option<PdfPageRange>,
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<ToolOutput> {
    let page_count = page_count(input, check)?;
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

    let output = extract_text(input, first_page, last_page, EXTRACTED_LIMIT, check)?;
    let pages = page_previews(&output, first_page, last_page)?;
    check()?;
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

pub(super) fn page_count(input: &[u8], check: &dyn Fn() -> AgentResult<()>) -> AgentResult<u32> {
    if !input.starts_with(b"%PDF-") {
        return Err(invalid());
    }
    let info = run_parser(
        "/usr/bin/pdfinfo",
        &["/document.pdf"],
        input,
        METADATA_LIMIT,
        check,
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
    Ok(page_count)
}

pub(super) fn extract_text(
    input: &[u8],
    first_page: u32,
    last_page: u32,
    output_limit: usize,
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<String> {
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
        output_limit,
        check,
    )?;
    String::from_utf8(output).map_err(|_| unsupported())
}

pub(super) fn page_texts(
    text: &str,
    expected_pages: u32,
) -> AgentResult<impl Iterator<Item = &str>> {
    // pdftotext appends a form feed for each page. Verify all boundaries
    // before assigning page numbers.
    let body = text.strip_suffix('\x0c').ok_or_else(unsupported)?;
    let pages = body.split('\x0c');
    if pages.clone().count() != expected_pages as usize {
        return Err(unsupported());
    }
    Ok(pages)
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
    let extracted = page_texts(text, last_page - first_page + 1)?;
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
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<Vec<u8>> {
    // Each invocation has independent address-space and CPU ceilings.
    // The caller's checkpoint bounds combined wall time and handles cancellation.
    let spec = sandbox::Spec {
        executable,
        read_only: &["/usr/share/poppler", "/usr/share/fonts", "/etc/fonts"],
        input_path: "/document.pdf",
        environment: &[("LC_ALL", "C")],
        limits: sandbox::Limits {
            address_space: 268_435_456,
            cpu_seconds: 2,
            file_size: 8_388_608,
            files: 64,
        },
    };
    let outcome = sandbox::run(
        &spec,
        arguments,
        input,
        output_limit,
        DIAGNOSTICS_LIMIT,
        check,
    )
    .map_err(|failure| match failure {
        sandbox::Failure::Unavailable => unavailable(),
        sandbox::Failure::OutputLimit => limit(),
        sandbox::Failure::Stopped(error) => error,
    })?;
    check()?;
    classify(outcome.status, &outcome.diagnostics)?;
    Ok(outcome.output)
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
