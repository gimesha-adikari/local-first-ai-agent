//! The frozen-frame region selector: a native full-screen GTK window that
//! paints the captured frame from host memory. The live stream has already
//! closed, so the overlay itself is never captured, and the frame never
//! enters a WebView or application IPC. The frame is wiped when the overlay
//! closes; only the confirmed crop leaves it.
use std::{cell::RefCell, rc::Rc, time::Duration};

use agent_core::{
    inference::Cancellation,
    screen::{Frame, Pixels, Region, crop_pixels, map_selection},
};
use gtk::{cairo, gdk, glib, prelude::*};

pub(crate) enum Outcome {
    Confirmed { region: Region, crop: Frame },
    Cancelled,
    TimedOut,
}

const TIMEOUT: Duration = Duration::from_secs(300);
const HANDLE: f64 = 10.0;
const ACCENT: (f64, f64, f64) = (0.36, 0.45, 0.31);

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    New,
    Move(f64, f64),
    /// Which edges follow the pointer: left, top, right, bottom.
    Resize(bool, bool, bool, bool),
}

struct State {
    width: u32,
    height: u32,
    pixels: Pixels,
    /// Selection corners in image-area coordinates.
    selection: Option<(f64, f64, f64, f64)>,
    drag: Drag,
    /// Image placement inside the window: offset and scale.
    placement: (f64, f64, f64),
    reply: Option<Box<dyn FnOnce(Outcome)>>,
}

impl State {
    fn normalized(&self) -> Option<(f64, f64, f64, f64)> {
        self.selection
            .map(|(x0, y0, x1, y1)| (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
    }

    fn view(&self) -> (f64, f64) {
        let (_, _, scale) = self.placement;
        (
            f64::from(self.width) * scale,
            f64::from(self.height) * scale,
        )
    }

    fn region(&self) -> Option<Region> {
        let (x0, y0, x1, y1) = self.normalized()?;
        map_selection(((x0, y0), (x1, y1)), self.view(), (self.width, self.height)).ok()
    }

    fn finish(&mut self, outcome: Outcome) {
        if let Some(reply) = self.reply.take() {
            reply(outcome);
        }
    }

    /// Toolbar buttons (cancel, confirm) beside the selection, in window coordinates.
    fn buttons(&self, window: (f64, f64)) -> Option<[(f64, f64, f64, f64); 2]> {
        let (x0, _, x1, y1) = self.normalized()?;
        let (ox, oy, _) = self.placement;
        let (w, h) = (96.0, 32.0);
        let gap = 8.0;
        let right = (x1 + ox).max(2.0 * w + gap + 8.0).min(window.0 - 8.0);
        let mut top = y1 + oy + 10.0;
        if top + h > window.1 - 8.0 {
            top = (y1 + oy - h - 10.0).max(8.0);
        }
        let _ = x0;
        Some([(right - 2.0 * w - gap, top, w, h), (right - w, top, w, h)])
    }
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.close_path();
}

fn label(cr: &cairo::Context, text: &str, size: f64) -> (f64, f64) {
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(size);
    cr.text_extents(text)
        .map(|extents| (extents.x_advance(), extents.height()))
        .unwrap_or((0.0, size))
}

fn pill(
    cr: &cairo::Context,
    (x, y, w, h): (f64, f64, f64, f64),
    text: &str,
    fill: (f64, f64, f64, f64),
) {
    rounded(cr, x, y, w, h, h / 2.0);
    cr.set_source_rgba(fill.0, fill.1, fill.2, fill.3);
    let _ = cr.fill();
    let (tw, th) = label(cr, text, 13.0);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.move_to(x + (w - tw) / 2.0, y + (h + th) / 2.0 - 1.0);
    let _ = cr.show_text(text);
}

fn draw(state: &mut State, cr: &cairo::Context, window: (f64, f64)) {
    let (fw, fh) = (f64::from(state.width), f64::from(state.height));
    let scale = (window.0 / fw).min(window.1 / fh);
    let (ox, oy) = ((window.0 - fw * scale) / 2.0, (window.1 - fh * scale) / 2.0);
    state.placement = (ox, oy, scale);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    let _ = cr.paint();
    let stride = cairo::Format::ARgb32
        .stride_for_width(state.width)
        .unwrap_or(0);
    // SAFETY: the surface borrows `pixels` (BGRA, tightly packed, stride
    // width * 4) only for this draw call and is dropped before returning.
    let surface = unsafe {
        cairo::ImageSurface::create_for_data_unsafe(
            state.pixels.as_mut().as_mut_ptr(),
            cairo::Format::ARgb32,
            state.width as i32,
            state.height as i32,
            stride,
        )
    };
    if let Ok(surface) = surface {
        let _ = cr.save();
        cr.translate(ox, oy);
        cr.scale(scale, scale);
        let _ = cr.set_source_surface(&surface, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Good);
        let _ = cr.paint();
        let _ = cr.restore();
        surface.finish();
    }
    // Dim everything outside the selection.
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.rectangle(0.0, 0.0, window.0, window.1);
    if let Some((x0, y0, x1, y1)) = state.normalized() {
        cr.rectangle(x0 + ox, y0 + oy, x1 - x0, y1 - y0);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        let _ = cr.fill();
        let (x, y, w, h) = (x0 + ox, y0 + oy, x1 - x0, y1 - y0);
        cr.set_line_width(3.0);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
        cr.rectangle(x - 0.5, y - 0.5, w + 1.0, h + 1.0);
        let _ = cr.stroke();
        cr.set_line_width(1.5);
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(x, y, w, h);
        let _ = cr.stroke();
        for (hx, hy) in [
            (x, y),
            (x + w / 2.0, y),
            (x + w, y),
            (x, y + h / 2.0),
            (x + w, y + h / 2.0),
            (x, y + h),
            (x + w / 2.0, y + h),
            (x + w, y + h),
        ] {
            cr.rectangle(hx - 4.0, hy - 4.0, 8.0, 8.0);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let _ = cr.fill_preserve();
            cr.set_source_rgb(ACCENT.0, ACCENT.1, ACCENT.2);
            cr.set_line_width(1.5);
            let _ = cr.stroke();
        }
        if let Some(region) = state.region() {
            let text = format!("{} × {} px", region.width, region.height);
            let (tw, _) = label(cr, &text, 12.0);
            let by = if y > 34.0 { y - 30.0 } else { y + 8.0 };
            pill(
                cr,
                (x, by, tw + 20.0, 24.0),
                &text,
                (0.07, 0.07, 0.08, 0.82),
            );
        }
        if let Some([cancel, confirm]) = state.buttons(window) {
            pill(cr, cancel, "Cancel", (0.07, 0.07, 0.08, 0.86));
            let ready = state.region().is_some();
            pill(
                cr,
                confirm,
                "Use region",
                (
                    ACCENT.0,
                    ACCENT.1,
                    ACCENT.2,
                    if ready { 0.98 } else { 0.45 },
                ),
            );
        }
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
        let _ = cr.fill();
    }
    cr.set_fill_rule(cairo::FillRule::Winding);
    let hint = "Drag to select a region  ·  Enter to confirm  ·  Esc to cancel";
    let (tw, _) = label(cr, hint, 13.0);
    pill(
        cr,
        ((window.0 - tw) / 2.0 - 18.0, 18.0, tw + 36.0, 34.0),
        hint,
        (0.07, 0.07, 0.08, 0.82),
    );
}

fn hit(state: &State, x: f64, y: f64) -> Drag {
    let Some((x0, y0, x1, y1)) = state.normalized() else {
        return Drag::New;
    };
    let near = |a: f64, b: f64| (a - b).abs() <= HANDLE;
    let within_y = y >= y0 - HANDLE && y <= y1 + HANDLE;
    let within_x = x >= x0 - HANDLE && x <= x1 + HANDLE;
    let (left, right) = (near(x, x0) && within_y, near(x, x1) && within_y);
    let (top, bottom) = (near(y, y0) && within_x, near(y, y1) && within_x);
    if left || right || top || bottom {
        Drag::Resize(left, top, right, bottom)
    } else if x > x0 && x < x1 && y > y0 && y < y1 {
        Drag::Move(x - x0, y - y0)
    } else {
        Drag::New
    }
}

/// Show the selector on the captured monitor (logical geometry when known).
/// `reply` is called exactly once. Must run on the GTK main thread.
pub(crate) fn select(
    frame: Frame,
    monitor: Option<(i32, i32, i32, i32)>,
    cancellation: Cancellation,
    reply: impl FnOnce(Outcome) + 'static,
) {
    let (width, height, pixels) = frame.into_pixels();
    let state = Rc::new(RefCell::new(State {
        width,
        height,
        pixels,
        selection: None,
        drag: Drag::None,
        placement: (0.0, 0.0, 1.0),
        reply: Some(Box::new(reply)),
    }));
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("Select a screen region");
    window.set_decorated(false);
    window.set_keep_above(true);
    window.set_skip_taskbar_hint(true);
    window.set_app_paintable(true);
    let area = gtk::DrawingArea::new();
    area.set_can_focus(true);
    area.add_events(
        gdk::EventMask::BUTTON_PRESS_MASK
            | gdk::EventMask::BUTTON_RELEASE_MASK
            | gdk::EventMask::POINTER_MOTION_MASK
            | gdk::EventMask::KEY_PRESS_MASK,
    );
    window.add(&area);

    let drawing = Rc::clone(&state);
    area.connect_draw(move |widget, cr| {
        let size = (
            f64::from(widget.allocated_width()),
            f64::from(widget.allocated_height()),
        );
        draw(&mut drawing.borrow_mut(), cr, size);
        glib::Propagation::Stop
    });

    let close = {
        let window = window.clone();
        let state = Rc::clone(&state);
        Rc::new(move |outcome: Outcome| {
            state.borrow_mut().finish(outcome);
            // SAFETY: GTK destroys the toplevel on the main thread.
            unsafe { window.destroy() };
        })
    };

    let confirm = {
        let state = Rc::clone(&state);
        let close = Rc::clone(&close);
        Rc::new(move || {
            let outcome = {
                let current = state.borrow();
                current.region().and_then(|region| {
                    crop_pixels(
                        current.pixels.as_slice(),
                        current.width,
                        current.height,
                        region,
                    )
                    .ok()
                    .map(|crop| Outcome::Confirmed { region, crop })
                })
            };
            if let Some(outcome) = outcome {
                close(outcome);
            }
        })
    };

    let pressing = Rc::clone(&state);
    let (press_confirm, press_close, press_window) =
        (Rc::clone(&confirm), Rc::clone(&close), window.clone());
    area.connect_button_press_event(move |widget, event| {
        widget.grab_focus();
        let (wx, wy) = event.position();
        let size = (
            f64::from(press_window.allocated_width()),
            f64::from(press_window.allocated_height()),
        );
        if let Some([cancel, accept]) = pressing.borrow().buttons(size) {
            let inside = |(x, y, w, h): (f64, f64, f64, f64)| {
                wx >= x && wx <= x + w && wy >= y && wy <= y + h
            };
            if inside(cancel) {
                glib::idle_add_local_once({
                    let close = Rc::clone(&press_close);
                    move || close(Outcome::Cancelled)
                });
                return glib::Propagation::Stop;
            }
            if inside(accept) {
                glib::idle_add_local_once({
                    let confirm = Rc::clone(&press_confirm);
                    move || confirm()
                });
                return glib::Propagation::Stop;
            }
        }
        let mut current = pressing.borrow_mut();
        let (ox, oy, _) = current.placement;
        let (x, y) = (wx - ox, wy - oy);
        if event.event_type() == gdk::EventType::DoubleButtonPress && current.region().is_some() {
            drop(current);
            glib::idle_add_local_once({
                let confirm = Rc::clone(&press_confirm);
                move || confirm()
            });
            return glib::Propagation::Stop;
        }
        current.drag = hit(&current, x, y);
        if current.drag == Drag::New {
            current.selection = Some((x, y, x, y));
        }
        widget.queue_draw();
        glib::Propagation::Stop
    });

    let moving = Rc::clone(&state);
    area.connect_motion_notify_event(move |widget, event| {
        let mut current = moving.borrow_mut();
        let (ox, oy, _) = current.placement;
        let (vw, vh) = current.view();
        let (wx, wy) = event.position();
        let (x, y) = ((wx - ox).clamp(0.0, vw), (wy - oy).clamp(0.0, vh));
        let window = widget.window();
        let cursor_name = match current.drag {
            Drag::Move(..) => "grabbing",
            Drag::None => match hit(&current, x, y) {
                Drag::Move(..) => "grab",
                Drag::Resize(..) => "nwse-resize",
                _ => "crosshair",
            },
            _ => "crosshair",
        };
        if let Some(window) = window {
            window.set_cursor(gdk::Cursor::from_name(&widget.display(), cursor_name).as_ref());
        }
        match (current.drag, current.normalized()) {
            (Drag::New, _) => {
                if let Some(selection) = current.selection.as_mut() {
                    selection.2 = x;
                    selection.3 = y;
                }
            }
            (Drag::Move(dx, dy), Some((x0, y0, x1, y1))) => {
                let (w, h) = (x1 - x0, y1 - y0);
                let nx = (x - dx).clamp(0.0, vw - w);
                let ny = (y - dy).clamp(0.0, vh - h);
                current.selection = Some((nx, ny, nx + w, ny + h));
            }
            (Drag::Resize(left, top, right, bottom), Some((mut x0, mut y0, mut x1, mut y1))) => {
                if left {
                    x0 = x;
                }
                if right {
                    x1 = x;
                }
                if top {
                    y0 = y;
                }
                if bottom {
                    y1 = y;
                }
                current.selection = Some((x0, y0, x1, y1));
            }
            _ => return glib::Propagation::Proceed,
        }
        widget.queue_draw();
        glib::Propagation::Stop
    });

    let releasing = Rc::clone(&state);
    area.connect_button_release_event(move |widget, _| {
        let mut current = releasing.borrow_mut();
        current.drag = Drag::None;
        if current.region().is_none() {
            current.selection = None;
        } else {
            current.selection = current.normalized();
        }
        widget.queue_draw();
        glib::Propagation::Stop
    });

    let keys = Rc::clone(&state);
    let (key_confirm, key_close) = (Rc::clone(&confirm), Rc::clone(&close));
    // Keys are handled on the toplevel, ahead of GtkWindow's own bindings
    // (which would otherwise take Return as "activate default").
    let key_area = area.clone();
    window.connect_key_press_event(move |_, event| {
        let widget = &key_area;
        let key = event.keyval();
        if key == gdk::keys::constants::Escape {
            glib::idle_add_local_once({
                let close = Rc::clone(&key_close);
                move || close(Outcome::Cancelled)
            });
            return glib::Propagation::Stop;
        }
        if key == gdk::keys::constants::Return || key == gdk::keys::constants::KP_Enter {
            glib::idle_add_local_once({
                let confirm = Rc::clone(&key_confirm);
                move || confirm()
            });
            return glib::Propagation::Stop;
        }
        let step = if event.state().contains(gdk::ModifierType::SHIFT_MASK) {
            10.0
        } else {
            1.0
        };
        let resize = event.state().contains(gdk::ModifierType::CONTROL_MASK);
        let (dx, dy) = match key {
            k if k == gdk::keys::constants::Left => (-step, 0.0),
            k if k == gdk::keys::constants::Right => (step, 0.0),
            k if k == gdk::keys::constants::Up => (0.0, -step),
            k if k == gdk::keys::constants::Down => (0.0, step),
            _ => return glib::Propagation::Proceed,
        };
        let mut current = keys.borrow_mut();
        let (vw, vh) = current.view();
        if let Some((x0, y0, x1, y1)) = current.normalized() {
            current.selection = Some(if resize {
                (
                    x0,
                    y0,
                    (x1 + dx).clamp(x0 + 1.0, vw),
                    (y1 + dy).clamp(y0 + 1.0, vh),
                )
            } else {
                let (w, h) = (x1 - x0, y1 - y0);
                let nx = (x0 + dx).clamp(0.0, vw - w);
                let ny = (y0 + dy).clamp(0.0, vh - h);
                (nx, ny, nx + w, ny + h)
            });
            widget.queue_draw();
        }
        glib::Propagation::Stop
    });

    let destroyed = Rc::clone(&state);
    window.connect_destroy(move |_| destroyed.borrow_mut().finish(Outcome::Cancelled));

    // Cancellation (panel dismissed) and the overall time limit.
    let watching = Rc::clone(&close);
    let deadline = std::time::Instant::now() + TIMEOUT;
    let watched = Rc::downgrade(&state);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let Some(state) = watched.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if state.borrow().reply.is_none() {
            return glib::ControlFlow::Break;
        }
        drop(state);
        if cancellation.is_cancelled() {
            watching(Outcome::Cancelled);
            return glib::ControlFlow::Break;
        }
        if std::time::Instant::now() >= deadline {
            watching(Outcome::TimedOut);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });

    let display = gdk::Display::default();
    let target = display.as_ref().and_then(|display| {
        let (x, y, _, _) = monitor?;
        (0..display.n_monitors()).find(|index| {
            display.monitor(*index).is_some_and(|m| {
                let geometry = m.geometry();
                geometry.x() == x && geometry.y() == y
            })
        })
    });
    window.show_all();
    match (target, gdk::Screen::default()) {
        (Some(index), Some(screen)) => window.fullscreen_on_monitor(&screen, index),
        _ => window.fullscreen(),
    }
    window.present();
    area.grab_focus();
}
