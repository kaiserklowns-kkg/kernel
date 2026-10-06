//! The Oceans app toolkit (ADR-0080): widgets for apps' windows, in the
//! desktop's look (ADR-0078) and with Thai text (ADR-0077).
//!
//! **Immediate mode:** each frame the app describes its screen by calling
//! the widgets in order (`ui.heading(..)`, `if ui.button(..) {..}`); a
//! widget draws itself and answers the input that landed on it. Nothing is
//! kept between frames but the app's own state, so an app is one function
//! from its state to its screen.
//!
//! [`run`] opens a window and drives the frames: a frame after every batch
//! of events, then once more so the screen shows what the input changed.
//! [`Ui`] works on any [`Surface`], so the toolkit is not tied to windows.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use oceans_display_proto::{Event, Window, events, kind};
pub use oceans_draw::{Rect, Rgb, Style, Surface, Typesetter};
use oceans_rt::Directory;

/// The look (ADR-0078).
pub mod colour {
    use super::Rgb;

    pub const WINDOW: Rgb = Rgb(0xf6_f6_f8);
    pub const SIDEBAR: Rgb = Rgb(0xe9_e9_ee);
    pub const SURFACE: Rgb = Rgb(0xff_ff_ff);
    pub const TEXT: Rgb = Rgb(0x1d_1d_1f);
    pub const MUTED: Rgb = Rgb(0x6e_6e_73);
    pub const ACCENT: Rgb = Rgb(0x2f_7c_f6);
    pub const ACCENT_PRESSED: Rgb = Rgb(0x52_93_f8);
    pub const BUTTON: Rgb = Rgb(0xe3_e3_e8);
    pub const BUTTON_HOVER: Rgb = Rgb(0xd5_d5_db);
    pub const SELECTED: Rgb = Rgb(0xd4_e3_fc);
    pub const LINE: Rgb = Rgb(0xd8_d8_de);
    pub const DANGER: Rgb = Rgb(0xff_3b_30);
    pub const SUCCESS: Rgb = Rgb(0x34_c7_59);
}

/// What arrived since the last frame.
#[derive(Default, Clone)]
pub struct Input {
    pub pointer: (i32, i32),
    /// A press of the main button, where it happened (taken by the widget
    /// under it).
    pub click: Option<(i32, i32)>,
    /// Keys typed (bytes: ASCII; `\r` Enter, `\x08`/`\x7f` Backspace,
    /// `\x1b` Escape).
    pub keys: Vec<u8>,
    pub focused: bool,
}

/// One frame's drawing and layout: a column of widgets from the top of
/// `area`, `padding` in from its edges.
pub struct Ui<'s, 'f> {
    pub surface: &'s mut Surface<'f>,
    pub typesetter: &'s mut Typesetter,
    pub input: &'s mut Input,
    /// Where widgets go; `y` moves down as they are laid out.
    pub area: Rect,
    pub y: i32,
    /// The text field with the keyboard (by its id), kept by the app.
    pub focus: &'s mut Option<u32>,
    /// Something changed the app's state: another frame is drawn.
    pub changed: bool,
}

const LINE_HEIGHT: i32 = 22;
const GAP: i32 = 8;
const BUTTON_HEIGHT: i32 = 30;
const ROW_HEIGHT: i32 = 32;

impl<'s, 'f> Ui<'s, 'f> {
    pub fn new(
        surface: &'s mut Surface<'f>,
        typesetter: &'s mut Typesetter,
        input: &'s mut Input,
        focus: &'s mut Option<u32>,
        area: Rect,
    ) -> Self {
        let y = area.y;
        Self {
            surface,
            typesetter,
            input,
            area,
            y,
            focus,
            changed: false,
        }
    }

    /// The rectangle of the next widget, `height` tall, across the area.
    fn next(&mut self, height: i32) -> Rect {
        let r = Rect::new(self.area.x, self.y, self.area.w, height);
        self.y += height + GAP;
        r
    }

    fn hovered(&self, r: Rect) -> bool {
        r.contains(self.input.pointer.0, self.input.pointer.1)
    }

    /// Whether the pending click is inside `r`; it is then taken.
    fn take_click(&mut self, r: Rect) -> bool {
        match self.input.click {
            Some((x, y)) if r.contains(x, y) => {
                self.input.click = None;
                self.changed = true;
                true
            }
            _ => false,
        }
    }

    pub fn measure(&mut self, text: &str, style: Style) -> i32 {
        self.typesetter.measure(text, style)
    }

    /// Text at (`x`, `y`), clipped to `clip`; the x after it.
    pub fn text_at(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        style: Style,
        colour: Rgb,
        clip: Rect,
    ) -> i32 {
        self.surface
            .text(self.typesetter, (x, y), text, style, colour, clip)
    }

    /// Fills the whole area (a frame's background).
    pub fn background(&mut self, colour: Rgb) {
        let r = self.area;
        self.surface.fill(r, colour);
    }

    /// Space below the last widget.
    pub fn space(&mut self, height: i32) {
        self.y += height;
    }

    pub fn heading(&mut self, text: &str) {
        let r = self.next(28);
        self.text_at(r.x, r.y + 2, text, Style::Title, colour::TEXT, r);
    }

    pub fn label(&mut self, text: &str) {
        let r = self.next(LINE_HEIGHT);
        self.text_at(r.x, r.y + 2, text, Style::Body, colour::TEXT, r);
    }

    pub fn muted(&mut self, text: &str) {
        let r = self.next(LINE_HEIGHT);
        self.text_at(r.x, r.y + 2, text, Style::Body, colour::MUTED, r);
    }

    /// A name and its value on one row, the value right-aligned, a line
    /// under it (lists of facts, as in Settings).
    pub fn row(&mut self, name: &str, value: &str) {
        let r = self.next(ROW_HEIGHT - GAP);
        self.text_at(r.x, r.y + 4, name, Style::Body, colour::TEXT, r);
        let width = self.measure(value, Style::Body);
        self.text_at(
            r.x + r.w - width,
            r.y + 4,
            value,
            Style::Body,
            colour::MUTED,
            r,
        );
        self.surface
            .fill(Rect::new(r.x, r.y + r.h + GAP / 2, r.w, 1), colour::LINE);
    }

    pub fn separator(&mut self) {
        let r = self.next(1);
        self.surface.fill(r, colour::LINE);
    }

    /// A button as wide as its text; `true` when clicked.
    pub fn button(&mut self, text: &str) -> bool {
        let width = self.measure(text, Style::Strong) + 28;
        let r = Rect::new(self.area.x, self.y, width, BUTTON_HEIGHT);
        self.y += BUTTON_HEIGHT + GAP;
        self.button_in(r, text, false)
    }

    /// A button in the accent; `true` when clicked.
    pub fn primary_button(&mut self, text: &str) -> bool {
        let width = self.measure(text, Style::Strong) + 28;
        let r = Rect::new(self.area.x, self.y, width, BUTTON_HEIGHT);
        self.y += BUTTON_HEIGHT + GAP;
        self.button_in(r, text, true)
    }

    /// A button at `r` (for grids and toolbars); `true` when clicked.
    pub fn button_in(&mut self, r: Rect, text: &str, primary: bool) -> bool {
        let clicked = self.take_click(r);
        let hovered = self.hovered(r);
        let fill = match (primary, hovered) {
            (true, false) => colour::ACCENT,
            (true, true) => colour::ACCENT_PRESSED,
            (false, false) => colour::BUTTON,
            (false, true) => colour::BUTTON_HOVER,
        };
        self.surface.round_fill(r, 8, fill);
        let width = self.measure(text, Style::Strong);
        let ink = if primary {
            Rgb(0xff_ff_ff)
        } else {
            colour::TEXT
        };
        self.text_at(
            r.x + (r.w - width) / 2,
            r.y + (r.h - 18) / 2,
            text,
            Style::Strong,
            ink,
            r,
        );
        clicked
    }

    /// A list of `items`, one row each, `selected` highlighted; returns the
    /// row clicked.
    pub fn list(&mut self, items: &[&str], selected: Option<usize>) -> Option<usize> {
        let mut clicked = None;
        for (i, item) in items.iter().enumerate() {
            let r = Rect::new(self.area.x, self.y, self.area.w, ROW_HEIGHT);
            self.y += ROW_HEIGHT + 2;
            if self.take_click(r) {
                clicked = Some(i);
            }
            if selected == Some(i) {
                self.surface.round_fill(r, 6, colour::SELECTED);
            } else if self.hovered(r) {
                self.surface.tint(r, 6, Rgb(0), 12);
            }
            self.text_at(r.x + 10, r.y + 6, item, Style::Body, colour::TEXT, r);
        }
        self.y += GAP;
        clicked
    }

    /// A one-line text field (`id` tells fields apart): a click gives it
    /// the keyboard. Returns `true` when Enter is pressed in it.
    pub fn text_field(&mut self, id: u32, text: &mut String, placeholder: &str) -> bool {
        let r = self.next(BUTTON_HEIGHT);
        if self.take_click(r) {
            *self.focus = Some(id);
        }
        let focused = *self.focus == Some(id) && self.input.focused;
        let mut entered = false;
        if focused {
            for key in core::mem::take(&mut self.input.keys) {
                match key {
                    b'\r' | b'\n' => entered = true,
                    0x08 | 0x7f => {
                        text.pop();
                    }
                    0x20..=0x7e if text.len() < 200 => text.push(char::from(key)),
                    _ => {}
                }
                self.changed = true;
            }
        }
        self.surface.round_fill(
            r,
            7,
            if focused {
                colour::ACCENT
            } else {
                colour::LINE
            },
        );
        let inner = Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2);
        self.surface.round_fill(inner, 6, colour::SURFACE);
        let (shown, ink) = if text.is_empty() {
            (placeholder, colour::MUTED)
        } else {
            (text.as_str(), colour::TEXT)
        };
        let clip = Rect::new(inner.x + 8, inner.y, inner.w - 16, inner.h);
        let end = self.text_at(inner.x + 8, inner.y + 5, shown, Style::Body, ink, clip);
        if focused {
            let x = if text.is_empty() {
                inner.x + 8
            } else {
                end + 1
            };
            self.surface
                .fill(Rect::new(x, inner.y + 6, 2, inner.h - 12), colour::ACCENT);
        }
        entered
    }

    /// A left sidebar of `items` (`width` wide, the full height); returns
    /// the item clicked. Widgets after it go to its right.
    pub fn sidebar(&mut self, width: i32, items: &[&str], selected: usize) -> Option<usize> {
        let bar = Rect::new(self.area.x, self.area.y, width, self.area.h);
        self.surface.fill(bar, colour::SIDEBAR);
        let mut clicked = None;
        for (i, item) in items.iter().enumerate() {
            let r = Rect::new(bar.x + 8, bar.y + 12 + i as i32 * 34, width - 16, 30);
            if self.take_click(r) {
                clicked = Some(i);
            }
            if i == selected {
                self.surface.round_fill(r, 6, colour::ACCENT);
            } else if self.hovered(r) {
                self.surface.tint(r, 6, Rgb(0), 14);
            }
            let ink = if i == selected {
                Rgb(0xff_ff_ff)
            } else {
                colour::TEXT
            };
            self.text_at(r.x + 10, r.y + 5, item, Style::Body, ink, r);
        }
        // The rest of the area, padded.
        self.area = Rect::new(
            bar.x + width + 24,
            self.area.y + 20,
            self.area.w - width - 48,
            self.area.h - 40,
        );
        self.y = self.area.y;
        clicked
    }
}

/// The exit codes `run` returns.
pub const EXIT_BAD_START: i64 = 2;
pub const EXIT_NO_WINDOW: i64 = 3;

/// Opens a `width × height` window titled `title` through the app's
/// `use windows` end and runs `frame` until the window is closed: after
/// every batch of events, and again when the frame says something
/// changed. `frame` gets the toolkit (laid out over the whole window) and
/// the app's state.
pub fn run<S>(
    directory: &Directory,
    title: &str,
    width: u16,
    height: u16,
    state: &mut S,
    mut frame: impl FnMut(&mut Ui<'_, '_>, &mut S),
) -> i64 {
    const EVENTS: u64 = 1;
    let (Some(windows), Ok(notification)) = (
        directory.find("use", "windows"),
        oceans_rt::notification_create(),
    ) else {
        return EXIT_NO_WINDOW;
    };
    let Ok(mut window) = Window::open(windows, notification, EVENTS, width, height, title) else {
        return EXIT_NO_WINDOW;
    };
    let Some(mut typesetter) = Typesetter::new() else {
        return EXIT_BAD_START;
    };
    let mut input = Input {
        focused: true,
        ..Input::default()
    };
    let mut focus = None;
    let mut batch = [Event::default(); 20];
    let mut draw =
        |window: &mut Window, input: &mut Input, focus: &mut Option<u32>, state: &mut S| {
            let (w, h) = (window.width, window.height);
            let Some(mut surface) = Surface::new(window.pixels(), w, h) else {
                return false;
            };
            let mut ui = Ui::new(
                &mut surface,
                &mut typesetter,
                input,
                focus,
                Rect::new(0, 0, w as i32, h as i32),
            );
            frame(&mut ui, state);
            let changed = ui.changed;
            let _ = window.present();
            changed
        };
    draw(&mut window, &mut input, &mut focus, state);
    loop {
        if oceans_rt::notification_wait(notification).is_err() {
            return EXIT_BAD_START;
        }
        loop {
            let count = match events(windows, &mut batch) {
                Ok(0) => break,
                Ok(count) => count,
                Err(_) => return EXIT_NO_WINDOW,
            };
            for event in &batch[..count] {
                match event.kind {
                    kind::POINTER => input.pointer = (i32::from(event.x), i32::from(event.y)),
                    kind::BUTTON if event.button == 1 && event.pressed => {
                        input.pointer = (i32::from(event.x), i32::from(event.y));
                        input.click = Some(input.pointer);
                    }
                    kind::KEY => input.keys.push(event.key),
                    kind::FOCUS => input.focused = event.pressed,
                    kind::CLOSE => {
                        let _ = window.close();
                        return 0;
                    }
                    _ => {}
                }
            }
        }
        // Once with the input, and again (input spent) to show its effect.
        if draw(&mut window, &mut input, &mut focus, state) {
            input.click = None;
            input.keys.clear();
            draw(&mut window, &mut input, &mut focus, state);
        }
        input.click = None;
        input.keys.clear();
    }
}
