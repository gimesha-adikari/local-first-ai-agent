//! The ScreenCast portal consent flow. Every capture creates a new session
//! with `persist_mode = 0` and never sends or keeps a restore token, so the
//! desktop asks for consent each time and shows its own sharing indicator
//! while the stream exists.
use std::{
    os::fd::{FromRawFd, OwnedFd},
    time::{Duration, Instant},
};

use gtk::{
    gio,
    glib::{Variant, variant::ObjectPath},
    prelude::*,
};

use crate::portal_bus::{Dict, Failure, call, call_with_fds, connect, property, request, unique};

const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
const SESSION: &str = "org.freedesktop.portal.Session";
const SOURCE_MONITOR: u32 = 1;
const CURSOR_HIDDEN: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;
/// Version 4 introduced `persist_mode`; requiring it lets the request say
/// explicitly that nothing may persist.
const MINIMUM_VERSION: u32 = 4;
const SETUP: Duration = Duration::from_secs(10);
/// The user may take time to read the consent dialog.
const CONSENT: Duration = Duration::from_secs(180);
/// How long `close` waits for the portal to unexport a closed session.
const CLOSE_CONFIRMATION: Duration = Duration::from_secs(1);

/// Why consent or stream setup stopped, and at which step (for diagnosis).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenError {
    pub failure: Failure,
    pub step: &'static str,
}

trait At<T> {
    fn at(self, step: &'static str) -> Result<T, OpenError>;
}

impl<T> At<T> for Result<T, Failure> {
    fn at(self, step: &'static str) -> Result<T, OpenError> {
        self.map_err(|failure| OpenError { failure, step })
    }
}

impl<T> At<T> for Option<T> {
    fn at(self, step: &'static str) -> Result<T, OpenError> {
        self.ok_or(Failure::Unavailable).at(step)
    }
}

pub(crate) struct Opened {
    pub bus: gio::DBusConnection,
    pub session: ObjectPath,
    pub node: u32,
    pub remote: OwnedFd,
    /// The stream's monitor in compositor (logical) coordinates.
    pub monitor: Option<(i32, i32, i32, i32)>,
    pub version: u32,
    pub cursor: &'static str,
    pub consent_ms: u64,
    pub restore_token_returned: bool,
}

/// Ask for consent and open one monitor stream. On any failure after the
/// session exists, the session is closed before returning.
pub(crate) async fn open(cancelled: &dyn Fn() -> bool) -> Result<Opened, OpenError> {
    let bus = connect().await.at("connect")?;
    let version = property(&bus, SCREENCAST, "version")
        .await
        .at("version")?
        .get::<u32>()
        .at("version")?;
    let sources = property(&bus, SCREENCAST, "AvailableSourceTypes")
        .await
        .at("version")?
        .get::<u32>()
        .unwrap_or(0);
    let cursors = property(&bus, SCREENCAST, "AvailableCursorModes")
        .await
        .at("version")?
        .get::<u32>()
        .unwrap_or(0);
    if version < MINIMUM_VERSION || sources & SOURCE_MONITOR == 0 {
        return Err(Failure::Unavailable).at("version");
    }
    let (cursor_mode, cursor) = if cursors & CURSOR_HIDDEN != 0 {
        (CURSOR_HIDDEN, "hidden")
    } else if cursors & CURSOR_EMBEDDED != 0 {
        (CURSOR_EMBEDDED, "embedded")
    } else {
        return Err(Failure::Unavailable).at("version");
    };
    let created = request(
        &bus,
        SCREENCAST,
        "CreateSession",
        |mut opts| {
            opts.insert(
                "session_handle_token".into(),
                unique("capture").to_variant(),
            );
            (opts,).to_variant()
        },
        SETUP,
        cancelled,
    )
    .await
    .at("create-session")?;
    let session = created
        .get("session_handle")
        .and_then(Variant::str)
        .and_then(|path| ObjectPath::try_from(path).ok())
        .at("create-session")?;
    let started = Instant::now();
    let opened = async {
        request(
            &bus,
            SCREENCAST,
            "SelectSources",
            |mut opts| {
                opts.insert("types".into(), SOURCE_MONITOR.to_variant());
                opts.insert("multiple".into(), false.to_variant());
                opts.insert("cursor_mode".into(), cursor_mode.to_variant());
                // Fresh consent every time: nothing persists, no restore token.
                opts.insert("persist_mode".into(), 0u32.to_variant());
                (&session, opts).to_variant()
            },
            CONSENT,
            cancelled,
        )
        .await
        .at("select-sources")?;
        let result = request(
            &bus,
            SCREENCAST,
            "Start",
            |opts| (&session, "", opts).to_variant(),
            CONSENT,
            cancelled,
        )
        .await
        .at("start")?;
        let consent_ms = started.elapsed().as_millis() as u64;
        // A backend should not return one with persist_mode 0; if it does, it
        // is dropped here and never stored or reused.
        let restore_token_returned = result.contains_key("restore_token");
        let streams = result
            .get("streams")
            .and_then(|streams| streams.get::<Vec<(u32, Dict)>>())
            .at("streams")?;
        let [(node, properties)] = streams.as_slice() else {
            return Err(Failure::Unavailable).at("streams");
        };
        let position = properties
            .get("position")
            .and_then(|v| v.get::<(i32, i32)>());
        let size = properties.get("size").and_then(|v| v.get::<(i32, i32)>());
        let (reply, fds) = call_with_fds(
            &bus,
            crate::portal_bus::ROOT,
            SCREENCAST,
            "OpenPipeWireRemote",
            &(&session, Dict::new()).to_variant(),
        )
        .await
        .at("pipewire-remote")?;
        let index = reply
            .get::<(gtk::glib::variant::Handle,)>()
            .map(|(handle,)| handle.0)
            .at("pipewire-remote")?;
        let raw = fds.get(index).ok().at("pipewire-remote")?;
        // SAFETY: g_unix_fd_list_get returns a new duplicate we now own.
        let remote = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(Opened {
            bus: bus.clone(),
            session: session.clone(),
            node: *node,
            remote,
            monitor: position.zip(size).map(|((x, y), (w, h))| (x, y, w, h)),
            version,
            cursor,
            consent_ms,
            restore_token_returned,
        })
    }
    .await;
    if opened.is_err() {
        close(&bus, &session).await;
    }
    opened
}

/// End the session (stopping the stream and the desktop's indicator), wait
/// a bounded time until the portal no longer exports it, and drop the private
/// connection, which also ends anything left on it. Returns whether the
/// session object was confirmed gone.
pub(crate) async fn close(bus: &gio::DBusConnection, session: &ObjectPath) -> bool {
    let _ = call(bus, session.as_str(), SESSION, "Close", &().to_variant()).await;
    let deadline = Instant::now() + CLOSE_CONFIRMATION;
    let confirmed = loop {
        let exported = call(
            bus,
            session.as_str(),
            "org.freedesktop.DBus.Properties",
            "Get",
            &(SESSION, "version").to_variant(),
        )
        .await
        .is_ok();
        if !exported {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        gtk::glib::timeout_future(Duration::from_millis(20)).await;
    };
    let _ = bus.close_future().await;
    confirmed
}
