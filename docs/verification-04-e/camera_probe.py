"""Probe the real XDG Camera portal on a private session bus (development
only). Prints IsCameraPresent, the AccessCamera response and whether
OpenPipeWireRemote returns a descriptor."""
import json
import os
import sys

from gi.repository import Gio, GLib

PORTAL = "org.freedesktop.portal.Desktop"
ROOT = "/org/freedesktop/portal/desktop"
CAM = "org.freedesktop.portal.Camera"
APP_ID = "io.github.gimesha-adikari.localfirstaiagent"
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)


def call(path, iface, method, args, reply=None, dest=PORTAL):
    return bus.call_sync(dest, path, iface, method, args,
                         GLib.VariantType(reply) if reply else None, Gio.DBusCallFlags.NONE, 10000, None)


def prop(name):
    return call(ROOT, "org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", (CAM, name)), "(v)").unpack()[0]


def access(wait=30):
    token = "probe%d" % os.getpid()
    sender = bus.get_unique_name()[1:].replace(".", "_")
    path = f"{ROOT}/request/{sender}/{token}"
    box = {}
    loop = GLib.MainLoop()
    sub = bus.signal_subscribe(PORTAL, "org.freedesktop.portal.Request", "Response", path, None,
                               Gio.DBusSignalFlags.NONE, lambda *a: (box.setdefault("r", a[5].unpack()), loop.quit()))
    call(ROOT, CAM, "AccessCamera", GLib.Variant("(a{sv})", ({"handle_token": GLib.Variant("s", token)},)), "(o)")
    GLib.timeout_add_seconds(wait, loop.quit)
    loop.run()
    bus.signal_unsubscribe(sub)
    return box.get("r")


call(ROOT, "org.freedesktop.host.portal.Registry", "Register", GLib.Variant("(sa{sv})", (APP_ID, {})))
report = {"version": prop("version"), "present": prop("IsCameraPresent")}
report["access"] = access()
if report["access"] and report["access"][0] == 0:
    reply, fds = bus.call_with_unix_fd_list_sync(PORTAL, ROOT, CAM, "OpenPipeWireRemote",
                                                 GLib.Variant("(a{sv})", ({},)), GLib.VariantType("(h)"),
                                                 Gio.DBusCallFlags.NONE, 10000, None, None)
    report["remote_fds"] = fds.get_length() if fds else 0
print("CAMERA-PROBE", json.dumps(report, default=str))
