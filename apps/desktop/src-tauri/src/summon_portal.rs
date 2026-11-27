//! GlobalShortcuts over a dedicated, identity-registered local session-bus
//! connection. The portal owns shortcut choice, consent, and persistence.
use super::*;
use crate::portal_bus::{Dict, Failure, ROOT, call, connect, unique};
use glib::{Variant, variant::ObjectPath};

const BUS: &str = crate::portal_bus::BUS;
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const ID: &str = "summon";
type Bindings = Vec<(String, Dict)>;
type Reply = tokio::sync::oneshot::Sender<ShortcutStatus>;

pub(super) struct Session {
    connection: gio::DBusConnection,
    path: ObjectPath,
    subscriptions: Vec<gio::SignalSubscriptionId>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for id in self.subscriptions.drain(..) {
            self.connection.signal_unsubscribe(id);
        }
        // Closing our private connection also closes its portal sessions.
        self.connection.close(gio::Cancellable::NONE, |_| {});
    }
}

async fn request(
    bus: &gio::DBusConnection,
    method: &str,
    build: impl FnOnce(Dict) -> Variant,
) -> Result<Dict, Failure> {
    crate::portal_bus::request(
        bus,
        SHORTCUTS,
        method,
        build,
        Duration::from_secs(120),
        &|| false,
    )
    .await
}

fn trigger(bindings: &Bindings) -> Option<String> {
    bindings
        .iter()
        .find(|(id, _)| id == ID)?
        .1
        .get("trigger_description")?
        .get::<String>()
        .filter(|s| !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}

fn bindings(result: &Dict) -> Result<Bindings, Failure> {
    result
        .get("shortcuts")
        .and_then(Variant::get)
        .ok_or(Failure::Unavailable)
}

fn activation(message: &Variant, path: &ObjectPath) -> Option<Activation> {
    let (session, id, _, options) = message.get::<(ObjectPath, String, u64, Dict)>()?;
    (session == *path && id == ID).then(|| Activation {
        panel: true,
        token: options
            .get("activation_token")
            .and_then(Variant::str)
            .and_then(token),
    })
}

fn watch(app: &AppHandle, bus: gio::DBusConnection, path: ObjectPath) -> Session {
    let event_app = app.clone();
    let event_path = path.clone();
    let events = bus.signal_subscribe(
        Some(BUS),
        Some(SHORTCUTS),
        None,
        Some(ROOT),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, signal, message| match signal {
            "Activated" => {
                if let Some(request) = activation(message, &event_path) {
                    activate(&event_app, request);
                }
            }
            "ShortcutsChanged" => {
                if let Some((session, shortcuts)) = message.get::<(ObjectPath, Bindings)>()
                    && session == event_path
                {
                    let state = event_app.state::<PlatformState>();
                    let mut status = state.status.lock().unwrap();
                    status.trigger = trigger(&shortcuts);
                    status.state = if status.trigger.is_some() {
                        "active"
                    } else {
                        "available"
                    };
                }
            }
            _ => {}
        },
    );
    let session_app = app.clone();
    let closed = bus.signal_subscribe(
        Some(BUS),
        Some("org.freedesktop.portal.Session"),
        Some("Closed"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, _| lost(&session_app),
    );
    let owner_app = app.clone();
    let owner = bus.signal_subscribe(
        Some("org.freedesktop.DBus"),
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        Some(BUS),
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, msg| {
            if msg
                .get::<(String, String, String)>()
                .is_some_and(|(_, old, _)| !old.is_empty())
            {
                lost(&owner_app);
            }
        },
    );
    Session {
        connection: bus,
        path,
        subscriptions: vec![events, closed, owner],
    }
}

fn lost(app: &AppHandle) {
    let platform = app.state::<PlatformState>();
    platform.session.lock().unwrap().take();
    *platform.status.lock().unwrap() = ShortcutStatus::new("unavailable");
}

async fn setup(app: &AppHandle, interactive: bool) -> Result<ShortcutStatus, Failure> {
    let platform = app.state::<PlatformState>();
    let bus = connect().await?;
    let version = call(
        &bus,
        ROOT,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(SHORTCUTS, "version").to_variant(),
    )
    .await?
    .get::<(Variant,)>()
    .and_then(|(v,)| v.get::<u32>())
    .ok_or(Failure::Unavailable)?;
    let created = request(&bus, "CreateSession", |mut opts| {
        opts.insert("session_handle_token".into(), unique("summon").to_variant());
        (opts,).to_variant()
    })
    .await?;
    let path = created
        .get("session_handle")
        .and_then(Variant::str)
        .and_then(|s| ObjectPath::try_from(s).ok())
        .ok_or(Failure::Unavailable)?;
    if !interactive {
        let prior = request(&bus, "ListShortcuts", |opts| (&path, opts).to_variant()).await?;
        if trigger(&bindings(&prior)?).is_none() {
            let _ = call(
                &bus,
                &path,
                "org.freedesktop.portal.Session",
                "Close",
                &().to_variant(),
            )
            .await;
            return Ok(ShortcutStatus::new("available"));
        }
    }
    let shortcuts = vec![(
        ID,
        Dict::from([
            (
                "description".into(),
                "Open the contextual text panel".to_variant(),
            ),
            ("preferred_trigger".into(), "CTRL+ALT+k".to_variant()),
        ]),
    )];
    let bound = request(&bus, "BindShortcuts", |opts| {
        (&path, shortcuts, "", opts).to_variant()
    })
    .await?;
    let display = trigger(&bindings(&bound)?).ok_or(Failure::Denied)?;
    *platform.session.lock().unwrap() = Some(watch(app, bus, path));
    Ok(ShortcutStatus {
        state: "active",
        trigger: Some(display),
        can_reconfigure: version >= 2,
    })
}

pub(super) fn start(app: &AppHandle, interactive: bool, reply: Option<Reply>) {
    let platform = app.state::<PlatformState>();
    let previous = platform.status.lock().unwrap().clone();
    if previous.state == "configuring" || previous.state == "active" && !previous.can_reconfigure {
        if let Some(reply) = reply {
            let _ = reply.send(previous);
        }
        return;
    }
    let reconfigure = if previous.state == "active" {
        platform
            .session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| (s.connection.clone(), s.path.clone()))
    } else {
        None
    };
    *platform.status.lock().unwrap() = ShortcutStatus::new("configuring");
    let app = app.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = if let Some((bus, path)) = reconfigure {
            call(
                &bus,
                ROOT,
                SHORTCUTS,
                "ConfigureShortcuts",
                &(&path, "", Dict::new()).to_variant(),
            )
            .await
            .map(|_| previous.clone())
        } else {
            setup(&app, interactive).await
        };
        let status = match result {
            Ok(status) => status,
            // A declined configuration dialog does not remove an existing binding.
            Err(_) if previous.state == "active" => previous,
            Err(Failure::Denied) => ShortcutStatus::new("denied"),
            Err(Failure::Unavailable | Failure::Cancelled) => ShortcutStatus::new("unavailable"),
        };
        *app.state::<PlatformState>().status.lock().unwrap() = status.clone();
        if let Some(reply) = reply {
            let _ = reply.send(status);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
    };

    #[test]
    fn only_the_owned_shortcut_session_accepts_its_activation_token() {
        let own = ObjectPath::try_from("/session/ours").unwrap();
        let signal = |session: &ObjectPath, id: &str, value: &str| {
            (
                session,
                id,
                1u64,
                Dict::from([("activation_token".into(), value.to_variant())]),
            )
                .to_variant()
        };
        let event = signal(&own, ID, "synthetic-focus-token");
        assert_eq!(event.type_().as_str(), "(osta{sv})");
        assert_eq!(
            activation(&event, &own).unwrap().token.as_deref(),
            Some("synthetic-focus-token")
        );
        assert!(activation(&event, &ObjectPath::try_from("/session/other").unwrap()).is_none());
        assert!(activation(&signal(&own, "other", "test"), &own).is_none());
        assert!(
            activation(&signal(&own, ID, "bad\ntoken"), &own)
                .unwrap()
                .token
                .is_none()
        );
    }

    #[test]
    fn private_bus_mock_delivers_an_early_portal_response() {
        // A fresh, anonymous test bus has no desktop services, grants or user data.
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        struct Stop(Child);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _daemon = Stop(daemon);
        let context = glib::MainContext::new();
        context.with_thread_default(|| context.block_on(async {
            let connect = || gio::DBusConnection::for_address_future(address.trim(),
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None);
            let service = connect().await.unwrap();
            service.set_exit_on_close(false);
            let client = connect().await.unwrap();
            client.set_exit_on_close(false);
            service.call_future(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
                "RequestName", Some(&(BUS, 0u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000)
                .await.unwrap();
            let xml = r#"<node><interface name="org.freedesktop.portal.GlobalShortcuts">
                <method name="BindShortcuts"><arg type="o" direction="in"/><arg type="a(sa{sv})" direction="in"/>
                <arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/>
                </method></interface></node>"#;
            let iface = gio::DBusNodeInfo::for_xml(xml).unwrap().lookup_interface(SHORTCUTS).unwrap();
            let reg = service.register_object(ROOT, &iface, |bus, sender, _, _, _, args, invocation| {
                let (session, shortcuts, parent, opts) = args.get::<(ObjectPath, Bindings, String, Dict)>().unwrap();
                assert_eq!(session.as_str(), "/session/synthetic");
                assert_eq!(shortcuts[0].0, ID);
                assert!(parent.is_empty());
                let handle = opts["handle_token"].str().unwrap();
                let sender_id = sender.trim_start_matches(':').replace('.', "_");
                let path = ObjectPath::try_from(format!("{ROOT}/request/{sender_id}/{handle}")).unwrap();
                let bindings = vec![(ID, Dict::from([("trigger_description".into(), "Ctrl+Alt+K".to_variant())]))];
                let result = Dict::from([("shortcuts".into(), bindings.to_variant())]);
                bus.emit_signal(Some(sender), &path, crate::portal_bus::REQUEST, "Response", Some(&(0u32, result).to_variant())).unwrap();
                invocation.return_value(Some(&(path,).to_variant()));
            }, |_, _, _, _, _| ().to_variant(), |_, _, _, _, _, _| false).unwrap();
            let session = ObjectPath::try_from("/session/synthetic").unwrap();
            let reply = request(&client, "BindShortcuts", |opts|
                (session, vec![(ID, Dict::new())], "", opts).to_variant()).await.unwrap();
            assert_eq!(trigger(&bindings(&reply).unwrap()).as_deref(), Some("Ctrl+Alt+K"));
            service.unregister_object(reg).unwrap();
        })).unwrap();
    }
}
