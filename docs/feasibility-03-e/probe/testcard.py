"""Synthetic full-screen test card. No user data."""
import sys
import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, GLib

def draw(_, cr):
    w, h = win.get_allocated_width(), win.get_allocated_height()
    cr.set_source_rgb(0.05, 0.08, 0.12); cr.paint()
    colors = [(1, 0, 0), (0, 1, 0), (0, 0, 1), (1, 1, 0), (0, 1, 1), (1, 0, 1), (1, 1, 1), (0.5, 0.5, 0.5)]
    for i, c in enumerate(colors):
        cr.set_source_rgb(*c); cr.rectangle(i * w / 8, 0, w / 8, h / 4); cr.fill()
    cr.set_source_rgb(1, 1, 1); cr.select_font_face("Sans"); cr.set_font_size(64)
    cr.move_to(80, h / 2); cr.show_text("SYNTHETIC 03-E TEST CARD")
    cr.set_font_size(32); cr.move_to(80, h / 2 + 60); cr.show_text(sys.argv[1] if len(sys.argv) > 1 else "frame")
    return False

win = Gtk.Window(title="synthetic-test-card")
win.connect("draw", draw); win.connect("destroy", Gtk.main_quit)
win.set_app_paintable(True)
if "--maximize" in sys.argv: win.maximize()
else: win.fullscreen()
win.show_all()
Gtk.main()
