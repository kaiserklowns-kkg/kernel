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
//!
//! **The clipboard** (ADR-0095): what the user pastes arrives in
//! [`Input::paste`]; text fields take it, and copy and cut all of their
//! text. An app copies with [`Ui::copy`], in answer to the user's keys.
//!
//! **The keyboard reaches every widget** (ADR-0106): each one drawn is
//! recorded with its [`Role`] and name in [`Ui::tree`]. Tab and Shift+Tab
//! move the keyboard's focus through what can be used, in the order drawn;
//! the focused widget shows a ring in the accent. Enter or Space presses
//! a focused button; Up, Down, Home and End choose a focused list's row,
//! and Enter opens it, as a second click would. An area the app draws and
//! answers keys in itself joins with [`Ui::focusable`].

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use oceans_abi::display::{is_copy, is_cut};
pub use oceans_access::Role;
use oceans_access::{Ids, Node, Tree, focus_move, list_key};
use oceans_display_proto::{Event, Window, events, kind};
pub use oceans_draw::{Path, Rect, Rgb, Scale, Style, Surface, Typesetter, icon, layout, theme};
use oceans_rt::Directory;

/// The look (ADR-0078): the colours of the theme the window draws with
/// (ADR-0113), light or dark, in the user's accent. Functions, not
/// constants: the theme changes while the app runs.
pub mod colour {
    use super::Rgb;
    use oceans_draw::theme;

    macro_rules! colours {
        ($($name:ident),*) => {
            $(pub fn $name() -> Rgb { theme::current().$name })*
        };
    }

    colours!(
        window,
        sidebar,
        surface,
        text,
        muted,
        accent,
        accent_pressed,
        on_accent,
        button,
        button_hover,
        selected,
        line,
        danger,
        success
    );
}

/// What arrived since the last frame.
#[derive(Default, Clone)]
pub struct Input {
    pub pointer: (i32, i32),
    /// A press of the main button, where it happened (taken by the widget
    /// under it).
    pub click: Option<(i32, i32)>,
    /// The main button is held down (a drag, while `pointer` moves).
    pub held: bool,
    /// Keys typed (bytes: ASCII; `\r` Enter, `\x08`/`\x7f` Backspace,
    /// `\x1b` Escape).
    pub keys: Vec<u8>,
    pub focused: bool,
    /// Text the user pasted (ADR-0095), for the widget with the keyboard.
    pub paste: Option<String>,
    /// Characters typed in a layout other than English (ADR-0114), for
    /// the widget with the keyboard; printable keys come here instead of
    /// `keys` then.
    pub text: Vec<char>,
    /// Wheel detents turned over the window (ADR-0138): down the page,
    /// and to the right; `pointer` is where it turned. Taken by the
    /// [`Scroll`] view under the pointer.
    pub wheel: (i32, i32),
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
    /// Text to put on the clipboard after this frame ([`Ui::copy`]).
    pub copied: Option<String>,
    /// The window end, for opening files (ADR-0099); `None` off a window.
    pub windows: Option<oceans_rt::Handle>,
    /// The window drawn into (its id), with `windows`.
    pub window: u32,
    /// What this frame drew, in order (ADR-0106).
    pub tree: Tree,
    ids: Ids,
    /// Tab (`false`) and Shift+Tab (`true`) pressed since the last frame,
    /// taken out of the keys: [`Ui::finish`] moves the focus.
    moves: Vec<bool>,
    /// The part of a scrolled view that shows (ADR-0138): widgets outside
    /// it are neither pointed at nor clicked.
    visible: Option<Rect>,
}

/// Where a view that scrolls is (ADR-0138), kept by the app from frame to
/// frame: how far down its content it shows, and how tall that content
/// was last laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scroll {
    pub offset: i32,
    content: i32,
}

impl Scroll {
    /// Back to the top (another page shown in the view).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A scrolled view being laid out: what [`Ui::end_scroll`] needs back.
pub struct ScrollView {
    view: Rect,
    top: i32,
    area: Rect,
    clip: Option<Rect>,
    visible: Option<Rect>,
    nodes: usize,
}

/// How far one detent of the wheel scrolls: three lines of text.
const WHEEL_STEP: i32 = 3 * LINE_HEIGHT;
/// The scroll bar's width, at the view's right edge.
const SCROLL_BAR: i32 = 6;

/// The most a text field holds, in bytes.
const FIELD_MAX: usize = 200;
const LINE_HEIGHT: i32 = 22;
const GAP: i32 = 8;
const BUTTON_HEIGHT: i32 = 30;
const ROW_HEIGHT: i32 = 32;
/// What the desktop keeps of the screen's height for itself: the top bar
/// and the dock's band (`desktop::MENU_HEIGHT` and `DOCK_RESERVE`).
const DESKTOP_BARS: i32 = 28 + 80;

impl<'s, 'f> Ui<'s, 'f> {
    pub fn new(
        surface: &'s mut Surface<'f>,
        typesetter: &'s mut Typesetter,
        input: &'s mut Input,
        focus: &'s mut Option<u32>,
        area: Rect,
    ) -> Self {
        let y = area.y;
        // Tab and Shift+Tab are the toolkit's, not a widget's or the app's.
        let mut moves = Vec::new();
        input.keys.retain(|&key| match focus_move(key) {
            Some(back) => {
                moves.push(back);
                false
            }
            None => true,
        });
        Self {
            surface,
            typesetter,
            input,
            area,
            y,
            focus,
            changed: false,
            copied: None,
            windows: None,
            window: 0,
            tree: Tree::default(),
            ids: Ids::default(),
            moves,
            visible: None,
        }
    }

    /// Lays what follows out in `view`, scrolled as `scroll` says
    /// (ADR-0138): widgets go on down the page from its top as usual, and
    /// show only inside `view`. End it with [`Ui::end_scroll`].
    pub fn begin_scroll(&mut self, view: Rect, scroll: &Scroll) -> ScrollView {
        let started = ScrollView {
            view,
            top: view.y - scroll.offset,
            area: self.area,
            clip: self.surface.clip_area(),
            visible: self.visible,
            nodes: self.tree.nodes().len(),
        };
        self.surface.clip_to(Some(view));
        self.visible = Some(view);
        // Room for the bar only when there is more than the view shows.
        let bar = if scroll.content > view.h {
            SCROLL_BAR + 6
        } else {
            0
        };
        self.area = Rect::new(view.x, view.y, view.w - bar, view.h);
        self.y = started.top;
        started
    }

    /// Ends a scrolled view: the wheel turned over it scrolls it, the
    /// widget with the keyboard is scrolled into sight, the offset stays
    /// within the content, and a bar shows where the view is when there is
    /// more than it shows. Layout goes on under the view.
    pub fn end_scroll(&mut self, started: ScrollView, scroll: &mut Scroll) {
        let view = started.view;
        scroll.content = self.y - started.top;
        let most = (scroll.content - view.h).max(0);
        let before = scroll.offset;
        let (px, py) = self.input.pointer;
        if self.input.wheel.0 != 0 && view.contains(px, py) {
            scroll.offset += self.input.wheel.0 * WHEEL_STEP;
            self.input.wheel.0 = 0;
        }
        // The keyboard's widget, if it is in this view, in sight.
        if let Some(focus) = *self.focus
            && let Some(node) = self.tree.nodes()[started.nodes..]
                .iter()
                .find(|n| n.id == focus && n.focusable)
        {
            let (top, height) = (node.rect.1 - started.top, node.rect.3);
            if top < scroll.offset {
                scroll.offset = top;
            } else if top + height > scroll.offset + view.h {
                scroll.offset = top + height - view.h;
            }
        }
        scroll.offset = scroll.offset.clamp(0, most);
        if scroll.offset != before {
            self.changed = true;
        }
        if most > 0 {
            let track = view.h - 8;
            let thumb = (track * view.h / scroll.content).max(24).min(track);
            let at = (track - thumb) * scroll.offset / most;
            let bar = Rect::new(
                view.x + view.w - SCROLL_BAR - 2,
                view.y + 4 + at,
                SCROLL_BAR,
                thumb,
            );
            self.surface
                .round_fill(bar, SCROLL_BAR / 2, colour::muted());
        }
        self.surface.clip_to(started.clip);
        self.visible = started.visible;
        self.area = started.area;
        self.y = view.y + view.h + GAP;
    }

    /// Ends the frame: Tab and Shift+Tab move the focus through what this
    /// frame drew (ADR-0106). The window loop calls it after the frame.
    pub fn finish(&mut self) {
        if !self.input.focused {
            self.moves.clear();
            return;
        }
        for back in core::mem::take(&mut self.moves) {
            if let Some(next) = self.tree.next_focus(*self.focus, back) {
                *self.focus = Some(next);
                self.changed = true;
            }
        }
    }

    /// Records a widget of this frame (ADR-0106).
    fn node(&mut self, id: u32, role: Role, name: &str, r: Rect, focusable: bool) {
        self.tree.push(Node {
            id,
            role,
            name: String::from(name),
            rect: (r.x, r.y, r.w, r.h),
            focusable,
        });
    }

    /// Records a widget the app does not name, and gives its id.
    fn auto(&mut self, role: Role, name: &str, r: Rect, focusable: bool) -> u32 {
        let id = self.ids.id(role, name);
        self.node(id, role, name, r, focusable);
        id
    }

    /// An area the app draws and answers keys in itself (a page, a
    /// picture), with the app's own focus id: Tab reaches it, and it has
    /// the keyboard while `*ui.focus == Some(id)`. The app shows that focus
    /// itself (a cursor, a selection): no ring is drawn round it.
    pub fn focusable(&mut self, id: u32, role: Role, name: &str, r: Rect) {
        self.node(id, role, name, r, true);
    }

    /// Whether widget `id` has the keyboard.
    pub fn has_focus(&self, id: u32) -> bool {
        *self.focus == Some(id) && self.input.focused
    }

    /// The first of `keys` pressed for the focused widget `id`, taken.
    fn take_key(&mut self, id: u32, keys: &[u8]) -> Option<u8> {
        if !self.has_focus(id) {
            return None;
        }
        let at = self.input.keys.iter().position(|key| keys.contains(key))?;
        self.changed = true;
        Some(self.input.keys.remove(at))
    }

    /// The ring that marks the keyboard's focus, just outside `r`.
    fn focus_ring(&mut self, r: Rect) {
        for grow in [2, 3] {
            let ring = Rect::new(r.x - grow, r.y - grow, r.w + 2 * grow, r.h + 2 * grow);
            self.surface.outline(ring, 8 + grow, colour::accent(), 255);
        }
    }

    /// Says what this window plays (ADR-0103), for the desktop's sound
    /// panel: `state` from `oceans_display_proto::proto::playing`, and a
    /// title. Only after [`Ui::want_media_keys`]; say it when it changes.
    pub fn now_playing(&self, state: u8, title: &str) {
        if let Some(windows) = self.windows {
            let _ = oceans_display_proto::now_playing(windows, self.window, state, title);
        }
    }

    /// The keyboard's media keys (Play/Pause, Stop, Previous, Next;
    /// ADR-0102) come to this window's keys from now on, whatever has the
    /// focus: for a player. Ask once.
    pub fn want_media_keys(&self) {
        if let Some(windows) = self.windows {
            let _ = oceans_display_proto::want_media_keys(windows, self.window);
        }
    }

    /// The screen the window is on (ADR-0136): its pixels, the scale in
    /// use and the one automatic chooses. `None` off a window.
    pub fn screen(&self) -> Option<oceans_display_proto::Screen> {
        oceans_display_proto::screen(self.windows?).ok()
    }

    /// The apps that open files like `name` (ADR-0099), as id and name, the
    /// one [`Ui::open_file`] would choose first.
    pub fn openers(&self, name: &str) -> Vec<(String, String)> {
        let mut found = Vec::new();
        if let Some(windows) = self.windows {
            let _ = oceans_display_proto::openers(windows, name, |id, app| {
                found.push((String::from(id), String::from(app)));
            });
        }
        found
    }

    /// Opens the file `name` of Home in the app `app`, or in the one that
    /// opens its kind (ADR-0099). Call it for the user's click or key: the
    /// system refuses an open the user did not ask for.
    pub fn open_file(&self, app: Option<&str>, name: &str) -> Result<(), &'static str> {
        let windows = self.windows.ok_or("This window cannot open files.")?;
        oceans_display_proto::open_file(windows, app, name).map_err(|error| match error {
            oceans_display_proto::WindowError::Refused(oceans_display_proto::Status::NotFound) => {
                "No app opens this kind of file."
            }
            _ => "The file could not be opened.",
        })
    }

    /// Puts `text` on the clipboard when the frame ends (ADR-0095). Call
    /// it for the user's Ctrl+C or Ctrl+X: the system refuses a copy the
    /// user did not ask for.
    pub fn copy(&mut self, text: &str) {
        if !text.is_empty() {
            self.copied = Some(String::from(text));
        }
    }

    /// The rectangle of the next widget, `height` tall, across the area.
    fn next(&mut self, height: i32) -> Rect {
        let r = Rect::new(self.area.x, self.y, self.area.w, height);
        self.y += height + GAP;
        r
    }

    /// Whether (`x`, `y`) shows: inside the scrolled view being laid out,
    /// if any (ADR-0138).
    fn shows(&self, x: i32, y: i32) -> bool {
        self.visible.is_none_or(|v| v.contains(x, y))
    }

    fn hovered(&self, r: Rect) -> bool {
        let (x, y) = self.input.pointer;
        r.contains(x, y) && self.shows(x, y)
    }

    /// Whether the pending click is inside `r`; it is then taken.
    fn take_click(&mut self, r: Rect) -> bool {
        match self.input.click {
            Some((x, y)) if r.contains(x, y) && self.shows(x, y) => {
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
        self.auto(Role::Heading, text, r, false);
        self.text_at(r.x, r.y + 2, text, Style::Title, colour::text(), r);
    }

    pub fn label(&mut self, text: &str) {
        let r = self.next(LINE_HEIGHT);
        self.auto(Role::Label, text, r, false);
        self.text_at(r.x, r.y + 2, text, Style::Body, colour::text(), r);
    }

    pub fn muted(&mut self, text: &str) {
        let r = self.next(LINE_HEIGHT);
        self.auto(Role::Label, text, r, false);
        self.text_at(r.x, r.y + 2, text, Style::Body, colour::muted(), r);
    }

    /// A name and its value on one row, the value right-aligned, a line
    /// under it (lists of facts, as in Settings).
    pub fn row(&mut self, name: &str, value: &str) {
        let r = self.next(ROW_HEIGHT - GAP);
        let said = alloc::format!("{name}: {value}");
        self.auto(Role::Label, &said, r, false);
        // The value keeps the right end; the name what is left of it
        // (ADR-0137), so a narrow window clips them apart.
        let width = self.measure(value, Style::Body).min(r.w * 2 / 3);
        let name_clip = Rect::new(r.x, r.y, r.w - width - 12, r.h);
        self.text_at(r.x, r.y + 4, name, Style::Body, colour::text(), name_clip);
        let value_clip = Rect::new(r.x + r.w - width, r.y, width, r.h);
        self.text_at(
            r.x + r.w - width,
            r.y + 4,
            value,
            Style::Body,
            colour::muted(),
            value_clip,
        );
        self.surface
            .fill(Rect::new(r.x, r.y + r.h + GAP / 2, r.w, 1), colour::line());
    }

    pub fn separator(&mut self) {
        let r = self.next(1);
        self.surface.fill(r, colour::line());
    }

    /// How wide a button with `text` is drawn ([`Ui::button`]): for laying
    /// out toolbars and rows of them ([`layout::Bar`], [`layout::Flow`]).
    pub fn button_width(&mut self, text: &str) -> i32 {
        self.measure(text, Style::Strong) + 28
    }

    /// The area at most `max` wide from its left (ADR-0137): text and
    /// rows of facts stay readable in a wide window instead of spreading
    /// across it.
    pub fn readable(&mut self, max: i32) {
        self.area.w = self.area.w.min(max);
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

    /// A button at `r` (for grids and toolbars); `true` when clicked, or
    /// pressed with Enter or Space while it has the keyboard.
    pub fn button_in(&mut self, r: Rect, text: &str, primary: bool) -> bool {
        let id = self.auto(Role::Button, text, r, true);
        let clicked = self.take_click(r) || self.take_key(id, b"\r ").is_some();
        if self.has_focus(id) {
            self.focus_ring(r);
        }
        let hovered = self.hovered(r);
        let fill = match (primary, hovered) {
            (true, false) => colour::accent(),
            (true, true) => colour::accent_pressed(),
            (false, false) => colour::button(),
            (false, true) => colour::button_hover(),
        };
        self.surface.round_fill(r, 8, fill);
        let width = self.measure(text, Style::Strong);
        let ink = if primary {
            colour::on_accent()
        } else {
            colour::text()
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
    /// row clicked. With the keyboard, Up, Down, Home and End return the
    /// row they move to, and Enter the selected row (as a second click).
    pub fn list(&mut self, items: &[&str], selected: Option<usize>) -> Option<usize> {
        let height = items.len() as i32 * (ROW_HEIGHT + 2);
        let whole = Rect::new(self.area.x, self.y, self.area.w, height.max(ROW_HEIGHT));
        let id = self.auto(Role::List, "", whole, !items.is_empty());
        let mut clicked = self.list_keys(id, selected, items.len());
        let focused = self.has_focus(id);
        for (i, item) in items.iter().enumerate() {
            let r = Rect::new(self.area.x, self.y, self.area.w, ROW_HEIGHT);
            self.y += ROW_HEIGHT + 2;
            if self.take_click(r) {
                clicked = Some(i);
            }
            if selected == Some(i) {
                self.surface.round_fill(r, 6, colour::selected());
                if focused {
                    self.focus_ring(r);
                }
            } else if self.hovered(r) {
                self.surface.tint(r, 6, Rgb(0), 12);
            }
            self.text_at(r.x + 10, r.y + 6, item, Style::Body, colour::text(), r);
        }
        // Focused with nothing selected: the ring round the whole list.
        if focused && selected.is_none_or(|i| i >= items.len()) {
            self.focus_ring(whole);
        }
        self.y += GAP;
        clicked
    }

    /// The row the keys pressed for list `id` choose (see [`Ui::list`]).
    fn list_keys(&mut self, id: u32, selected: Option<usize>, count: usize) -> Option<usize> {
        use oceans_abi::display::{KEY_DOWN, KEY_END, KEY_HOME, KEY_UP};
        let mut row = None;
        while let Some(key) = self.take_key(id, &[KEY_UP, KEY_DOWN, KEY_HOME, KEY_END, b'\r']) {
            let from = row.or(selected);
            row = if key == b'\r' {
                from.filter(|&i| i < count)
            } else {
                list_key(key, from, count).or(row)
            };
        }
        row
    }

    /// A one-line text field (`id` tells fields apart): a click gives it
    /// the keyboard. Returns `true` when Enter is pressed in it.
    pub fn text_field(&mut self, id: u32, text: &mut String, placeholder: &str) -> bool {
        self.field(id, text, placeholder, false)
    }

    /// A text field for a password (ADR-0111): a dot for each character,
    /// and it cannot be copied or cut (pasting into it works).
    pub fn password_field(&mut self, id: u32, text: &mut String, placeholder: &str) -> bool {
        self.field(id, text, placeholder, true)
    }

    fn field(&mut self, id: u32, text: &mut String, placeholder: &str, secret: bool) -> bool {
        let r = self.next(BUTTON_HEIGHT);
        self.node(id, Role::Field, placeholder, r, true);
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
                    0x20..=0x7e if text.len() < FIELD_MAX => text.push(char::from(key)),
                    // A field has no selection: Ctrl+C copies all of it,
                    // Ctrl+X cuts all of it (ADR-0095).
                    key if !secret && (is_copy(key) || is_cut(key)) => {
                        self.copied = (!text.is_empty()).then(|| text.clone());
                        if is_cut(key) {
                            text.clear();
                        }
                    }
                    _ => {}
                }
                self.changed = true;
            }
            // Typed in another layout (ADR-0114): Thai, for one.
            for c in core::mem::take(&mut self.input.text) {
                if !c.is_control() && text.len() + c.len_utf8() <= FIELD_MAX {
                    text.push(c);
                }
                self.changed = true;
            }
            // Pasted: its first line, as far as the field goes.
            if let Some(pasted) = self.input.paste.take() {
                for c in pasted.lines().next().unwrap_or("").chars() {
                    if c.is_control() || text.len() + c.len_utf8() > FIELD_MAX {
                        continue;
                    }
                    text.push(c);
                }
                self.changed = true;
            }
        }
        self.surface.round_fill(
            r,
            7,
            if focused {
                colour::accent()
            } else {
                colour::line()
            },
        );
        let inner = Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2);
        self.surface.round_fill(inner, 6, colour::surface());
        let (shown, ink) = if text.is_empty() {
            (placeholder, colour::muted())
        } else {
            (text.as_str(), colour::text())
        };
        let clip = Rect::new(inner.x + 8, inner.y, inner.w - 16, inner.h);
        let end = if secret && !text.is_empty() {
            // A dot for each character, never the text.
            let dots = text.chars().count().min(40) as i32;
            for i in 0..dots {
                self.surface.circle(
                    inner.x + 12 + i * 12,
                    inner.y + inner.h / 2,
                    4,
                    colour::text(),
                );
            }
            inner.x + 12 + dots * 12 - 6
        } else {
            self.text_at(inner.x + 8, inner.y + 5, shown, Style::Body, ink, clip)
        };
        if focused {
            let x = if text.is_empty() {
                inner.x + 8
            } else {
                end + 1
            };
            self.surface
                .fill(Rect::new(x, inner.y + 6, 2, inner.h - 12), colour::accent());
        }
        entered
    }

    /// A left sidebar of `items` (`width` wide, the full height); returns
    /// the item clicked. Widgets after it go to its right.
    /// With the keyboard, Up, Down, Home and End choose an item.
    pub fn sidebar(&mut self, width: i32, items: &[&str], selected: usize) -> Option<usize> {
        let bar = Rect::new(self.area.x, self.area.y, width, self.area.h);
        self.surface.fill(bar, colour::sidebar());
        let id = self.auto(Role::List, "Sidebar", bar, !items.is_empty());
        let mut clicked = self
            .list_keys(id, Some(selected), items.len())
            .filter(|&i| i != selected);
        let focused = self.has_focus(id);
        for (i, item) in items.iter().enumerate() {
            let r = Rect::new(bar.x + 8, bar.y + 12 + i as i32 * 34, width - 16, 30);
            if self.take_click(r) {
                clicked = Some(i);
            }
            if i == selected {
                self.surface.round_fill(r, 6, colour::accent());
                if focused {
                    self.focus_ring(r);
                }
            } else if self.hovered(r) {
                self.surface.tint(r, 6, Rgb(0), 14);
            }
            let ink = if i == selected {
                colour::on_accent()
            } else {
                colour::text()
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
    frame: impl FnMut(&mut Ui<'_, '_>, &mut S),
) -> i64 {
    run_ticking(directory, title, width, height, None, state, frame)
}

/// As [`run`], and also a frame every `tick_ms` (for apps that show what
/// changes by itself: a clock, the processes).
pub fn run_ticking<S>(
    directory: &Directory,
    title: &str,
    width: u16,
    height: u16,
    tick_ms: Option<u64>,
    state: &mut S,
    frame: impl FnMut(&mut Ui<'_, '_>, &mut S),
) -> i64 {
    run_window(directory, title, width, height, None, tick_ms, state, frame)
}

/// As [`run_ticking`], in a window the user may resize and maximize
/// (ADR-0097), down to `min` (width, height). The frame is laid out over
/// the whole window whatever its size: lay out from `ui.area`.
#[allow(clippy::too_many_arguments)]
pub fn run_resizable<S>(
    directory: &Directory,
    title: &str,
    width: u16,
    height: u16,
    min: (u16, u16),
    tick_ms: Option<u64>,
    state: &mut S,
    frame: impl FnMut(&mut Ui<'_, '_>, &mut S),
) -> i64 {
    run_window(
        directory,
        title,
        width,
        height,
        Some(min),
        tick_ms,
        state,
        frame,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_window<S>(
    directory: &Directory,
    title: &str,
    width: u16,
    height: u16,
    min: Option<(u16, u16)>,
    tick_ms: Option<u64>,
    state: &mut S,
    mut frame: impl FnMut(&mut Ui<'_, '_>, &mut S),
) -> i64 {
    const EVENTS: u64 = 1;
    const TICK: u64 = 2;
    let (Some(windows), Ok(notification)) = (
        directory.find("use", "windows"),
        oceans_rt::notification_create(),
    ) else {
        return EXIT_NO_WINDOW;
    };
    // A window that can be resized opens as much larger as the screen has
    // room for (ADR-0137); one of a fixed size keeps it.
    let (width, height) = match (min, oceans_display_proto::screen(windows)) {
        (Some(min), Ok(screen)) => {
            let fit = |pixels: u16| screen.scale.logical_fit(i32::from(pixels));
            let area = (fit(screen.width), fit(screen.height) - DESKTOP_BARS);
            let (w, h) = oceans_draw::layout::window_size(
                (i32::from(width), i32::from(height)),
                (i32::from(min.0), i32::from(min.1)),
                area,
            );
            let limit = |v: i32, max: u16| v.clamp(1, i32::from(max)) as u16;
            (
                limit(w, oceans_display_proto::proto::MAX_WIDTH),
                limit(h, oceans_display_proto::proto::MAX_HEIGHT),
            )
        }
        _ => (width, height),
    };
    let Ok(mut window) = Window::open(windows, notification, EVENTS, width, height, title) else {
        return EXIT_NO_WINDOW;
    };
    if let Some((min_width, min_height)) = min {
        // Without it the window keeps its size: the app still works.
        let _ = window.set_resizable(min_width, min_height);
    }
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
            let (w, h, id, scale) = (window.width, window.height, window.id, window.scale);
            // Laid out in logical pixels, drawn at the scale (ADR-0136).
            let Some(mut surface) = Surface::scaled(window.pixels(), w, h, scale) else {
                return false;
            };
            typesetter.set_scale(scale);
            let area = Rect::new(0, 0, surface.width, surface.height);
            let mut ui = Ui::new(&mut surface, &mut typesetter, input, focus, area);
            ui.windows = Some(windows);
            ui.window = id;
            frame(&mut ui, state);
            ui.finish();
            let changed = ui.changed;
            let copied = ui.copied.take();
            let _ = window.present();
            if let Some(text) = copied {
                let _ = oceans_display_proto::copy(windows, &text);
            }
            changed
        };
    draw(&mut window, &mut input, &mut focus, state);
    if let Some(ms) = tick_ms {
        let _ = oceans_rt::timer_set(notification, TICK, ms);
    }
    loop {
        let Ok(bits) = oceans_rt::notification_wait(notification) else {
            return EXIT_BAD_START;
        };
        if bits & TICK != 0
            && let Some(ms) = tick_ms
        {
            let _ = oceans_rt::timer_set(notification, TICK, ms);
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
                        input.held = true;
                    }
                    kind::BUTTON if event.button == 1 => input.held = false,
                    kind::KEY => input.keys.push(event.key),
                    kind::TEXT => input.text.extend(event.character()),
                    // The wheel (ADR-0138), where it turned: for the view
                    // under the pointer, focused or not.
                    kind::WHEEL => {
                        if let Some((down, right)) = event.wheel() {
                            input.wheel.0 += down;
                            input.wheel.1 += right;
                            input.pointer = (i32::from(event.x), i32::from(event.y));
                        }
                    }
                    // New pixels at the new size; the frame after this
                    // batch draws into them.
                    kind::RESIZE => {
                        let _ = window.resize();
                    }
                    // The theme (ADR-0113): the frame after this batch is
                    // drawn in it.
                    kind::THEME => oceans_draw::theme::set(
                        oceans_draw::theme::Appearance::from_byte(event.key),
                        event.button,
                    ),
                    kind::PASTE => {
                        input.paste =
                            oceans_display_proto::paste(windows, |text: &str| String::from(text))
                                .ok();
                    }
                    kind::FOCUS => {
                        input.focused = event.pressed;
                        input.held = false;
                    }
                    kind::CLOSE => {
                        let _ = window.close();
                        return 0;
                    }
                    _ => {}
                }
            }
        }
        // Keys typed after Tab or Shift+Tab are for the widget it moves
        // to: they wait for the frame that moves the focus (ADR-0137), so
        // Tab then End reaches the sidebar however quickly they came.
        let mut later = Vec::new();
        loop {
            if let Some(at) = input.keys.iter().position(|&key| focus_move(key).is_some()) {
                later = input.keys.split_off(at + 1);
            }
            // Once with the input, and again (input spent) to show its
            // effect.
            if draw(&mut window, &mut input, &mut focus, state) {
                input.click = None;
                input.keys.clear();
                input.paste = None;
                input.text.clear();
                input.wheel = (0, 0);
                draw(&mut window, &mut input, &mut focus, state);
            }
            input.click = None;
            input.keys.clear();
            input.paste = None;
            input.text.clear();
            input.wheel = (0, 0);
            if later.is_empty() {
                break;
            }
            input.keys = core::mem::take(&mut later);
        }
    }
}
