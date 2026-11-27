//! Native host for the ephemeral contextual panel.

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use agent_core::inference::{Cancellation, ErrorCode, InferenceError, Result};
use engine::chat::{
    ChatEvent, ChatEventSink,
    glance::{Answer, Input, MAX_TEXT_BYTES, Origin, PanelState},
};
use gtk::prelude::*;
use tauri::{
    AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, ipc::Channel,
};

use crate::commands::Desktop;

const LABEL: &str = "panel";

#[derive(Default)]
pub struct Panel(Mutex<Option<Arc<Glance>>>);

#[cfg(test)]
impl Panel {
    /// Whether no panel session (and so no source text or answer) remains.
    pub fn is_closed(&self) -> bool {
        self.0.lock().map(|item| item.is_none()).unwrap_or(false)
    }
}

struct Glance {
    closed: Cancellation,
    data: Mutex<Contents>,
}

#[derive(Default)]
struct Contents {
    clipboard: Option<String>,
    reading: bool,
    operation: Option<Cancellation>,
    answer: Option<Answer>,
    remembered: bool,
}

impl Glance {
    fn data(&self) -> Result<MutexGuard<'_, Contents>> {
        self.data.lock().map_err(|_| closed())
    }

    fn cancel(&self) {
        self.closed.cancel();
        if let Ok(mut data) = self.data.lock() {
            if let Some(operation) = data.operation.take() {
                operation.cancel();
            }
            *data = Contents::default();
        }
    }
}

fn closed() -> InferenceError {
    InferenceError::new(ErrorCode::Cancelled, "The contextual panel is closed.")
}

fn busy() -> InferenceError {
    InferenceError::new(
        ErrorCode::Busy,
        "Wait for the contextual operation to finish.",
    )
}

fn current(window: &WebviewWindow, panel: &Panel) -> Result<Arc<Glance>> {
    if window.label() != LABEL {
        return Err(closed());
    }
    panel
        .0
        .lock()
        .map_err(|_| closed())?
        .clone()
        .filter(|glance| !glance.closed.is_cancelled())
        .ok_or_else(closed)
}

fn raise(gtk: &gtk::ApplicationWindow, token: Option<&str>) {
    if let Some(token) = token.filter(|token| token.len() <= 4096 && !token.contains('\0')) {
        gtk.set_startup_id(token);
    }
    // The window is built hidden so the token is set before it maps;
    // `present` alone would map it without its WebView child.
    gtk.show_all();
    gtk.present();
}

/// Must run on the desktop main thread. An open panel keeps its current
/// session and is raised with the new activation token. The compositor
/// decides whether a window may take focus.
pub fn present(app: &AppHandle, token: Option<&str>) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(LABEL) {
        raise(&window.gtk_window()?, token);
        return Ok(());
    }
    let window =
        WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?glance=1".into()))
            .title("Contextual intelligence")
            .inner_size(520.0, 660.0)
            .min_inner_size(360.0, 400.0)
            .center()
            // Not incognito: on WebKitGTK, wry drops the ephemeral context that
            // owns the `tauri://` handlers, leaving the panel on about:blank.
            // The panel keeps nothing in web storage; text and answers live in
            // React state and this host's memory only.
            .visible(false)
            .skip_taskbar(true)
            .on_navigation(|url| {
                let packaged = url.scheme() == "tauri" && url.host_str() == Some("localhost");
                let development = cfg!(debug_assertions)
                    && url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1")
                    && url.port() == Some(1420);
                (packaged || development)
                    && matches!(url.path(), "/" | "/index.html")
                    && url.query() == Some("glance=1")
            })
            .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
            .build()?;
    let glance = Arc::new(Glance {
        closed: Cancellation::default(),
        data: Mutex::default(),
    });
    let panel = app.state::<Panel>();
    *panel.0.lock().expect("panel lifecycle") = Some(Arc::clone(&glance));
    let closing = Arc::clone(&glance);
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            closing.cancel();
            if let Ok(mut active) = handle.state::<Panel>().0.lock()
                && active
                    .as_ref()
                    .is_some_and(|item| Arc::ptr_eq(item, &closing))
            {
                active.take();
            }
        }
    });
    raise(&window.gtk_window()?, token);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(600)).await;
        if !glance.closed.is_cancelled() {
            let main = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                if !glance.closed.is_cancelled()
                    && let Some(window) = main.get_webview_window(LABEL)
                {
                    glance.cancel();
                    let _ = window.destroy();
                }
            });
        }
    });
    Ok(())
}

#[tauri::command]
pub fn glance_show(app: AppHandle) -> Result<()> {
    present(&app, None).map_err(|_| {
        InferenceError::new(
            ErrorCode::InvalidConfig,
            "The contextual panel could not open.",
        )
    })
}

#[tauri::command]
pub async fn glance_state(
    window: WebviewWindow,
    pane: State<'_, Panel>,
    desktop: State<'_, Desktop>,
) -> Result<PanelState> {
    let glance = current(&window, &pane)?;
    let state = desktop.engine.glance_state().await?;
    if glance.closed.is_cancelled() {
        return Err(closed());
    }
    Ok(state)
}

#[tauri::command]
pub async fn glance_clipboard(
    window: WebviewWindow,
    pane: State<'_, Panel>,
    desktop: State<'_, Desktop>,
) -> Result<String> {
    let glance = current(&window, &pane)?;
    {
        let mut data = glance.data()?;
        if data.reading || data.operation.is_some() {
            return Err(busy());
        }
        desktop
            .engine
            .authorize_glance_clipboard(glance.closed.clone())?;
        data.reading = true;
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    let native_glance = Arc::clone(&glance);
    let scheduled = window.run_on_main_thread(move || {
        if native_glance.closed.is_cancelled() {
            return;
        }
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_text(move |_, text| {
            let text = text.filter(|text| text.len() <= MAX_TEXT_BYTES && !text.contains('\0'));
            let _ = send.send(text.map(str::to_owned));
        });
    });
    let result = if scheduled.is_ok() {
        tokio::time::timeout(Duration::from_secs(5), receive)
            .await
            .ok()
            .and_then(|item| item.ok())
            .flatten()
    } else {
        None
    };
    let mut data = glance.data()?;
    data.reading = false;
    if glance.closed.is_cancelled() {
        return Err(closed());
    }
    let text = result
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::InvalidConfig,
                "No clipboard text was available within the panel’s 8192-byte limit.",
            )
        })?;
    data.clipboard = Some(text.clone());
    Ok(text)
}

struct Deltas {
    owner: Arc<Glance>,
    cancelled: Cancellation,
    channel: Channel<String>,
}

impl ChatEventSink for Deltas {
    fn send(&self, event: ChatEvent) {
        if !self.owner.closed.is_cancelled()
            && !self.cancelled.is_cancelled()
            && let ChatEvent::Delta { text, .. } = event
        {
            let _ = self.channel.send(text);
        }
    }
}

#[tauri::command]
pub async fn glance_run(
    window: WebviewWindow,
    pane: State<'_, Panel>,
    desktop: State<'_, Desktop>,
    input: Input,
    deltas: Channel<String>,
) -> Result<Answer> {
    let glimpse = current(&window, &pane)?;
    let cancellation = Cancellation::default();
    let origin = {
        let mut data = glimpse.data()?;
        if glimpse.closed.is_cancelled() {
            return Err(closed());
        }
        if data.reading || data.operation.is_some() {
            return Err(busy());
        }
        data.answer = None;
        data.remembered = false;
        data.operation = Some(cancellation.clone());
        match &data.clipboard {
            Some(captured) if captured == &input.text => Origin::Clipboard,
            Some(_) => Origin::EditedClipboard,
            None => Origin::Manual,
        }
    };
    let events = Arc::new(Deltas {
        owner: Arc::clone(&glimpse),
        cancelled: cancellation.clone(),
        channel: deltas,
    });
    let result = desktop
        .engine
        .run_glance(input, origin, cancellation, events)
        .await;
    let mut data = glimpse.data()?;
    data.operation = None;
    if glimpse.closed.is_cancelled() {
        return Err(closed());
    }
    if let Ok(answer) = &result {
        data.answer = Some(answer.clone());
    }
    result
}

#[tauri::command]
pub fn glance_stop(window: WebviewWindow, pane: State<'_, Panel>) -> Result<()> {
    if let Some(operation) = &current(&window, &pane)?.data()?.operation {
        operation.cancel();
    }
    Ok(())
}

#[tauri::command]
pub fn glance_copy(window: WebviewWindow, pane: State<'_, Panel>) -> Result<()> {
    let glance = current(&window, &pane)?;
    let text = glance
        .data()?
        .answer
        .as_ref()
        .map(|answer| answer.text.clone())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            InferenceError::new(ErrorCode::InvalidConfig, "There is no response to copy.")
        })?;
    window
        .run_on_main_thread(move || {
            if !glance.closed.is_cancelled() {
                gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text);
            }
        })
        .map_err(|_| closed())
}

#[tauri::command]
pub fn glance_remember(
    window: WebviewWindow,
    pane: State<'_, Panel>,
    desktop: State<'_, Desktop>,
) -> Result<()> {
    let glance = current(&window, &pane)?;
    let mut data = glance.data()?;
    if data.remembered {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfig,
            "This response is already saved to memory.",
        ));
    }
    let answer = data.answer.as_ref().ok_or_else(|| {
        InferenceError::new(
            ErrorCode::InvalidConfig,
            "There is no response to remember.",
        )
    })?;
    desktop.engine.remember_glance(answer)?;
    data.remembered = true;
    Ok(())
}

#[tauri::command]
pub fn glance_dismiss(window: WebviewWindow, pane: State<'_, Panel>) -> Result<()> {
    current(&window, &pane)?.cancel();
    window.destroy().map_err(|_| closed())
}

#[tauri::command]
pub fn glance_open_main(window: WebviewWindow, pane: State<'_, Panel>) -> Result<()> {
    let main = window.get_webview_window("main").ok_or_else(closed)?;
    glance_dismiss(window, pane)?;
    main.show().map_err(|_| closed())?;
    main.set_focus().map_err(|_| closed())?;
    Ok(())
}
