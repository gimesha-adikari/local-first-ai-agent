//! Local text recognition with Tesseract fast models, contained in the
//! shared sandbox: no network, no writable or user filesystem, bounded memory,
//! CPU and output. The image reaches Tesseract through a pipe into an
//! in-memory file inside the sandbox, so no image is written to disk.

use std::{
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::sandbox;

/// Largest accepted PNG input.
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Largest recognized text returned.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Most languages combined in one recognition.
pub const MAX_LANGUAGES: usize = 4;
const DIAGNOSTICS_LIMIT: usize = 16 * 1024;
const EXECUTABLE: &str = "/usr/bin/tesseract";
const TESSDATA: [&str; 3] = [
    "/usr/share/tesseract-ocr/5/tessdata",
    "/usr/share/tesseract-ocr/4.00/tessdata",
    "/usr/share/tessdata",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrErrorKind {
    /// Tesseract, Bubblewrap or namespace isolation is unavailable.
    Unavailable,
    /// The image could not be decoded.
    Invalid,
    /// Size, time, memory or output limit reached.
    Limit,
    /// A requested language is not installed.
    Language,
    /// The caller stopped the recognition.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrError {
    pub kind: OcrErrorKind,
    pub message: &'static str,
}

impl OcrError {
    pub fn cancelled() -> Self {
        Self {
            kind: OcrErrorKind::Cancelled,
            message: "Text recognition stopped.",
        }
    }
}

fn error(kind: OcrErrorKind) -> OcrError {
    OcrError {
        kind,
        message: match kind {
            OcrErrorKind::Unavailable => {
                "Local text recognition requires Tesseract, Bubblewrap and Linux namespace isolation."
            }
            OcrErrorKind::Invalid => "The selected region could not be read as an image.",
            OcrErrorKind::Limit => {
                "The region exceeds the text recognition size, time or memory limit."
            }
            OcrErrorKind::Language => "Choose installed recognition languages (at most four).",
            OcrErrorKind::Cancelled => "Text recognition stopped.",
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrOutput {
    pub text: String,
    pub languages: Vec<String>,
    pub engine: String,
    pub elapsed_ms: u64,
}

/// The installed Tesseract and its language data.
#[derive(Debug, Clone)]
pub struct Tesseract {
    tessdata: PathBuf,
    engine: String,
}

/// Language data names Tesseract uses: `eng`, `chi_sim`, `script/Latin`
/// style paths are not accepted.
fn valid_language(name: &str) -> bool {
    let mut parts = name.split('_');
    parts.next().is_some_and(|code| {
        (3..=4).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase())
    }) && parts
        .all(|part| (2..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_lowercase()))
        && name.len() <= 24
}

/// Sandbox settings for one recognition. Tesseract's LSTM models need more
/// memory than the PDF parser; four combined fast models fit comfortably.
pub(crate) fn limits() -> sandbox::Limits {
    sandbox::Limits {
        address_space: 1_073_741_824,
        cpu_seconds: 20,
        file_size: 1_048_576,
        files: 64,
    }
}

impl Tesseract {
    /// The system installation, if Tesseract and any language data exist.
    pub fn discover() -> Option<Self> {
        let tessdata = TESSDATA
            .iter()
            .map(Path::new)
            .find(|path| path.join("eng.traineddata").is_file())
            .or_else(|| TESSDATA.iter().map(Path::new).find(|path| path.is_dir()))?;
        Self::with_tessdata(tessdata)
    }

    /// A specific language data directory (for example an optional pack).
    pub fn with_tessdata(tessdata: &Path) -> Option<Self> {
        if !Path::new(EXECUTABLE).is_file() || !tessdata.is_dir() {
            return None;
        }
        let tessdata = tessdata.canonicalize().ok()?;
        let mut tesseract = Self {
            tessdata,
            engine: String::new(),
        };
        let version = tesseract.contained(&["--version"], b"", &|| Ok(())).ok()?;
        let text = String::from_utf8_lossy(&version.output);
        let diagnostics = String::from_utf8_lossy(&version.diagnostics);
        tesseract.engine = text
            .lines()
            .chain(diagnostics.lines())
            .find(|line| line.starts_with("tesseract "))?
            .trim()
            .chars()
            .take(40)
            .collect();
        Some(tesseract)
    }

    pub fn engine(&self) -> &str {
        &self.engine
    }

    /// Installed recognition languages, sorted. Orientation data is excluded.
    pub fn languages(&self) -> Vec<String> {
        let mut languages: Vec<String> = std::fs::read_dir(&self.tessdata)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let language = name.strip_suffix(".traineddata")?;
                (valid_language(language) && language != "osd" && language != "equ")
                    .then(|| language.to_owned())
            })
            .collect();
        languages.sort();
        languages
    }

    fn contained(
        &self,
        arguments: &[&str],
        input: &[u8],
        check: &dyn Fn() -> Result<(), OcrError>,
    ) -> Result<sandbox::Outcome, OcrError> {
        let tessdata = self
            .tessdata
            .to_str()
            .ok_or_else(|| error(OcrErrorKind::Unavailable))?;
        let spec = sandbox::Spec {
            executable: EXECUTABLE,
            read_only: &[tessdata],
            input_path: "/region.png",
            environment: &[
                ("LC_ALL", "C"),
                ("OMP_THREAD_LIMIT", "1"),
                ("TESSDATA_PREFIX", tessdata),
            ],
            limits: limits(),
        };
        sandbox::run(
            &spec,
            arguments,
            input,
            MAX_TEXT_BYTES,
            DIAGNOSTICS_LIMIT,
            check,
        )
        .map_err(|failure| match failure {
            sandbox::Failure::Unavailable => error(OcrErrorKind::Unavailable),
            sandbox::Failure::OutputLimit => error(OcrErrorKind::Limit),
            sandbox::Failure::Stopped(error) => error,
        })
    }

    /// Recognize the text in one PNG image with the given installed
    /// languages. Text is returned exactly as Tesseract produced it, apart
    /// from the trailing page separator, so scripts and directions are kept.
    pub fn recognize(
        &self,
        png: &[u8],
        languages: &[String],
        check: &dyn Fn() -> Result<(), OcrError>,
    ) -> Result<OcrOutput, OcrError> {
        if png.len() > MAX_IMAGE_BYTES {
            return Err(error(OcrErrorKind::Limit));
        }
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(error(OcrErrorKind::Invalid));
        }
        let installed = self.languages();
        if languages.is_empty()
            || languages.len() > MAX_LANGUAGES
            || languages
                .iter()
                .any(|language| !installed.contains(language))
        {
            return Err(error(OcrErrorKind::Language));
        }
        let joined = languages.join("+");
        let started = Instant::now();
        let outcome = self.contained(
            &[
                "/region.png",
                "stdout",
                "-l",
                &joined,
                "--oem",
                "1",
                "--psm",
                "3",
                "-c",
                "preserve_interword_spaces=1",
            ],
            png,
            check,
        )?;
        check()?;
        let diagnostics = String::from_utf8_lossy(&outcome.diagnostics).to_ascii_lowercase();
        if diagnostics.starts_with("bwrap:")
            || diagnostics.starts_with("prlimit:")
            || diagnostics.contains("error while loading shared libraries")
        {
            return Err(error(OcrErrorKind::Unavailable));
        }
        if exceeded(&outcome.status)
            || diagnostics.contains("bad_alloc")
            || diagnostics.contains("out of memory")
        {
            return Err(error(OcrErrorKind::Limit));
        }
        if diagnostics.contains("failed loading language") {
            return Err(error(OcrErrorKind::Language));
        }
        if !outcome.status.success() {
            return Err(error(OcrErrorKind::Invalid));
        }
        let mut text =
            String::from_utf8(outcome.output).map_err(|_| error(OcrErrorKind::Invalid))?;
        while text.ends_with(['\u{c}', '\n', ' ']) {
            text.pop();
        }
        Ok(OcrOutput {
            text,
            languages: languages.to_vec(),
            engine: self.engine.clone(),
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    }
}

/// Bubblewrap reports a signalled child as exit code 128 + signal.
fn exceeded(status: &std::process::ExitStatus) -> bool {
    matches!(status.signal(), Some(libc::SIGXCPU | libc::SIGKILL))
        || matches!(status.code(), Some(137 | 152))
}

/// Character error rate: edit distance over the reference length, with all
/// whitespace removed from both (layout differences are not counted).
pub fn character_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let a: Vec<char> = reference.chars().filter(|c| !c.is_whitespace()).collect();
    let b: Vec<char> = hypothesis.chars().filter(|c| !c.is_whitespace()).collect();
    if a.is_empty() {
        return if b.is_empty() { 0.0 } else { 1.0 };
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(x != y))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()] as f64 / a.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    /// Run Python inside the OCR sandbox limits to probe containment. The
    /// probe is a stand-in for a compromised Tesseract.
    fn probe(script: &str, cpu_seconds: Option<u32>) -> sandbox::Outcome {
        let mut limits = limits();
        if let Some(cpu) = cpu_seconds {
            limits.cpu_seconds = cpu;
        }
        let spec = sandbox::Spec {
            executable: "/usr/bin/python3",
            read_only: &["/usr/share/tesseract-ocr"],
            input_path: "/region.png",
            environment: &[("LC_ALL", "C"), ("OMP_THREAD_LIMIT", "1")],
            limits,
        };
        sandbox::run::<()>(&spec, &["-I", "-c", script], b"input", 4096, 16384, &|| {
            Ok(())
        })
        .expect("sandbox ran")
    }

    fn text(outcome: &sandbox::Outcome) -> String {
        String::from_utf8_lossy(&outcome.output).into_owned()
    }

    #[test]
    #[ignore = "uses Bubblewrap with Linux namespace isolation and the system Python"]
    fn ocr_sandbox_has_no_network_user_files_or_writable_system() {
        let outcome = probe(
            r#"
import os, socket
r = []
try:
    socket.socket(socket.AF_INET, socket.SOCK_STREAM); r.append("socket-open")
except OSError: r.append("socket-denied")
try:
    socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); r.append("unix-open")
except OSError: r.append("unix-denied")
r.append("home-" + str(os.path.exists("/home")))
r.append("etc-" + str(os.path.exists("/etc/passwd")))
r.append("run-" + str(os.path.exists("/run/user")))
try:
    open("/usr/lib/agent-ocr-probe", "w"); r.append("usr-writable")
except OSError: r.append("usr-readonly")
try:
    os.fork(); r.append("fork-allowed")
except OSError: r.append("fork-denied")
r.append("input-" + open("/region.png").read())
r.append("env-" + ",".join(sorted(os.environ)))
print(" ".join(r))
"#,
            None,
        );
        let report = text(&outcome);
        for expected in [
            "socket-denied",
            "unix-denied",
            "home-False",
            "etc-False",
            "run-False",
            "usr-readonly",
            "fork-denied",
            "input-input",
        ] {
            assert!(
                report.contains(expected),
                "{expected} missing from {report}"
            );
        }
        assert!(
            !report.contains("HOME") && !report.contains("DBUS") && !report.contains("DISPLAY")
        );
    }

    #[test]
    #[ignore = "uses Bubblewrap with Linux namespace isolation and the system Python"]
    fn ocr_sandbox_enforces_memory_cpu_and_file_size_limits() {
        let memory = probe(
            "b = bytearray(1536 * 1024 * 1024)\nprint('allocated')",
            None,
        );
        assert!(!memory.status.success() && !text(&memory).contains("allocated"));
        let started = Instant::now();
        let cpu = probe("while True: pass", Some(1));
        assert!(exceeded(&cpu.status), "{:?}", cpu.status);
        assert!(started.elapsed() < Duration::from_secs(10));
        let file = probe(
            "f = open('/scratch', 'wb')\ntry:\n    f.write(b'x' * 4 * 1024 * 1024); f.flush(); print('wrote')\nexcept OSError: print('limited')",
            None,
        );
        assert!(!text(&file).contains("wrote"), "{}", text(&file));
    }

    #[test]
    #[ignore = "uses Bubblewrap with Linux namespace isolation and the system Python"]
    fn cancelling_a_contained_process_stops_and_reaps_it() {
        let marker = format!("agent-ocr-cancel-{}", std::process::id());
        let spec = sandbox::Spec {
            executable: "/usr/bin/python3",
            read_only: &[],
            input_path: "/region.png",
            environment: &[],
            limits: limits(),
        };
        let started = Instant::now();
        let calls = Cell::new(0u32);
        let script = format!("import time\n# {marker}\ntime.sleep(60)");
        let result = sandbox::run(&spec, &["-I", "-c", &script], b"", 64, 64, &|| {
            calls.set(calls.get() + 1);
            if started.elapsed() > Duration::from_millis(400) {
                Err("stopped")
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(sandbox::Failure::Stopped("stopped"))));
        assert!(started.elapsed() < Duration::from_secs(5));
        thread_free_of(&marker);
    }

    fn thread_free_of(marker: &str) {
        std::thread::sleep(Duration::from_millis(200));
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            if let Ok(command) = std::fs::read(entry.path().join("cmdline")) {
                assert!(
                    !String::from_utf8_lossy(&command).contains(marker),
                    "a contained process survived cancellation"
                );
            }
        }
    }

    #[test]
    #[ignore = "uses the system Tesseract (English) with Bubblewrap isolation"]
    fn recognizes_synthetic_english_and_rejects_bad_input_and_languages() {
        let tesseract = Tesseract::discover().expect("Tesseract with English data");
        assert!(tesseract.engine().starts_with("tesseract "));
        assert!(tesseract.languages().contains(&"eng".to_owned()));
        let png = include_bytes!("../tests/fixtures/ocr-latin-eng-14.png");
        let english = vec!["eng".to_owned()];
        let output = tesseract.recognize(png, &english, &|| Ok(())).unwrap();
        let cer = character_error_rate(
            "Settings Privacy Notifications Help\nSave changes before closing?",
            &output.text,
        );
        // Latin threshold (docs/stage-03-e-screen-intelligence.md): 5% at 14 px.
        assert!(cer <= 0.05, "CER {cer}: {}", output.text);
        assert!(output.text.contains("Notifications"));
        let mut truncated = png.to_vec();
        truncated.truncate(200);
        assert_eq!(
            tesseract
                .recognize(&truncated, &english, &|| Ok(()))
                .unwrap_err()
                .kind,
            OcrErrorKind::Invalid
        );
        assert_eq!(
            tesseract
                .recognize(b"GIF89a", &english, &|| Ok(()))
                .unwrap_err()
                .kind,
            OcrErrorKind::Invalid
        );
        assert_eq!(
            tesseract
                .recognize(png, &["zzz".to_owned()], &|| Ok(()))
                .unwrap_err()
                .kind,
            OcrErrorKind::Language
        );
        assert_eq!(
            tesseract.recognize(png, &[], &|| Ok(())).unwrap_err().kind,
            OcrErrorKind::Language
        );
        let cancelled = tesseract.recognize(png, &english, &|| Err(OcrError::cancelled()));
        assert_eq!(cancelled.unwrap_err().kind, OcrErrorKind::Cancelled);
    }

    #[test]
    fn character_error_rate_ignores_whitespace_and_counts_edits() {
        assert_eq!(character_error_rate("a b c", "abc"), 0.0);
        assert!((character_error_rate("abcd", "abxd") - 0.25).abs() < 1e-9);
        assert!((character_error_rate("أهلا", "أهل") - 0.25).abs() < 1e-9);
        assert_eq!(character_error_rate("", ""), 0.0);
    }

    #[test]
    fn language_names_are_strict() {
        for good in ["eng", "chi_sim", "chi_tra_vert", "sin", "frk"] {
            assert!(valid_language(good), "{good}");
        }
        for bad in [
            "",
            "en",
            "ENG",
            "../eng",
            "eng.traineddata",
            "script/Latin",
            "eng+fra",
            "e_n",
        ] {
            assert!(!valid_language(bad), "{bad}");
        }
    }
}
