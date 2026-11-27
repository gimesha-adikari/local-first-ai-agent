"""03-E feasibility: ScreenCast portal consent, OpenPipeWireRemote, one frame in
memory, immediate closure. Disposable headless GNOME session, synthetic card.
argv: OUT, key plan (comma-separated keysyms in hex, applied after the dialog
appears; 'shot' captures the screen), cycles."""
import json
import os
import subprocess
import sys
import time

import gi
gi.require_version("Gst", "1.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Gio, GLib, Gst, GdkPixbuf

sys.path.insert(0, os.path.dirname(__file__))
OUT = sys.argv[1] if len(sys.argv) > 1 else "."
Gst.init(None)
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
ctx = GLib.MainContext.default()
PORTAL, ROOT = "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop"
SCI = "org.freedesktop.portal.ScreenCast"
APP_ID = "io.github.gimesha-adikari.localfirstaiagent"
sequence = [0]


def spin_until(probe, seconds):
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        while ctx.pending():
            ctx.iteration(False)
        value = probe()
        if value is not None:
            return value
        time.sleep(0.002)
    return None


def call(path, iface, method, args, reply=None, fds=None, dest=PORTAL):
    if fds is not None:
        result, out = bus.call_with_unix_fd_list_sync(dest, path, iface, method, args,
                                                      GLib.VariantType(reply) if reply else None,
                                                      Gio.DBusCallFlags.NONE, 10000, None, None)
        return result, out
    return bus.call_sync(dest, path, iface, method, args, GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 10000, None)


def request(method, build, wait=60, during=None):
    sequence[0] += 1
    token = f"probe{os.getpid()}_{sequence[0]}"
    sender = bus.get_unique_name()[1:].replace(".", "_")
    path = f"{ROOT}/request/{sender}/{token}"
    box = {}
    sub = bus.signal_subscribe(PORTAL, "org.freedesktop.portal.Request", "Response", path, None,
                               Gio.DBusSignalFlags.NONE, lambda *a: box.setdefault("r", a[5].unpack()))
    t0 = time.perf_counter()
    call(ROOT, SCI, method, build({"handle_token": GLib.Variant("s", token)}), "(o)")
    if during:
        during(box)
    spin_until(lambda: box.get("r"), wait)
    done = time.perf_counter()
    bus.signal_unsubscribe(sub)
    if "clicked" in box:
        box["response_after_click_ms"] = round((done - box["clicked"]) * 1000, 1)
    request.last = box
    return box.get("r"), (done - t0) * 1000


# Mutter-side screenshot of the private session (to see the consent dialog).
SC = "org.gnome.Mutter.ScreenCast"


def screenshot(name):
    session = call("/org/gnome/Mutter/ScreenCast", SC, "CreateSession", GLib.Variant("(a{sv})", ({},)), "(o)",
                   dest=SC).unpack()[0]
    stream = call(session, SC + ".Session", "RecordMonitor", GLib.Variant("(sa{sv})", ("", {})), "(o)",
                  dest=SC).unpack()[0]
    box = {}
    sub = bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None, Gio.DBusSignalFlags.NONE,
                               lambda *a: box.setdefault("n", a[5].unpack()[0]))
    call(session, SC + ".Session", "Start", None, dest=SC)
    node = spin_until(lambda: box.get("n"), 5)
    bus.signal_unsubscribe(sub)
    pipeline = Gst.parse_launch(f"pipewiresrc path={node} ! videoconvert ! pngenc ! filesink location={OUT}/{name}.png")
    pipeline.set_state(Gst.State.PLAYING)
    pipeline.get_bus().timed_pop_filtered(5 * Gst.SECOND, Gst.MessageType.EOS | Gst.MessageType.ERROR)
    time.sleep(0.5)
    pipeline.set_state(Gst.State.NULL)
    call(session, SC + ".Session", "Stop", None, dest=SC)


RD = "org.gnome.Mutter.RemoteDesktop"


def keys(keysyms):
    session = call("/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", None, "(o)", dest=RD).unpack()[0]
    call(session, RD + ".Session", "Start", None, dest=RD)
    for k in keysyms:
        for state in (True, False):
            call(session, RD + ".Session", "NotifyKeyboardKeysym", GLib.Variant("(ub)", (k, state)), dest=RD)
        time.sleep(0.15)
    call(session, RD + ".Session", "Stop", None, dest=RD)


def click(x, y):
    """Pointer click in the private session, the way a user approves or cancels."""
    rd = call("/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", None, "(o)", dest=RD).unpack()[0]
    sid = call(rd, "org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", (RD + ".Session", "SessionId")),
               "(v)", dest=RD).unpack()[0]
    sc = call("/org/gnome/Mutter/ScreenCast", SC, "CreateSession",
              GLib.Variant("(a{sv})", ({"remote-desktop-session-id": GLib.Variant("s", sid)},)), "(o)", dest=SC).unpack()[0]
    stream = call(sc, SC + ".Session", "RecordMonitor", GLib.Variant("(sa{sv})", ("", {})), "(o)", dest=SC).unpack()[0]
    box = {}
    sub = bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None, Gio.DBusSignalFlags.NONE,
                               lambda *a: box.setdefault("n", 1))
    call(rd, RD + ".Session", "Start", None, dest=RD)
    spin_until(lambda: box.get("n"), 5)
    bus.signal_unsubscribe(sub)
    for px, py in ((640.0, 400.0), (float(x), float(y))):
        call(rd, RD + ".Session", "NotifyPointerMotionAbsolute", GLib.Variant("(sdd)", (stream, px, py)), dest=RD)
        time.sleep(0.2)
    for state in (True, False):
        call(rd, RD + ".Session", "NotifyPointerButton", GLib.Variant("(ib)", (0x110, state)), dest=RD)
        time.sleep(0.05)
    t = time.perf_counter()
    call(rd, RD + ".Session", "Stop", None, dest=RD)
    return t


def frame_now():
    session = call("/org/gnome/Mutter/ScreenCast", SC, "CreateSession", GLib.Variant("(a{sv})", ({},)), "(o)",
                   dest=SC).unpack()[0]
    stream = call(session, SC + ".Session", "RecordMonitor", GLib.Variant("(sa{sv})", ("", {})), "(o)",
                  dest=SC).unpack()[0]
    box = {}
    sub = bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None, Gio.DBusSignalFlags.NONE,
                               lambda *a: box.setdefault("n", a[5].unpack()[0]))
    call(session, SC + ".Session", "Start", None, dest=SC)
    node = spin_until(lambda: box.get("n"), 5)
    bus.signal_unsubscribe(sub)
    pipeline = Gst.parse_launch(f"pipewiresrc path={node} always-copy=true ! videoconvert ! video/x-raw,format=RGB ! "
                                "appsink name=sink sync=false max-buffers=1 drop=true")
    pipeline.set_state(Gst.State.PLAYING)
    sample = pipeline.get_by_name("sink").emit("try-pull-sample", 5 * Gst.SECOND)
    data = None
    if sample:
        buffer = sample.get_buffer()
        ok, info = buffer.map(Gst.MapFlags.READ)
        data = bytes(info.data)
        buffer.unmap(info)
    pipeline.set_state(Gst.State.NULL)
    call(session, SC + ".Session", "Stop", None, dest=SC)
    return data


def share_button_visible():
    data = frame_now()
    if not data:
        return None
    stride = (1280 * 3 + 3) & ~3
    r, g, b = data[189 * stride + 925 * 3:189 * stride + 925 * 3 + 3]
    return True if r > 200 and 60 < g < 130 and b < 60 else None


def nodes():
    dump = json.loads(subprocess.run(["pw-dump"], capture_output=True, text=True).stdout or "[]")
    return {o["id"] for o in dump if o.get("type") == "PipeWire:Interface:Node"}


if __name__ == "__main__":
    PLAN = json.loads(sys.argv[2])
    keys([0xFF1B])  # leave the startup Overview
    card = subprocess.Popen([sys.executable, "-I", os.path.join(os.path.dirname(__file__), "testcard.py"), "portal", "--maximize"],
                            env={**os.environ, "GDK_BACKEND": "wayland"})
    time.sleep(2.5)
    report = {"registry": None, "cycles": []}
    call(ROOT, "org.freedesktop.host.portal.Registry", "Register", GLib.Variant("(sa{sv})", (APP_ID, {})))
    report["registry"] = "registered"
    report["version"] = call(ROOT, "org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", (SCI, "version")),
                             "(v)").unpack()[0]
    baseline = nodes()

    restore = None
    for cycle, plan in enumerate(PLAN):
        entry = {"plan": plan}
        r, t = request("CreateSession", lambda o: GLib.Variant("(a{sv})", ({**o, "session_handle_token": GLib.Variant("s", f"s{cycle}")},)))
        entry["create_session"] = {"code": r[0], "ms": round(t, 1)}
        session = r[1]["session_handle"]
        r, t = request("SelectSources", lambda o: GLib.Variant("(oa{sv})", (session, {
            **o, "types": GLib.Variant("u", 1), "multiple": GLib.Variant("b", False),
            "cursor_mode": GLib.Variant("u", 1), "persist_mode": GLib.Variant("u", plan["persist"]),
            **({"restore_token": GLib.Variant("s", restore)} if plan.get("restore") and restore else {})})))
        entry["select_sources"] = {"code": r[0], "ms": round(t, 1)}

        def drive(box):
            t0 = time.perf_counter()
            seen = spin_until(lambda: True if box.get("r") else share_button_visible(), 10)
            entry["dialog_visible_ms"] = round((time.perf_counter() - t0) * 1000, 1) if seen and not box.get("r") else None
            if box.get("r"):
                entry["no_dialog"] = True
                return
            if cycle == 0:
                screenshot(f"portal-dialog-{cycle}")
            target = (925, 189) if plan["action"] == "share" else (357, 189)
            box["clicked"] = click(*target)

        r, t = request("Start", lambda o: GLib.Variant("(osa{sv})", (session, "", o)), wait=30, during=drive)
        entry["start"] = {"code": None if r is None else r[0], "ms_including_dialog": round(t, 1)}
        entry["response_after_click_ms"] = request.last.get("response_after_click_ms")
        if r and r[0] == 0 and "restore_token" in r[1]:
            restore = r[1]["restore_token"]
        if r and r[0] == 0:
            streams = r[1].get("streams", [])
            entry["streams"] = [(node, {k: str(v) for k, v in props.items()}) for node, props in streams]
            entry["restore_token_returned"] = "restore_token" in r[1]
            t0 = time.perf_counter()
            result, fdlist = call(ROOT, SCI, "OpenPipeWireRemote", GLib.Variant("(oa{sv})", (session, {})), "(h)", fds=True)
            fd = fdlist.get(result.unpack()[0])
            node = streams[0][0]
            pipeline = Gst.parse_launch(f"pipewiresrc fd={fd} path={node} always-copy=true ! videoconvert ! "
                                        "video/x-raw,format=RGB ! appsink name=sink sync=false max-buffers=1 drop=true")
            pipeline.set_state(Gst.State.PLAYING)
            sink = pipeline.get_by_name("sink")
            sample = sink.emit("try-pull-sample", 5 * Gst.SECOND)
            entry["remote_to_frame_ms"] = round((time.perf_counter() - t0) * 1000, 1)
            if cycle == 0 and sample:
                st = sample.get_caps().get_structure(0)
                w, h = st.get_value("width"), st.get_value("height")
                buffer = sample.get_buffer()
                ok, info = buffer.map(Gst.MapFlags.READ)
                GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(bytes(info.data)), GdkPixbuf.Colorspace.RGB, False, 8, w, h,
                                                (w * 3 + 3) & ~3).savev(f"{OUT}/portal-first-frame.png", "png", [], [])
                buffer.unmap(info)

            def clean(sample):
                st = sample.get_caps().get_structure(0)
                w = st.get_value("width")
                buffer = sample.get_buffer()
                ok, info = buffer.map(Gst.MapFlags.READ)
                stride = (w * 3 + 3) & ~3
                r, g, b = info.data[420 * stride + 640 * 3:420 * stride + 640 * 3 + 3]
                buffer.unmap(info)
                return r < 60 and g < 60 and b < 60

            discarded = 0
            while sample and not clean(sample) and time.perf_counter() - t0 < 3:
                discarded += 1
                sample = sink.emit("try-pull-sample", 1 * Gst.SECOND)
            entry["remote_to_clean_frame_ms"] = round((time.perf_counter() - t0) * 1000, 1) if sample and clean(sample) else None
            entry["frames_discarded_for_dialog"] = discarded
            if sample:
                s = sample.get_caps().get_structure(0)
                buffer = sample.get_buffer()
                ok, info = buffer.map(Gst.MapFlags.READ)
                frame = bytes(info.data)
                buffer.unmap(info)
                entry["frame"] = [s.get_value("width"), s.get_value("height"), len(frame)]
                if cycle == 0:
                    w, h = s.get_value("width"), s.get_value("height")
                    GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(frame), GdkPixbuf.Colorspace.RGB, False, 8, w, h,
                                                    (w * 3 + 3) & ~3).savev(f"{OUT}/portal-clean-frame.png", "png", [], [])
            pipeline.set_state(Gst.State.NULL)
            os.close(fd)
            t0 = time.perf_counter()
            call(session, "org.freedesktop.portal.Session", "Close", None)
            closed = spin_until(lambda: True if node not in nodes() else None, 5)
            entry["close_to_node_removed_ms"] = round((time.perf_counter() - t0) * 1000, 1) if closed else None
        else:
            try:
                call(session, "org.freedesktop.portal.Session", "Close", None)
            except GLib.Error:
                pass
        report["cycles"].append(entry)
        time.sleep(1.0)

    report["leftover_nodes"] = sorted(nodes() - baseline)
    store = subprocess.run(["gdbus", "call", "--session", "--dest", "org.freedesktop.impl.portal.PermissionStore",
                            "--object-path", "/org/freedesktop/impl/portal/PermissionStore", "--method",
                            "org.freedesktop.impl.portal.PermissionStore.List", "screencast"],
                           capture_output=True, text=True)
    report["permission_store_screencast"] = (store.stdout or store.stderr).strip()[:300]
    card.terminate()
    print(json.dumps(report, indent=1, default=str))
