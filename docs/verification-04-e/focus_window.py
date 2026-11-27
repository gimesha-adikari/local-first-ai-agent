"""Development only: give the headless GNOME session a focused window (the
Shell's Access dialog needs a focused app), then screenshot the desktop
once the camera test signals that it is asking for permission.
Usage: python3 -I focus_window.py <output dir>"""
import os
import subprocess
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
probe_dir = os.path.join(here, "..", "feasibility-03-e", "probe")
out = sys.argv[1]
sys.path.insert(0, probe_dir)
sys.argv = [sys.argv[0], out]
import portal_probe as probe  # noqa: E402

probe.keys([0xFF1B])  # leave the startup Overview
# A plain window carrying the application's ID, the way the app's own
# window is focused when the user clicks its camera button.
WINDOW = """
import gi
gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk
GLib.set_prgname("io.github.gimesha-adikari.localfirstaiagent")
win = Gtk.Window(title="camera consent fixture")
win.add(Gtk.Label(label="Synthetic window for the camera permission test"))
win.connect("destroy", Gtk.main_quit)
win.maximize()
win.show_all()
win.present()
Gtk.main()
"""
card = subprocess.Popen([sys.executable, "-I", "-c", WINDOW], env={**os.environ, "GDK_BACKEND": "wayland"})
time.sleep(2)
open(os.path.join(out, "stage-focused"), "w").close()
deadline = time.time() + 120
while not os.path.exists(os.path.join(out, "stage-asking")) and time.time() < deadline:
    time.sleep(0.05)
time.sleep(2.0)
probe.screenshot("camera-access-dialog")
open(os.path.join(out, "stage-shot"), "w").close()
card.wait()
