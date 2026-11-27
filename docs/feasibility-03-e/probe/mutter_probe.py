"""03-E feasibility: single-frame, in-memory capture through Mutter ScreenCast
and PipeWire in a disposable headless session showing a synthetic test card."""
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time

import gi
gi.require_version("Gst", "1.0")
gi.require_version("Gtk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Gio, GLib, Gst, Gtk, GdkPixbuf

OUT = sys.argv[1]
CYCLES = int(sys.argv[2]) if len(sys.argv) > 2 else 10
Gst.init(None)
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
ctx = GLib.MainContext.default()
SC = "org.gnome.Mutter.ScreenCast"


def call(path, iface, method, args, reply):
    return bus.call_sync(SC, path, iface, method, args, GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 5000, None)


def spin_until(probe, seconds):
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        while ctx.pending():
            ctx.iteration(False)
        value = probe()
        if value is not None:
            return value
        time.sleep(0.001)
    return None


def nodes():
    dump = json.loads(subprocess.run(["pw-dump"], capture_output=True, text=True).stdout or "[]")
    return {o["id"] for o in dump if o.get("type") == "PipeWire:Interface:Node"
            and "gnome-shell" in json.dumps(o.get("info", {}).get("props", {}))}


def start_stream(kind="monitor", area=None):
    t0 = time.perf_counter()
    session = call("/org/gnome/Mutter/ScreenCast", SC, "CreateSession",
                   GLib.Variant("(a{sv})", ({},)), "(o)").unpack()[0]
    props = {"cursor-mode": GLib.Variant("u", 0)}
    if kind == "area":
        stream = call(session, SC + ".Session", "RecordArea",
                      GLib.Variant("(iiiia{sv})", (*area, props)), "(o)").unpack()[0]
    else:
        stream = call(session, SC + ".Session", "RecordMonitor",
                      GLib.Variant("(sa{sv})", ("", props)), "(o)").unpack()[0]
    added = {}
    sub = bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None,
                               Gio.DBusSignalFlags.NONE,
                               lambda *a: added.setdefault("node", a[5].unpack()[0]))
    call(session, SC + ".Session", "Start", None, None)
    node = spin_until(lambda: added.get("node"), 5)
    bus.signal_unsubscribe(sub)
    return session, node, (time.perf_counter() - t0) * 1000


def stop(session):
    call(session, SC + ".Session", "Stop", None, None)


def grab(node):
    """One frame into memory; the pipeline is torn down immediately."""
    t0 = time.perf_counter()
    pipeline = Gst.parse_launch(
        f"pipewiresrc path={node} always-copy=true do-timestamp=true ! videoconvert ! "
        "video/x-raw,format=RGB ! appsink name=sink sync=false max-buffers=1 drop=true")
    sink = pipeline.get_by_name("sink")
    pipeline.set_state(Gst.State.PLAYING)
    sample = sink.emit("try-pull-sample", 5 * Gst.SECOND)
    t_frame = time.perf_counter()
    data = caps = None
    if sample:
        buffer = sample.get_buffer()
        ok, info = buffer.map(Gst.MapFlags.READ)
        data = bytes(info.data)
        buffer.unmap(info)
        s = sample.get_caps().get_structure(0)
        caps = (s.get_value("width"), s.get_value("height"))
    pipeline.set_state(Gst.State.NULL)
    return data, caps, (t_frame - t0) * 1000, (time.perf_counter() - t_frame) * 1000


RD = "org.gnome.Mutter.RemoteDesktop"


def key(keysym):
    """Virtual keyboard in the disposable session (leaves the startup Overview)."""
    session = bus.call_sync(RD, "/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", None,
                            GLib.VariantType("(o)"), Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
    bus.call_sync(RD, session, RD + ".Session", "Start", None, None, Gio.DBusCallFlags.NONE, 5000, None)
    for state in (True, False):
        bus.call_sync(RD, session, RD + ".Session", "NotifyKeyboardKeysym", GLib.Variant("(ub)", (keysym, state)),
                      None, Gio.DBusCallFlags.NONE, 5000, None)
    time.sleep(0.1)
    bus.call_sync(RD, session, RD + ".Session", "Stop", None, None, Gio.DBusCallFlags.NONE, 5000, None)


def red_pixels(data, size):
    w, h = size
    stride = (w * 3 + 3) & ~3
    count = 0
    for y in range(0, h, 4):
        row = y * stride
        for x in range(0, w, 4):
            r, g, b = data[row + 3 * x:row + 3 * x + 3]
            count += r > 230 and g < 30 and b < 30
    return count


def pixel(data, size, x, y):
    w = size[0]
    stride = (w * 3 + 3) & ~3
    i = y * stride + x * 3
    return tuple(data[i:i + 3])


def p95(values):
    ordered = sorted(values)
    return ordered[max(0, int(round(0.95 * len(ordered))) - 1)]


def summary(values):
    return {"min": round(min(values), 1), "p50": round(statistics.median(values), 1),
            "p95": round(p95(values), 1), "max": round(max(values), 1)}


report = {"cycles": [], "checks": {}}
marker = time.time()
before_nodes = nodes()
key(0xFF1B)
time.sleep(1.0)
card = subprocess.Popen([sys.executable, "-I", os.path.join(os.path.dirname(__file__), "testcard.py"), "monitor"],
                        env={**os.environ, "GDK_BACKEND": "wayland"})
time.sleep(2.5)

first = None
for cycle in range(CYCLES):
    session, node, t_start = start_stream()
    data, size, t_frame, t_teardown = grab(node)
    t0 = time.perf_counter()
    stop(session)
    gone = spin_until(lambda: True if node not in nodes() else None, 5)
    t_closed = (time.perf_counter() - t0) * 1000
    report["cycles"].append({"start_to_node_ms": round(t_start, 1), "node_to_frame_ms": round(t_frame, 1),
                             "pipeline_teardown_ms": round(t_teardown, 1), "stop_to_node_removed_ms": round(t_closed, 1),
                             "node_removed": bool(gone), "frame": list(size) if size else None,
                             "bytes": len(data) if data else 0})
    if first is None and data:
        first = (data, size)

data, size = first
report["checks"]["test_card_red_samples"] = red_pixels(data, size)
report["checks"]["sha256_first_frame"] = hashlib.sha256(data).hexdigest()[:16]
GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(data), GdkPixbuf.Colorspace.RGB, False, 8, size[0], size[1],
                                (size[0] * 3 + 3) & ~3).savev(os.path.join(OUT, "mutter-monitor-frame.png"), "png", [], [])

# Region capture: an 800x600 area of the card.
session, node, t_start = start_stream("area", (100, 100, 800, 600))
adata, asize, t_frame, _ = grab(node)
stop(session)
report["checks"]["area_frame"] = list(asize) if asize else None
report["checks"]["area_start_to_frame_ms"] = round(t_start + t_frame, 1)

# Cancellation before any frame: stop as soon as the node appears.
session, node, _ = start_stream()
t0 = time.perf_counter()
stop(session)
report["checks"]["cancel_before_frame_node_removed"] = spin_until(lambda: True if node not in nodes() else None, 5) is True
report["checks"]["cancel_before_frame_ms"] = round((time.perf_counter() - t0) * 1000, 1)

# Frozen overlay from the in-memory frame, after the stream is closed.
pix = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(data), GdkPixbuf.Colorspace.RGB, False, 8, size[0], size[1],
                                      (size[0] * 3 + 3) & ~3)
overlay = Gtk.Window(title="frozen-overlay")
drawn = {}


def on_draw(widget, cr):
    from gi.repository import Gdk
    Gdk.cairo_set_source_pixbuf(cr, pix, 0, 0)
    cr.paint()
    cr.set_source_rgba(0, 0, 0, 0.35)
    cr.paint()
    drawn.setdefault("t", time.perf_counter())
    return False


overlay.set_app_paintable(True)
overlay.connect("draw", on_draw)
t0 = time.perf_counter()
overlay.fullscreen()
overlay.show_all()
spin_until(lambda: drawn.get("t"), 5)
report["checks"]["overlay_first_draw_ms"] = round((drawn["t"] - t0) * 1000, 1) if drawn else None
time.sleep(0.5)
session, node, _ = start_stream()
odata, osize, _, _ = grab(node)
stop(session)
# A capture taken while the overlay is up sees the dimmed frozen frame, so
# capture must finish (and the stream close) before the overlay maps.
report["checks"]["red_samples_while_dimmed_overlay_up"] = red_pixels(odata, osize)
overlay.destroy()
card.terminate()
card.wait()

# Indicator: capture the shell itself while one stream is active.
time.sleep(1.0)
session, node, _ = start_stream()
time.sleep(1.5)
idata, isize, _, _ = grab(node)
GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(idata), GdkPixbuf.Colorspace.RGB, False, 8, isize[0], isize[1],
                                (isize[0] * 3 + 3) & ~3).savev(os.path.join(OUT, "mutter-shell-during-stream.png"), "png", [], [])
stop(session)

spin_until(lambda: True if nodes() == before_nodes else None, 5)
report["checks"]["no_leftover_nodes"] = nodes() == before_nodes
created = subprocess.run(["find", os.environ["HOME"], os.environ["XDG_CACHE_HOME"], os.environ["XDG_DATA_HOME"],
                          os.environ["XDG_STATE_HOME"], "/tmp", "/dev/shm", "-xdev", "-newermt", f"@{marker}",
                          "-type", "f", "-not", "-path", OUT + "*"], capture_output=True, text=True).stdout.split()
report["checks"]["files_created_during_capture"] = sorted({f.split("/cache/")[0] + "/cache/mesa_shader_cache/*" if "mesa_shader_cache" in f else f for f in created})
report["summary"] = {k: summary([c[k] for c in report["cycles"]]) for k in
                     ["start_to_node_ms", "node_to_frame_ms", "stop_to_node_removed_ms"]}
report["summary"]["start_to_frame_ms"] = summary([c["start_to_node_ms"] + c["node_to_frame_ms"] for c in report["cycles"]])
print(json.dumps(report, indent=1))
