"""Write the synthetic camera fixtures (development only): known text and
simple visual objects, plus a sideways photo with EXIF orientation 6 for
the attachment orientation check. Usage: python3 -I make_fixtures.py <dir>"""
import os
import struct
import sys

import cairo

out = sys.argv[1]
os.makedirs(out, exist_ok=True)


def card(name, title, subtitle, background, shape):
    surface = cairo.ImageSurface(cairo.FORMAT_RGB24, 640, 480)
    c = cairo.Context(surface)
    c.set_source_rgb(*background)
    c.paint()
    # A red square and a blue circle: visual objects with known positions.
    c.set_source_rgb(0.85, 0.1, 0.1)
    c.rectangle(40, 300, 140, 140)
    c.fill()
    c.set_source_rgb(0.1, 0.2, 0.85)
    c.arc(540, 370, 70, 0, 6.2832)
    c.fill()
    if shape == "triangle":
        c.set_source_rgb(0.1, 0.6, 0.2)
        c.move_to(320, 290)
        c.line_to(250, 440)
        c.line_to(390, 440)
        c.close_path()
        c.fill()
    c.set_source_rgb(0, 0, 0)
    c.select_font_face("DejaVu Sans", cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_BOLD)
    c.set_font_size(54)
    c.move_to(40, 110)
    c.show_text(title)
    c.set_font_size(34)
    c.move_to(40, 190)
    c.show_text(subtitle)
    surface.write_to_png(os.path.join(out, name))


card("camera-card.png", "CAMERA TEST 42", "Visual search fixture", (0.97, 0.97, 0.94), "none")
card("second-card.png", "SECOND CAMERA", "Bonjour le monde", (0.9, 0.95, 1.0), "triangle")
print("fixtures in", out)
