//! Screen-region capture on Linux: ScreenCast portal consent, one PipeWire
//! frame in memory, immediate session closure, then a frozen-frame selector.
//! Platform code stays here; the engine only ever receives a confirmed crop.
mod overlay;
mod pipewire;
mod portal;
mod spa;

use std::time::Instant;

use agent_core::{
    inference::Cancellation,
    screen::{Frame, Region, SettlePolicy},
};
use serde::Serialize;
use tauri::AppHandle;

pub(crate) use overlay::Outcome;
use pipewire::{StreamError, StreamRecord};

use crate::portal_bus::Failure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureError {
    /// No usable ScreenCast portal, PipeWire or identity.
    Unavailable,
    /// The user declined consent.
    Denied,
    Cancelled,
    /// Only unreadable formats or GPU buffers were offered.
    Unsupported,
    /// No frame arrived in time.
    Timeout,
    Failed,
}

impl CaptureError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Unavailable => {
                "Screen capture needs the desktop's ScreenCast portal (version 4 or later) and PipeWire."
            }
            Self::Denied => "Screen sharing was not allowed. Nothing was captured.",
            Self::Cancelled => "Screen capture was cancelled.",
            Self::Unsupported => {
                "The desktop offered only screen buffers this app cannot read safely. Nothing was captured."
            }
            Self::Timeout => "The screen did not deliver a frame in time. Nothing was captured.",
            Self::Failed => "Screen capture failed. Nothing was captured.",
        }
    }
}

/// System-derived facts for the receipt. No pixel data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureRecord {
    pub source: &'static str,
    pub portal_version: u32,
    pub persist_mode: u32,
    pub restore_token: &'static str,
    /// Whether the portal returned a token anyway (it was dropped unused).
    pub restore_token_returned: bool,
    pub cursor: &'static str,
    pub consent_ms: u64,
    pub stream: StreamRecord,
    pub session_closed_ms: u64,
    pub monitor: Option<(i32, i32, i32, i32)>,
}

pub(crate) struct Snapshot {
    pub frame: Frame,
    pub record: CaptureRecord,
}

async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    task: impl FnOnce(tokio::sync::oneshot::Sender<T>) + Send + 'static,
) -> Result<T, CaptureError> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || task(reply))
        .map_err(|_| CaptureError::Failed)?;
    answer.await.map_err(|_| CaptureError::Failed)
}

/// Ask the desktop for consent, read one settled frame and close the
/// session. The session is closed on every path before this returns.
pub(crate) async fn snapshot(
    app: &AppHandle,
    cancellation: Cancellation,
) -> Result<Snapshot, CaptureError> {
    let cancel = cancellation.clone();
    let opened = on_main(app, move |reply| {
        gtk::glib::MainContext::default().spawn_local(async move {
            let _ = reply.send(portal::open(&|| cancel.is_cancelled()).await);
        });
    })
    .await?
    .map_err(|failure| match failure {
        Failure::Denied => CaptureError::Denied,
        Failure::Cancelled => CaptureError::Cancelled,
        Failure::Unavailable => CaptureError::Unavailable,
    })?;
    let portal::Opened {
        bus,
        session,
        node,
        remote,
        monitor,
        version,
        cursor,
        consent_ms,
        restore_token_returned,
    } = opened;
    let reading = cancellation.clone();
    let frame = tauri::async_runtime::spawn_blocking(move || {
        pipewire::single_frame(remote, node, SettlePolicy::default(), &reading)
    })
    .await
    .map_err(|_| CaptureError::Failed)
    .and_then(|result| {
        result.map_err(|error| match error {
            StreamError::Unavailable => CaptureError::Unavailable,
            StreamError::Unsupported => CaptureError::Unsupported,
            StreamError::Timeout => CaptureError::Timeout,
            StreamError::Failed => CaptureError::Failed,
            StreamError::Cancelled => CaptureError::Cancelled,
        })
    });
    let closing = Instant::now();
    let _ = on_main(app, move |reply| {
        gtk::glib::MainContext::default().spawn_local(async move {
            portal::close(&bus, &session).await;
            let _ = reply.send(());
        });
    })
    .await;
    let session_closed_ms = closing.elapsed().as_millis() as u64;
    let (frame, stream) = frame?;
    Ok(Snapshot {
        frame,
        record: CaptureRecord {
            source: "org.freedesktop.portal.ScreenCast",
            portal_version: version,
            persist_mode: 0,
            restore_token: "not requested or kept",
            restore_token_returned,
            cursor,
            consent_ms,
            stream,
            session_closed_ms,
            monitor,
        },
    })
}

/// Show the frozen frame and wait for a confirmed region.
pub(crate) async fn select(
    app: &AppHandle,
    snapshot: Snapshot,
    cancellation: Cancellation,
) -> Result<(Outcome, CaptureRecord), CaptureError> {
    let Snapshot { frame, record } = snapshot;
    let monitor = record.monitor;
    let outcome = on_main(app, move |reply| {
        overlay::select(frame, monitor, cancellation, move |outcome| {
            let _ = reply.send(outcome);
        });
    })
    .await?;
    Ok((outcome, record))
}

/// A record for the desktop fixture's synthetic frame, which bypasses the
/// portal (test builds only).
#[cfg(test)]
pub(crate) fn fixture_record(width: u32, height: u32) -> CaptureRecord {
    CaptureRecord {
        source: "desktop test fixture (synthetic frame, no portal)",
        portal_version: 0,
        persist_mode: 0,
        restore_token: "not requested or kept",
        restore_token_returned: false,
        cursor: "hidden",
        consent_ms: 0,
        stream: StreamRecord {
            library: "none".into(),
            format: "RGBA".into(),
            width,
            height,
            frames: 1,
            rejected_buffers: 0,
            settled: agent_core::screen::Settled::Quiescent,
            first_frame_ms: 0,
            waited_ms: 0,
        },
        session_closed_ms: 0,
        monitor: None,
    }
}

/// The selected region in frame pixels and the frame size, for receipts.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegionRecord {
    pub region: Region,
    pub frame_width: u32,
    pub frame_height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::Duration};

    /// Opt-in: runs inside a disposable headless GNOME session with private
    /// portals and PipeWire (docs/feasibility-03-e/probe). A driver process
    /// approves the real consent dialog, drags a selection and confirms it,
    /// coordinating through stage files in `AGENT_CAPTURE_OUT`.
    #[test]
    #[ignore = "needs a disposable headless GNOME session with portals, PipeWire and the capture driver"]
    fn portal_frame_overlay_and_ocr_in_a_headless_session() {
        let out = PathBuf::from(std::env::var_os("AGENT_CAPTURE_OUT").expect("output directory"));
        let stage = |name: &str| std::fs::write(out.join(format!("stage-{name}")), b"").unwrap();
        let waited = |name: &str| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !out.join(format!("stage-{name}")).exists() {
                assert!(Instant::now() < deadline, "driver did not reach {name}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        gtk::init().expect("GTK on the headless Wayland display");
        let context = gtk::glib::MainContext::default();
        let mut report = serde_json::Map::new();

        waited("ready");
        // Denial first: the driver clicks Cancel in the real consent dialog.
        stage("consent-cancel");
        let started = Instant::now();
        let denied = context.block_on(portal::open(&|| false));
        assert!(
            matches!(denied, Err(Failure::Denied)),
            "denied: {:?}",
            denied.err()
        );
        report.insert(
            "denied_ms".into(),
            (started.elapsed().as_millis() as u64).into(),
        );
        waited("cancelled");

        // Fresh consent again (no restore token): the driver clicks Share.
        stage("consent-share");
        let opened = context.block_on(portal::open(&|| false)).expect("consent");
        assert!(!opened.restore_token_returned);
        let portal::Opened {
            bus,
            session,
            node,
            remote,
            monitor,
            version,
            consent_ms,
            ..
        } = opened;
        let cancellation = Cancellation::default();
        let (frame, stream) =
            pipewire::single_frame(remote, node, SettlePolicy::default(), &cancellation)
                .expect("frame");
        let closing = Instant::now();
        context.block_on(portal::close(&bus, &session));
        report.insert(
            "session_closed_ms".into(),
            (closing.elapsed().as_millis() as u64).into(),
        );
        report.insert("portal_version".into(), version.into());
        report.insert("consent_ms".into(), consent_ms.into());
        report.insert("stream".into(), serde_json::to_value(&stream).unwrap());
        report.insert("monitor".into(), serde_json::to_value(monitor).unwrap());
        stage("frame");

        // Cancelling during acquisition: stop before the first frame.
        stage("consent-share-2");
        let opened = context
            .block_on(portal::open(&|| false))
            .expect("second consent");
        let stop = Cancellation::default();
        stop.cancel();
        let stopped =
            pipewire::single_frame(opened.remote, opened.node, SettlePolicy::default(), &stop);
        assert!(matches!(stopped, Err(StreamError::Cancelled)));
        context.block_on(portal::close(&opened.bus, &opened.session));
        stage("cancelled-acquisition");

        // The frozen overlay, opened only after the stream closed.
        let (frame_w, frame_h) = (frame.width(), frame.height());
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot = std::rc::Rc::clone(&result);
        overlay::select(frame, monitor, Cancellation::default(), move |outcome| {
            *slot.borrow_mut() = Some(outcome);
        });
        let shown = Instant::now();
        while shown.elapsed() < Duration::from_millis(800) {
            context.iteration(false);
        }
        stage("overlay");
        let deadline = Instant::now() + Duration::from_secs(60);
        while result.borrow().is_none() {
            assert!(Instant::now() < deadline, "overlay did not finish");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        let Some(Outcome::Confirmed { region, crop }) = result.borrow_mut().take() else {
            panic!("selection was not confirmed");
        };
        assert_eq!((crop.width(), crop.height()), (region.width, region.height));
        report.insert("frame".into(), serde_json::json!([frame_w, frame_h]));
        report.insert("region".into(), serde_json::to_value(region).unwrap());
        let png = crate::panel::screen::encode_for_test(&crop);
        std::fs::write(out.join("crop.png"), &png).unwrap();
        let tesseract = agent_tools::ocr::Tesseract::discover().expect("Tesseract");
        let ocr = tesseract
            .recognize(&png, &["eng".to_owned()], &|| Ok(()))
            .expect("OCR");
        report.insert("ocr_text".into(), ocr.text.clone().into());
        report.insert("ocr_ms".into(), ocr.elapsed_ms.into());
        stage("done");
        println!("CAPTURE-REPORT {}", serde_json::Value::Object(report));
    }
}
