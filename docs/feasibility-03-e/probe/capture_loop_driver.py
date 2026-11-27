"""Approve every consent dialog of the repeated-capture fixture
(`capture::tests::back_to_back_captures_in_a_headless_session`) in a
disposable headless GNOME session.

Unlike `capture_driver.py`, this driver never opens a screencast of its own
while the fixture runs: it notices each dialog from the GNOME portal
backend's log and clicks Share with relative pointer motion through a
RemoteDesktop session that has no screencast. Extra screencast sessions in
the same compositor would otherwise compete with the fixture's."""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import portal_probe as probe  # noqa: E402  (reuses its bus helpers)

OUT = sys.argv[1]
LOG = os.path.join(os.environ["XDG_RUNTIME_DIR"], "xdpg.log")
MARKER = "Failed to associate portal window"
RD = probe.RD
# AGENT_CAPTURE_COMPETE=1 simulates another screencast client (a recorder or
# remote-desktop tool) that keeps opening Mutter screencasts while the
# fixture captures, which is what the original 03-E driver did by accident.
COMPETE = os.environ.get("AGENT_CAPTURE_COMPETE") == "1"


def exists(name):
    return os.path.exists(os.path.join(OUT, f"stage-{name}"))


def dialogs():
    try:
        with open(LOG, errors="replace") as log:
            return log.read().count(MARKER)
    except FileNotFoundError:
        return 0


def click_share():
    session = probe.call("/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", None, "(o)", dest=RD).unpack()[0]
    probe.call(session, RD + ".Session", "Start", None, dest=RD)
    for dx, dy in ((-4000.0, -4000.0), (925.0, 189.0)):
        probe.call(session, RD + ".Session", "NotifyPointerMotionRelative", probe.GLib.Variant("(dd)", (dx, dy)), dest=RD)
        time.sleep(0.05)
    for state in (True, False):
        probe.call(session, RD + ".Session", "NotifyPointerButton", probe.GLib.Variant("(ib)", (0x110, state)), dest=RD)
        time.sleep(0.05)
    probe.call(session, RD + ".Session", "Stop", None, dest=RD)


probe.keys([0xFF1B])  # leave the startup Overview
card = probe.subprocess.Popen([sys.executable, "-I", os.path.join(os.path.dirname(__file__), "testcard.py"),
                               "repeated capture", "--maximize"], env={**os.environ, "GDK_BACKEND": "wayland"})
time.sleep(2.5)
open(os.path.join(OUT, "stage-ready"), "w").close()
cycle = 0
seen = dialogs()
while not exists("done"):
    if not exists(f"consent-{cycle}"):
        time.sleep(0.01)
        continue
    end = time.time() + 30
    while dialogs() == seen and not exists(f"finished-{cycle}") and time.time() < end:
        if COMPETE:
            probe.frame_now()
        time.sleep(0.01)
    if dialogs() > seen and not exists(f"finished-{cycle}"):
        seen = dialogs()
        time.sleep(0.6)  # let the dialog map and take input
        if COMPETE:
            probe.click(925, 189)  # absolute motion: opens one more screencast
        else:
            click_share()
        print(f"cycle {cycle}: approved", flush=True)
    else:
        print(f"cycle {cycle}: no dialog", flush=True)
    while not exists(f"finished-{cycle}") and not exists("done"):
        time.sleep(0.01)
    seen = dialogs()
    cycle += 1
card.terminate()
print("driver finished")
