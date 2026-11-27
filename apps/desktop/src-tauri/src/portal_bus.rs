//! Shared xdg-desktop-portal plumbing on a dedicated, identity-registered
//! session-bus connection. Used by the GlobalShortcuts and ScreenCast
//! adapters. Runs on the GLib main context.
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use gtk::{
    gio,
    glib::{self, Variant, variant::ObjectPath},
    prelude::*,
};

pub(crate) const BUS: &str = "org.freedesktop.portal.Desktop";
pub(crate) const ROOT: &str = "/org/freedesktop/portal/desktop";
pub(crate) const REQUEST: &str = "org.freedesktop.portal.Request";
pub(crate) const APP_ID: &str = "io.github.gimesha-adikari.localfirstaiagent";
pub(crate) type Dict = HashMap<String, Variant>;
/// A Request's Response once received: `None` if it could not be decoded.
type Slot = Rc<RefCell<Option<Option<(u32, Dict)>>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No portal, no registered identity, a malformed reply or a timeout.
    Unavailable,
    /// The user declined or closed the portal dialog.
    Denied,
    /// The caller stopped waiting; the portal request was closed.
    Cancelled,
}

pub(crate) fn unique(prefix: &str) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    format!(
        "{prefix}{}_{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) async fn call(
    bus: &gio::DBusConnection,
    path: &str,
    interface: &str,
    method: &str,
    args: &Variant,
) -> Result<Variant, Failure> {
    bus.call_future(
        Some(BUS),
        path,
        interface,
        method,
        Some(args),
        None,
        gio::DBusCallFlags::NONE,
        5000,
    )
    .await
    .map_err(|_| Failure::Unavailable)
}

/// A call whose reply carries file descriptors (OpenPipeWireRemote).
pub(crate) async fn call_with_fds(
    bus: &gio::DBusConnection,
    path: &str,
    interface: &str,
    method: &str,
    args: &Variant,
) -> Result<(Variant, gio::UnixFDList), Failure> {
    let (reply, fds) = bus
        .call_with_unix_fd_list_future(
            Some(BUS),
            path,
            interface,
            method,
            Some(args),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::UnixFDList::NONE,
        )
        .await
        .map_err(|_| Failure::Unavailable)?;
    Ok((reply, fds))
}

pub(crate) async fn property(
    bus: &gio::DBusConnection,
    interface: &str,
    name: &str,
) -> Result<Variant, Failure> {
    call(
        bus,
        ROOT,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(interface, name).to_variant(),
    )
    .await?
    .get::<(Variant,)>()
    .map(|(value,)| value)
    .ok_or(Failure::Unavailable)
}

/// A private connection to the session bus, registered under this app's ID
/// before any other portal call, so another process identity (an IDE or a
/// terminal) can never acquire the permission.
pub(crate) async fn connect() -> Result<gio::DBusConnection, Failure> {
    let address = gio::dbus_address_get_for_bus_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|_| Failure::Unavailable)?;
    if !address.split(';').all(|part| part.starts_with("unix:")) {
        return Err(Failure::Unavailable);
    }
    let bus = gio::DBusConnection::for_address_future(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
    )
    .await
    .map_err(|_| Failure::Unavailable)?;
    bus.set_exit_on_close(false);
    call(
        &bus,
        ROOT,
        "org.freedesktop.host.portal.Registry",
        "Register",
        &(APP_ID, Dict::new()).to_variant(),
    )
    .await?;
    Ok(bus)
}

/// One portal request: subscribe to its Response before calling, wait up to
/// `timeout`, and close the request (dismissing any dialog) if the caller
/// stops waiting.
pub(crate) async fn request(
    bus: &gio::DBusConnection,
    interface: &str,
    method: &str,
    build: impl FnOnce(Dict) -> Variant,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<Dict, Failure> {
    let name = bus.unique_name().ok_or(Failure::Unavailable)?;
    let sender = name.trim_start_matches(':').replace('.', "_");
    let handle = unique("agent");
    let path = format!("{ROOT}/request/{sender}/{handle}");
    let opts = Dict::from([("handle_token".into(), handle.to_variant())]);
    // Subscribe before the method call; the response may arrive before its reply.
    let slot: Slot = Rc::new(RefCell::new(None));
    let response = Rc::clone(&slot);
    let subscription = bus.signal_subscribe(
        Some(BUS),
        Some(REQUEST),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, message| {
            response
                .borrow_mut()
                .get_or_insert(message.get::<(u32, Dict)>());
        },
    );
    let result = async {
        let returned = call(bus, ROOT, interface, method, &build(opts)).await?;
        if returned
            .get::<(ObjectPath,)>()
            .is_none_or(|(p,)| p.as_str() != path)
        {
            return Err(Failure::Unavailable);
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(response) = slot.borrow_mut().take() {
                return match response {
                    Some((0, result)) => Ok(result),
                    Some((1, _)) => Err(Failure::Denied),
                    _ => Err(Failure::Unavailable),
                };
            }
            let stop = if cancelled() {
                Some(Failure::Cancelled)
            } else if Instant::now() >= deadline {
                Some(Failure::Unavailable)
            } else {
                None
            };
            if let Some(failure) = stop {
                let _ = call(bus, &path, REQUEST, "Close", &().to_variant()).await;
                return Err(failure);
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
    }
    .await;
    bus.signal_unsubscribe(subscription);
    result
}
