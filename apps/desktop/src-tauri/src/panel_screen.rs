//! Screen-region commands for the contextual panel session. The full frame
//! exists only inside `crate::capture`; the session keeps the confirmed crop
//! (encoded PNG, wiped on drop) and any recognized text until it ends.

use std::sync::Mutex;

use agent_core::{
    capability::ImageInput,
    classification::{Labels, SCREEN_IMAGE},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    screen::Frame,
};
use agent_tools::ocr::{OcrError, OcrErrorKind, Tesseract};
use engine::chat::glance::{Attachment, Estimate, Input, Origin};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State, WebviewWindow};
use zeroize::Zeroizing;

use super::{Glance, LABEL, Panel, busy, closed, current, present};
use crate::{
    capture::{self, CaptureError, CaptureRecord, Outcome, RegionRecord},
    commands::Desktop,
};

/// The confirmed crop of this session.
pub(super) struct ScreenCrop {
    pub id: String,
    pub png: Zeroizing<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub region: RegionRecord,
    pub capture: CaptureRecord,
    pub ocr: Option<OcrRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OcrRecord {
    pub engine: String,
    pub languages: Vec<String>,
    pub elapsed_ms: u64,
    pub characters: usize,
    pub sandbox: &'static str,
}

/// What the panel learns about a confirmed crop. No pixels: the preview is
/// fetched from the panel-only `glance-crop:` protocol.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CropInfo {
    id: String,
    width: u32,
    height: u32,
    bytes: usize,
    image_eligible: bool,
    region: RegionRecord,
    capture: CaptureRecord,
}

/// Screen facts attached to a result's receipt.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScreenReceipt {
    capture: CaptureRecord,
    region: RegionRecord,
    ocr: Option<OcrRecord>,
    image_attached: bool,
    full_frame: &'static str,
}

fn crop_id() -> String {
    let mut bytes = [0u8; 12];
    getrandom(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn getrandom(bytes: &mut [u8]) {
    // SAFETY: getrandom writes at most `len` bytes into the buffer.
    let filled = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    assert_eq!(filled, bytes.len() as isize, "kernel randomness");
}

fn capture_error(error: CaptureError) -> InferenceError {
    let code = match error {
        CaptureError::Denied => ErrorCode::PolicyDenied,
        CaptureError::Cancelled => ErrorCode::Cancelled,
        CaptureError::Timeout => ErrorCode::TimedOut,
        CaptureError::Unavailable | CaptureError::Unsupported => ErrorCode::UnsupportedCapability,
        CaptureError::Failed | CaptureError::Interrupted => ErrorCode::GenerationFailed,
        CaptureError::Busy => ErrorCode::Busy,
    };
    InferenceError::new(code, error.message())
}

fn encode(crop: &Frame) -> Result<Zeroizing<Vec<u8>>> {
    let failed =
        || InferenceError::new(ErrorCode::InvalidConfig, "The region could not be encoded.");
    let rgba = crop.rgba();
    let mut out = Zeroizing::new(Vec::new());
    {
        let mut encoder = png::Encoder::new(&mut *out, crop.width(), crop.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| failed())?;
        writer
            .write_image_data(rgba.as_slice())
            .map_err(|_| failed())?;
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) fn encode_for_test(crop: &Frame) -> Vec<u8> {
    encode(crop).unwrap().to_vec()
}

/// The discovered installation. A missing one is looked up again on the
/// next use, so installing Tesseract takes effect without a restart.
fn tesseract() -> Option<Tesseract> {
    static TESSERACT: Mutex<Option<Tesseract>> = Mutex::new(None);
    let mut found = TESSERACT.lock().ok()?;
    if found.is_none() {
        *found = Tesseract::discover();
    }
    found.clone()
}

/// Recommended optional language data (Blueprint G6): broad coverage with
/// measured synthetic quality within the regression thresholds. English is
/// the required base. None of these is installed automatically.
pub(super) const RECOMMENDED_LANGUAGES: [&str; 10] = [
    "spa", "fra", "deu", "por", "rus", "ara", "hin", "chi_sim", "jpn", "kor",
];

/// The Debian or Ubuntu package that provides a language's data.
fn package(language: &str) -> String {
    format!("tesseract-ocr-{}", language.replace('_', "-"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OcrState {
    available: bool,
    engine: Option<String>,
    languages: Vec<String>,
    defaults: Vec<String>,
    /// What to install when recognition is unavailable or English is missing.
    guidance: Option<String>,
    /// Packages of recommended languages that are not installed.
    missing_recommended: Vec<String>,
}

fn guidance(installed: &[String]) -> Option<String> {
    if installed.is_empty() {
        Some("Local text recognition needs Tesseract with English data. Install it with “sudo apt install tesseract-ocr tesseract-ocr-eng”, then reopen this panel.".to_owned())
    } else if !installed.iter().any(|language| language == "eng") {
        Some("English recognition data is missing. Install it with “sudo apt install tesseract-ocr-eng”, then reopen this panel.".to_owned())
    } else {
        None
    }
}

fn missing_recommended(installed: &[String]) -> Vec<String> {
    RECOMMENDED_LANGUAGES
        .iter()
        .filter(|language| !installed.iter().any(|have| have == *language))
        .map(|language| package(language))
        .collect()
}

/// Locale code to Tesseract language data name.
fn locale_language(locale: &str) -> Option<&'static str> {
    let lower = locale.to_ascii_lowercase();
    let (language, region) = lower.split_once(['_', '-']).unwrap_or((&lower, ""));
    Some(match (language, region) {
        ("zh", r) if r.starts_with("tw") || r.starts_with("hk") || r.starts_with("hant") => {
            "chi_tra"
        }
        ("zh", _) => "chi_sim",
        ("en", _) => "eng",
        ("es", _) => "spa",
        ("fr", _) => "fra",
        ("de", _) => "deu",
        ("pt", _) => "por",
        ("it", _) => "ita",
        ("ru", _) => "rus",
        ("uk", _) => "ukr",
        ("ar", _) => "ara",
        ("he", _) => "heb",
        ("hi", _) => "hin",
        ("bn", _) => "ben",
        ("ja", _) => "jpn",
        ("ko", _) => "kor",
        ("th", _) => "tha",
        ("vi", _) => "vie",
        ("tr", _) => "tur",
        ("id", _) => "ind",
        ("si", _) => "sin",
        ("ta", _) => "tam",
        _ => return None,
    })
}

pub(super) fn ocr_state() -> OcrState {
    let Some(tesseract) = tesseract() else {
        return OcrState {
            available: false,
            engine: None,
            languages: Vec::new(),
            defaults: Vec::new(),
            guidance: guidance(&[]),
            missing_recommended: missing_recommended(&[]),
        };
    };
    let languages = tesseract.languages();
    let mut defaults = Vec::new();
    for variable in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(variable) {
            for locale in value.split(':') {
                if let Some(code) = locale_language(locale.split('.').next().unwrap_or(""))
                    && languages.iter().any(|l| l == code)
                    && !defaults.iter().any(|d| d == code)
                {
                    defaults.push(code.to_owned());
                }
            }
        }
    }
    if defaults.is_empty() && languages.iter().any(|l| l == "eng") {
        defaults.push("eng".to_owned());
    }
    defaults.truncate(agent_tools::ocr::MAX_LANGUAGES);
    OcrState {
        available: !languages.is_empty(),
        engine: Some(tesseract.engine().to_owned()),
        guidance: guidance(&languages),
        missing_recommended: missing_recommended(&languages),
        languages,
        defaults,
    }
}

/// The installed recognizer and the default languages for this locale, for
/// text in attached images (Stage 04-D). Nothing is installed automatically.
pub(crate) fn recognizer() -> Option<(Tesseract, Vec<String>)> {
    let state = ocr_state();
    if !state.available || state.defaults.is_empty() {
        return None;
    }
    tesseract().map(|tesseract| (tesseract, state.defaults))
}

/// The desktop fixture's synthetic frame and region (test builds only).
#[cfg(test)]
pub(crate) static FIXTURE: std::sync::Mutex<Option<(Frame, agent_core::screen::Region)>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn fixture_crop() -> Option<(Frame, agent_core::screen::Region, CaptureRecord, u32, u32)> {
    let (frame, region) = FIXTURE.lock().ok()?.take()?;
    let crop = frame.crop(region).ok()?;
    let (width, height) = (frame.width(), frame.height());
    Some((
        crop,
        region,
        capture::fixture_record(width, height),
        width,
        height,
    ))
}

/// Explicit gesture: hide the panel, ask the desktop for consent, take one
/// frame, close the stream, then let the user select and confirm a region.
#[tauri::command]
pub async fn glance_capture(window: WebviewWindow, pane: State<'_, Panel>) -> Result<CropInfo> {
    let glance = current(&window, &pane)?;
    let cancellation = Cancellation::default();
    {
        let mut data = glance.data()?;
        if data.reading || data.operation.is_some() {
            return Err(busy());
        }
        data.operation = Some(cancellation.clone());
        data.screen = None;
    }
    let app = window.app_handle().clone();
    // The panel must not appear in the frame it is about to capture.
    let _ = window.hide();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let result = async {
        #[cfg(test)]
        if let Some((crop, region, record, frame_width, frame_height)) = fixture_crop() {
            let png = encode(&crop)?;
            return Ok(ScreenCrop {
                id: crop_id(),
                width: region.width,
                height: region.height,
                png,
                region: RegionRecord {
                    region,
                    frame_width,
                    frame_height,
                },
                capture: record,
                ocr: None,
            });
        }
        let snapshot = capture::snapshot(&app, cancellation.clone())
            .await
            .map_err(capture_error)?;
        let (frame_width, frame_height) = (snapshot.frame.width(), snapshot.frame.height());
        let (outcome, record) = capture::select(&app, snapshot, cancellation.clone())
            .await
            .map_err(capture_error)?;
        let (region, crop) = match outcome {
            Outcome::Confirmed { region, crop } => (region, crop),
            Outcome::Cancelled => return Err(capture_error(CaptureError::Cancelled)),
            Outcome::TimedOut => return Err(capture_error(CaptureError::Timeout)),
        };
        let png = tauri::async_runtime::spawn_blocking(move || encode(&crop))
            .await
            .map_err(|_| closed())??;
        Ok(ScreenCrop {
            id: crop_id(),
            width: region.width,
            height: region.height,
            png,
            region: RegionRecord {
                region,
                frame_width,
                frame_height,
            },
            capture: record,
            ocr: None,
        })
    }
    .await;
    let mut data = glance.data()?;
    data.operation = None;
    if glance.closed.is_cancelled() {
        return Err(closed());
    }
    drop(data);
    let reopen = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = present(&reopen, None);
    });
    let crop = result?;
    let info = CropInfo {
        id: crop.id.clone(),
        width: crop.width,
        height: crop.height,
        bytes: crop.png.len(),
        image_eligible: crop.png.len() <= agent_core::capability::MAX_IMAGE_BYTES,
        region: crop.region,
        capture: crop.capture.clone(),
    };
    glance.data()?.screen = Some(crop);
    Ok(info)
}

#[tauri::command]
pub fn glance_capture_clear(window: WebviewWindow, pane: State<'_, Panel>) -> Result<()> {
    let glance = current(&window, &pane)?;
    let mut data = glance.data()?;
    if data.operation.is_some() {
        return Err(busy());
    }
    data.screen = None;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrRequest {
    crop: String,
    languages: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrResult {
    text: String,
    record: OcrRecord,
}

/// Recognize the confirmed crop locally in the contained Tesseract.
#[tauri::command]
pub async fn glance_ocr(
    window: WebviewWindow,
    pane: State<'_, Panel>,
    request: OcrRequest,
) -> Result<OcrResult> {
    let glance = current(&window, &pane)?;
    let tesseract = tesseract().ok_or_else(|| {
        InferenceError::new(
            ErrorCode::UnsupportedCapability,
            "Local text recognition needs Tesseract with English data (sudo apt install tesseract-ocr tesseract-ocr-eng).",
        )
    })?;
    let cancellation = Cancellation::default();
    let png = {
        let mut data = glance.data()?;
        if data.reading || data.operation.is_some() {
            return Err(busy());
        }
        let crop = data
            .screen
            .as_ref()
            .filter(|crop| crop.id == request.crop)
            .ok_or_else(|| {
                InferenceError::new(ErrorCode::InvalidConfig, "Capture a region first.")
            })?;
        let png = Zeroizing::new(crop.png.to_vec());
        data.operation = Some(cancellation.clone());
        png
    };
    let languages = request.languages;
    let stop = cancellation.clone();
    let owner = std::sync::Arc::clone(&glance);
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        tesseract.recognize(&png, &languages, &|| {
            if stop.is_cancelled() || owner.closed.is_cancelled() {
                Err(OcrError::cancelled())
            } else {
                Ok(())
            }
        })
    })
    .await
    .map_err(|_| closed())?;
    let mut data = glance.data()?;
    data.operation = None;
    if glance.closed.is_cancelled() {
        return Err(closed());
    }
    let output = outcome.map_err(|error| {
        let code = match error.kind {
            OcrErrorKind::Cancelled => ErrorCode::Cancelled,
            OcrErrorKind::Unavailable => ErrorCode::UnsupportedCapability,
            OcrErrorKind::Limit => ErrorCode::MemoryLimit,
            OcrErrorKind::Invalid | OcrErrorKind::Language => ErrorCode::InvalidConfig,
        };
        InferenceError::new(code, error.message)
    })?;
    let record = OcrRecord {
        engine: output.engine,
        languages: output.languages,
        elapsed_ms: output.elapsed_ms,
        characters: output.text.chars().count(),
        sandbox: "bubblewrap: no network, read-only system and language data, seccomp, 1 GiB, 20 s CPU",
    };
    let crop = data
        .screen
        .as_mut()
        .filter(|crop| crop.id == request.crop)
        .ok_or_else(closed)?;
    crop.ocr = Some(record.clone());
    data.screen_text = true;
    Ok(OcrResult {
        text: output.text,
        record,
    })
}

/// How the request's text is labelled: every source this session read from
/// counts, so editing never removes a classification.
pub(super) fn origin(data: &super::Contents, text: &str) -> Origin {
    match (data.screen_text, &data.clipboard) {
        (true, Some(_)) => Origin::ScreenAndClipboard,
        (true, None) => Origin::Screen,
        (false, Some(captured)) if captured == text => Origin::Clipboard,
        (false, Some(_)) => Origin::EditedClipboard,
        (false, None) => Origin::Manual,
    }
}

/// The crop as an image attachment, only for the exact crop the user reviewed.
pub(super) fn attachment(data: &super::Contents, input: &Input) -> Result<Option<Attachment>> {
    if !input.wants_image() {
        return Ok(None);
    }
    let crop = data
        .screen
        .as_ref()
        .filter(|crop| input.crop() == Some(crop.id.as_str()))
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::PolicyDenied,
                "The screen region changed. Review the current region again.",
            )
        })?;
    let image = ImageInput::new(crop.png.to_vec()).ok_or_else(|| {
        InferenceError::new(
            ErrorCode::InvalidConfig,
            "This region is too large to send as an image. Select a smaller region or extract its text.",
        )
    })?;
    Ok(Some(Attachment {
        image,
        size: (crop.width, crop.height),
        labels: Labels::from_source(SCREEN_IMAGE),
    }))
}

pub(super) fn receipt(data: &super::Contents, image: bool) -> Option<ScreenReceipt> {
    let crop = data.screen.as_ref()?;
    (image || data.screen_text).then(|| ScreenReceipt {
        capture: crop.capture.clone(),
        region: crop.region,
        ocr: crop.ocr.clone(),
        image_attached: image,
        full_frame: "released after region confirmation; never sent or stored",
    })
}

#[tauri::command]
pub fn glance_estimate(
    window: WebviewWindow,
    pane: State<'_, Panel>,
    desktop: State<'_, Desktop>,
    input: Input,
) -> Result<Estimate> {
    let glance = current(&window, &pane)?;
    let data = glance.data()?;
    let attachment = attachment(&data, &input)?;
    desktop
        .engine
        .estimate_glance(&input, origin(&data, input.text()), attachment.as_ref())
}

/// `glance-crop://localhost/<id>.png`: the confirmed crop of the current
/// session, served only to the panel WebView, never cached.
pub fn crop_protocol(
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
    if context.webview_label() != LABEL || request.method() != tauri::http::Method::GET {
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
    let panel = context.app_handle().state::<Panel>();
    let Some(glance): Option<std::sync::Arc<Glance>> = panel
        .active()
        .filter(|glance| !glance.closed.is_cancelled())
    else {
        return refuse(404);
    };
    let Ok(data) = glance.data() else {
        return refuse(404);
    };
    match data.screen.as_ref().filter(|crop| crop.id == id) {
        Some(crop) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", "image/png")
            .header("Cache-Control", "no-store")
            .body(crop.png.to_vec())
            .unwrap_or_else(|_| refuse(500)),
        None => refuse(404),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::screen::{PixelFormat, Region};

    fn session(
        screen_text: bool,
        clipboard: Option<&str>,
        crop: Option<(u32, u32, usize)>,
    ) -> super::super::Contents {
        super::super::Contents {
            clipboard: clipboard.map(str::to_owned),
            screen_text,
            screen: crop.map(|(width, height, extra)| {
                let raw = vec![200u8; (width * height * 4) as usize];
                let frame = Frame::from_raw(
                    &raw,
                    width,
                    height,
                    width as usize * 4,
                    0,
                    PixelFormat::Rgba,
                )
                .unwrap();
                let mut png = encode(&frame).unwrap();
                png.extend(std::iter::repeat_n(0, extra));
                ScreenCrop {
                    id: "c1".into(),
                    png,
                    width,
                    height,
                    region: RegionRecord {
                        region: Region {
                            x: 0,
                            y: 0,
                            width,
                            height,
                        },
                        frame_width: width * 2,
                        frame_height: height * 2,
                    },
                    capture: capture::fixture_record(width * 2, height * 2),
                    ocr: None,
                }
            }),
            ..Default::default()
        }
    }

    fn input(image: bool, crop: Option<&str>) -> Input {
        serde_json::from_value(serde_json::json!({
            "text": "t", "recipe": "explain", "share": true, "memory": false,
            "documents": [], "image": image, "crop": crop
        }))
        .unwrap()
    }

    #[test]
    fn text_keeps_every_source_label_the_session_used() {
        assert!(matches!(
            origin(&session(false, None, None), "x"),
            Origin::Manual
        ));
        assert!(matches!(
            origin(&session(false, Some("x"), None), "x"),
            Origin::Clipboard
        ));
        assert!(matches!(
            origin(&session(false, Some("x"), None), "edited"),
            Origin::EditedClipboard
        ));
        // Editing or replacing recognized text never drops its screen labels.
        assert!(matches!(
            origin(&session(true, None, None), "anything"),
            Origin::Screen
        ));
        assert!(matches!(
            origin(&session(true, Some("x"), None), "x"),
            Origin::ScreenAndClipboard
        ));
    }

    #[test]
    fn an_image_is_attached_only_for_the_reviewed_crop_and_within_limits() {
        let data = session(false, None, Some((40, 20, 0)));
        assert!(attachment(&data, &input(false, None)).unwrap().is_none());
        let attached = attachment(&data, &input(true, Some("c1")))
            .unwrap()
            .unwrap();
        assert_eq!(attached.size, (40, 20));
        assert!(attached.labels.contains(SCREEN_IMAGE));
        for stale in [None, Some("other")] {
            assert_eq!(
                attachment(&data, &input(true, stale)).err().unwrap().code,
                ErrorCode::PolicyDenied
            );
        }
        assert_eq!(
            attachment(&session(false, None, None), &input(true, Some("c1")))
                .err()
                .unwrap()
                .code,
            ErrorCode::PolicyDenied
        );
        let oversized = session(
            false,
            None,
            Some((40, 20, agent_core::capability::MAX_IMAGE_BYTES)),
        );
        assert_eq!(
            attachment(&oversized, &input(true, Some("c1")))
                .err()
                .unwrap()
                .code,
            ErrorCode::InvalidConfig
        );
        // The receipt names the crop, never pixels, and states the frame's fate.
        let record = serde_json::to_string(&receipt(&data, true).unwrap()).unwrap();
        assert!(
            record.contains("\"imageAttached\":true") && record.contains("never sent or stored")
        );
        assert!(record.len() < 2048);
        assert!(receipt(&session(false, None, Some((8, 8, 0))), false).is_none());
    }

    #[test]
    fn locales_map_to_tesseract_language_data() {
        for (locale, code) in [
            ("en_US", "eng"),
            ("zh_TW", "chi_tra"),
            ("zh_CN", "chi_sim"),
            ("zh-Hant", "chi_tra"),
            ("si_LK", "sin"),
            ("ta", "tam"),
            ("pt_BR", "por"),
            ("ar_EG", "ara"),
        ] {
            assert_eq!(locale_language(locale), Some(code), "{locale}");
        }
        assert_eq!(locale_language("C"), None);
        assert_eq!(locale_language(""), None);
    }

    #[test]
    fn missing_language_data_gets_install_guidance_and_nothing_is_installed() {
        let have = |codes: &[&str]| codes.iter().map(|c| (*c).to_owned()).collect::<Vec<_>>();
        assert!(
            guidance(&[])
                .unwrap()
                .contains("tesseract-ocr tesseract-ocr-eng")
        );
        assert!(
            guidance(&have(&["fra"]))
                .unwrap()
                .contains("English recognition data is missing")
        );
        assert_eq!(guidance(&have(&["eng"])), None);
        assert_eq!(package("chi_sim"), "tesseract-ocr-chi-sim");
        let missing = missing_recommended(&have(&["eng", "spa", "chi_sim"]));
        assert_eq!(missing.len(), 8);
        assert!(missing.contains(&"tesseract-ocr-jpn".to_owned()));
        assert!(!missing.contains(&"tesseract-ocr-spa".to_owned()));
        assert!(!missing.contains(&"tesseract-ocr-chi-sim".to_owned()));
    }
}
