"""Drive the 03-E native capture fixture in a disposable headless GNOME
session: approve or cancel the real consent dialog, drag a region on the
frozen overlay, adjust it with the keyboard and confirm it. Screenshots are
taken through a separate Mutter ScreenCast session of the private display."""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import portal_probe as probe  # noqa: E402  (reuses its bus helpers)

OUT = sys.argv[1]
probe.OUT = OUT


def wait_stage(name, seconds=90):
    end = time.time() + seconds
    while not os.path.exists(os.path.join(OUT, f"stage-{name}")):
        if time.time() > end:
            raise SystemExit(f"timed out waiting for {name}")
        time.sleep(0.05)


def mark(name):
    open(os.path.join(OUT, f"stage-{name}"), "w").close()


def wait_dialog():
    seen = probe.spin_until(probe.share_button_visible, 20)
    if not seen:
        raise SystemExit("consent dialog did not appear")


def drag(start, end, steps=12):
    RD, SC = probe.RD, probe.SC
    rd = probe.call("/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", None, "(o)", dest=RD).unpack()[0]
    sid = probe.call(rd, "org.freedesktop.DBus.Properties", "Get",
                     probe.GLib.Variant("(ss)", (RD + ".Session", "SessionId")), "(v)", dest=RD).unpack()[0]
    sc = probe.call("/org/gnome/Mutter/ScreenCast", SC, "CreateSession",
                    probe.GLib.Variant("(a{sv})", ({"remote-desktop-session-id": probe.GLib.Variant("s", sid)},)),
                    "(o)", dest=SC).unpack()[0]
    stream = probe.call(sc, SC + ".Session", "RecordMonitor", probe.GLib.Variant("(sa{sv})", ("", {})),
                        "(o)", dest=SC).unpack()[0]
    box = {}
    sub = probe.bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None,
                                     probe.Gio.DBusSignalFlags.NONE, lambda *a: box.setdefault("n", 1))
    probe.call(rd, RD + ".Session", "Start", None, dest=RD)
    probe.spin_until(lambda: box.get("n"), 5)
    probe.bus.signal_unsubscribe(sub)

    def move(x, y):
        probe.call(rd, RD + ".Session", "NotifyPointerMotionAbsolute",
                   probe.GLib.Variant("(sdd)", (stream, float(x), float(y))), dest=RD)
        time.sleep(0.03)

    move(640, 400)
    move(*start)
    probe.call(rd, RD + ".Session", "NotifyPointerButton", probe.GLib.Variant("(ib)", (0x110, True)), dest=RD)
    for i in range(1, steps + 1):
        move(start[0] + (end[0] - start[0]) * i / steps, start[1] + (end[1] - start[1]) * i / steps)
    probe.call(rd, RD + ".Session", "NotifyPointerButton", probe.GLib.Variant("(ib)", (0x110, False)), dest=RD)
    time.sleep(0.2)
    probe.call(rd, RD + ".Session", "Stop", None, dest=RD)


probe.keys([0xFF1B])  # leave the startup Overview
card = probe.subprocess.Popen([sys.executable, "-I", os.path.join(os.path.dirname(__file__), "testcard.py"),
                               "native capture", "--maximize"], env={**os.environ, "GDK_BACKEND": "wayland"})
time.sleep(2.5)
mark("ready")
wait_stage("consent-cancel")
wait_dialog()
probe.click(357, 189)
mark("cancelled")
wait_stage("consent-share")
wait_dialog()
probe.screenshot("native-consent-dialog")
probe.click(925, 189)
wait_stage("frame")
wait_stage("consent-share-2")
wait_dialog()
probe.click(925, 189)
wait_stage("overlay")
time.sleep(0.4)
probe.screenshot("native-overlay-initial")
drag((60, 330), (900, 500))
time.sleep(0.3)
probe.screenshot("native-overlay-selected")
probe.click(480, 415)  # a click inside the selection focuses the overlay
time.sleep(0.2)
probe.keys([0xFF53, 0xFF0D])  # Right adjusts by one pixel, Return confirms
end = time.time() + 3
while time.time() < end and not os.path.exists(os.path.join(OUT, "stage-done")):
    time.sleep(0.1)
if not os.path.exists(os.path.join(OUT, "stage-done")):
    print("Return did not confirm; clicking Use region")
    probe.click(852, 527)
wait_stage("done")
card.terminate()
print("driver finished")
