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

/// The panel host. Each summon starts a session; text, answers and work
/// belong to that session only. A window may outlive sessions only when it
/// is kept warm, and then only hidden with a freshly reloaded page.
#[derive(Default)]
pub struct Panel {
    active: Mutex<Option<Arc<Glance>>>,
    page: Mutex<Page>,
}

/// Whether the WebView's page is known to hold no earlier session.
#[derive(Default)]
struct Page {
    stale: bool,
    pending: Option<Option<String>>,
}

impl Panel {
    fn active(&self) -> Option<Arc<Glance>> {
        self.active.lock().ok()?.clone()
    }

    #[cfg(test)]
    /// Whether no panel session (and so no source text or answer) remains.
    pub fn is_closed(&self) -> bool {
        self.active
            .lock()
            .map(|item| item.is_none())
            .unwrap_or(false)
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
        .active()
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

/// Keep an empty, hidden panel loaded between uses. It has no session, so
/// every panel command fails until a summon starts one. Measured on Xvfb
/// (docs/stage-03-e-screen-intelligence.md): content in about 30 ms instead
/// of about 470 ms, for about 35–80 MB of additional PSS that does not grow
/// with use.
const WARM: bool = true;

fn warm() -> bool {
    WARM
}

fn build(app: &AppHandle) -> tauri::Result<WebviewWindow> {
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
            .on_page_load(|window, payload| {
                if payload.event() == tauri::webview::PageLoadEvent::Finished {
                    loaded(window.app_handle());
                }
            })
            .build()?;
    let handle = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } if warm() => {
            api.prevent_close();
            end(&handle);
        }
        tauri::WindowEvent::Destroyed => {
            let panel = handle.state::<Panel>();
            if let Some(glance) = panel
                .active
                .lock()
                .ok()
                .and_then(|mut active| active.take())
            {
                glance.cancel();
            }
            if let Ok(mut page) = panel.page.lock() {
                *page = Page::default();
            }
        }
        _ => {}
    });
    Ok(window)
}

/// A reloaded page finished loading: it holds no earlier session, so a
/// summon that arrived during the reload may now show it.
fn loaded(app: &AppHandle) {
    let pending = {
        let panel = app.state::<Panel>();
        let Ok(mut page) = panel.page.lock() else {
            return;
        };
        page.stale = false;
        page.pending.take()
    };
    if let Some(token) = pending
        && let Some(window) = app.get_webview_window(LABEL)
        && let Ok(gtk) = window.gtk_window()
    {
        raise(&gtk, token.as_deref());
    }
}

/// Start a session unless one is open, with its 10-minute limit.
fn begin(app: &AppHandle) {
    let panel = app.state::<Panel>();
    let Ok(mut active) = panel.active.lock() else {
        return;
    };
    if active
        .as_ref()
        .is_some_and(|glance| !glance.closed.is_cancelled())
    {
        return;
    }
    let glance = Arc::new(Glance {
        closed: Cancellation::default(),
        data: Mutex::default(),
    });
    *active = Some(Arc::clone(&glance));
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(600)).await;
        if glance.closed.is_cancelled() {
            return;
        }
        let main = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            let current = main
                .state::<Panel>()
                .active()
                .is_some_and(|item| Arc::ptr_eq(&item, &glance));
            if current {
                end(&main);
            }
        });
    });
}

/// End the session: cancel work and clear host state. A warm panel hides
/// and reloads, so no text survives in the WebView; otherwise the window
/// is destroyed. Must run on the desktop main thread.
fn end(app: &AppHandle) {
    let panel = app.state::<Panel>();
    if let Some(glance) = panel
        .active
        .lock()
        .ok()
        .and_then(|mut active| active.take())
    {
        glance.cancel();
    }
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if warm() {
        if let Ok(mut page) = panel.page.lock() {
            page.stale = true;
            page.pending = None;
        }
        let _ = window.hide();
        if window.reload().is_err() {
            let _ = window.destroy();
        }
    } else {
        let _ = window.destroy();
    }
}

/// Load an empty, hidden panel shortly after startup (warm mode only).
pub fn prewarm(app: &AppHandle) {
    if !warm() {
        return;
    }
    let app = app.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        if app.get_webview_window(LABEL).is_none() {
            let _ = build(&app);
        }
    });
}

/// Must run on the desktop main thread. An open panel keeps its session and
/// is raised with the new activation token. A warm panel whose page is still
/// reloading is shown once the fresh page has loaded. The compositor decides
/// whether a window may take focus.
pub fn present(app: &AppHandle, token: Option<&str>) -> tauri::Result<()> {
    let window = match app.get_webview_window(LABEL) {
        Some(window) => window,
        None => build(app)?,
    };
    begin(app);
    let token = token
        .filter(|token| token.len() <= 4096 && !token.contains('\0'))
        .map(str::to_owned);
    if let Ok(mut page) = app.state::<Panel>().page.lock()
        && page.stale
    {
        page.pending = Some(token);
        return Ok(());
    }
    raise(&window.gtk_window()?, token.as_deref());
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
    current(&window, &pane)?;
    end(window.app_handle());
    Ok(())
}

#[tauri::command]
pub fn glance_open_main(window: WebviewWindow, pane: State<'_, Panel>) -> Result<()> {
    let main = window.get_webview_window("main").ok_or_else(closed)?;
    glance_dismiss(window, pane)?;
    main.show().map_err(|_| closed())?;
    main.set_focus().map_err(|_| closed())?;
    Ok(())
}
