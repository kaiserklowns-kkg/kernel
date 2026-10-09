//! Oceans windows (ADR-0059), host-tested.
//!
//! - [`proto`]: the window protocol's wire format, between apps and the
//!   display service (requests, statuses, events, limits).
//! - [`Manager`]: the window manager's state, apart from drawing: where
//!   windows are, which is on top, which has the keyboard focus (a window
//!   or the Terminal), dragging by the title bar, the minimize and close
//!   buttons (ADR-0076), each app's queue of events, and the clipboard
//!   (ADR-0095): who may copy, who pasted. The display service draws what
//!   it describes and moves bytes and pixels; the decisions are made here.
//!
//! Apps are untrusted: every request is bounded, and an app only ever
//! reaches its own windows and events.

#![no_std]

extern crate alloc;

pub mod proto;

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use proto::{Event, MAX_WINDOWS_PER_APP, Status, kind};

/// Height of the title bar the system draws above each window.
pub const TITLE_HEIGHT: i32 = 28;
/// Side of a title bar button's target (ADR-0078: round buttons at the
/// left of the title bar, close first).
pub const CLOSE_SIZE: i32 = 20;
/// From one title bar button to the next.
pub const BUTTON_STEP: i32 = 22;
/// Windows on the screen at once, of all apps.
pub const MAX_WINDOWS: usize = 16;
/// Events kept per app until it takes them; later ones are dropped.
pub const MAX_QUEUED: usize = 64;
/// The most one paste types into the Terminal (the shell's line is 200).
pub const TERMINAL_PASTE_MAX: usize = 200;
/// Offset between windows placed one after another.
const CASCADE: i32 = 32;

/// A rectangle in screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// A window, as the manager knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: u32,
    /// The app's connection (its window end's badge).
    pub owner: u64,
    /// The app's name, as Oceans Core verified it.
    pub app: String,
    /// The app's own title for the window.
    pub title: String,
    /// The frame's top left corner.
    pub x: i32,
    pub y: i32,
    /// The content's size.
    pub width: i32,
    pub height: i32,
    /// The app has presented its pixels at least once.
    pub presented: bool,
    /// Hidden until restored from the taskbar (ADR-0076).
    pub minimized: bool,
    /// The smallest content the app can lay out, once it said it can be
    /// resized (ADR-0097); `None`: its size is fixed.
    pub min: Option<(i32, i32)>,
    /// Where it was before it was maximized (`x`, `y`, `width`,
    /// `height`): the zoom button and a double click put it back.
    pub restore: Option<(i32, i32, i32, i32)>,
}

impl Frame {
    /// Everything the window covers: border, title bar and content.
    pub fn outer(&self) -> Rect {
        Rect::new(
            self.x,
            self.y,
            self.width + 2,
            self.height + TITLE_HEIGHT + 1,
        )
    }

    pub fn title_bar(&self) -> Rect {
        Rect::new(self.x, self.y, self.width + 2, TITLE_HEIGHT)
    }

    /// At the left of the title bar (ADR-0078).
    pub fn close_button(&self) -> Rect {
        Rect::new(
            self.x + 6,
            self.y + (TITLE_HEIGHT - CLOSE_SIZE) / 2,
            CLOSE_SIZE,
            CLOSE_SIZE,
        )
    }

    /// Right of the close button.
    pub fn minimize_button(&self) -> Rect {
        let close = self.close_button();
        Rect::new(close.x + BUTTON_STEP, close.y, CLOSE_SIZE, CLOSE_SIZE)
    }

    /// Right of the minimize button: maximizes and restores a window that
    /// can be resized (ADR-0097); drawn disabled on one that cannot.
    pub fn zoom_button(&self) -> Rect {
        let minimize = self.minimize_button();
        Rect::new(minimize.x + BUTTON_STEP, minimize.y, CLOSE_SIZE, CLOSE_SIZE)
    }

    /// Where the app's pixels go.
    pub fn content(&self) -> Rect {
        Rect::new(self.x + 1, self.y + TITLE_HEIGHT, self.width, self.height)
    }

    pub fn resizable(&self) -> bool {
        self.min.is_some()
    }

    /// The edges (`edge::*`) a press at `x`, `y` would drag: within
    /// [`GRIP`] outside the frame, or on its border; never the title bar.
    pub fn edges_at(&self, x: i32, y: i32) -> u8 {
        if !self.resizable() || self.minimized {
            return 0;
        }
        let o = self.outer();
        let (right, bottom) = (o.x + o.w, o.y + o.h);
        if x < o.x - GRIP || x >= right + GRIP || y < o.y - GRIP || y >= bottom + GRIP {
            return 0;
        }
        let mut edges = 0;
        if x < o.x + 1 {
            edges |= edge::LEFT;
        } else if x >= right - 1 {
            edges |= edge::RIGHT;
        }
        if y < o.y {
            edges |= edge::TOP;
        } else if y >= bottom - 1 {
            edges |= edge::BOTTOM;
        }
        // Near a corner, both edges: the corner is easier to catch.
        if edges & (edge::TOP | edge::BOTTOM) != 0 {
            if x < o.x + CORNER {
                edges |= edge::LEFT;
            } else if x >= right - CORNER {
                edges |= edge::RIGHT;
            }
        }
        if edges & (edge::LEFT | edge::RIGHT) != 0 && y >= bottom - CORNER {
            edges |= edge::BOTTOM;
        }
        edges
    }
}

/// The edges a resize drags (bits).
pub mod edge {
    pub const LEFT: u8 = 1 << 0;
    pub const RIGHT: u8 = 1 << 1;
    pub const TOP: u8 = 1 << 2;
    pub const BOTTOM: u8 = 1 << 3;
}

/// How far outside a resizable window's frame its edges can be caught.
pub const GRIP: i32 = 5;
/// How far along an edge from a corner a press takes the corner.
const CORNER: i32 = 14;

/// A resize under way: the window, its edges, where the press was and the
/// frame (`x`, `y`, `width`, `height`) then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Resize {
    id: u32,
    edges: u8,
    from: (i32, i32),
    start: (i32, i32, i32, i32),
}

/// Who gets the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The Terminal: keys go to the console (the shell).
    Terminal,
    Window(u32),
}

/// Where a key goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    /// To the console.
    Terminal(u8),
    /// Queued for a window's app.
    Window(u64),
    /// Taken by the manager (the "next window" key, or a paste with
    /// nothing on the clipboard).
    Consumed,
    /// Ctrl+Shift+V with the Terminal focused: the display types
    /// [`Manager::terminal_paste`] into the console (ADR-0095).
    TerminalPaste,
}

/// The window manager.
pub struct Manager {
    /// Where windows are placed and may be dragged.
    area: Rect,
    /// Bottom to top.
    frames: Vec<Frame>,
    focus: Focus,
    queues: BTreeMap<u64, VecDeque<Event>>,
    /// Apps with newly queued events, to be signalled.
    signals: BTreeSet<u64>,
    next_id: u32,
    /// The window being dragged, and the grab point inside it.
    drag: Option<(u32, i32, i32)>,
    /// The window being resized by an edge (ADR-0097).
    resize: Option<Resize>,
    /// Content pixels allowed on all windows together.
    pixel_budget: usize,
    /// The clipboard's text (ADR-0095): the system's copy, kept when the
    /// app that copied it ends.
    clipboard: Option<String>,
    /// The app that may copy: the user gave its focused window a key or a
    /// click since its last copy.
    may_copy: Option<u64>,
    /// The app the user pasted into, until it takes the text.
    paste: Option<u64>,
}

impl Manager {
    pub fn new(area: Rect, pixel_budget: usize) -> Self {
        Self {
            area,
            frames: Vec::new(),
            focus: Focus::Terminal,
            queues: BTreeMap::new(),
            signals: BTreeSet::new(),
            next_id: 1,
            drag: None,
            resize: None,
            pixel_budget,
            clipboard: None,
            may_copy: None,
            paste: None,
        }
    }

    /// The windows, bottom to top.
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The apps with events queued since the last call: the display
    /// service signals each.
    pub fn take_signals(&mut self) -> BTreeSet<u64> {
        core::mem::take(&mut self.signals)
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.frames.iter().position(|f| f.id == id)
    }

    fn owned(&self, owner: u64, id: u32) -> Result<usize, Status> {
        self.index(id)
            .filter(|&i| self.frames[i].owner == owner)
            .ok_or(Status::NotFound)
    }

    fn queue(&mut self, owner: u64, event: Event) {
        let queue = self.queues.entry(owner).or_default();
        // Pointer motion: only the latest position matters.
        if event.kind == kind::POINTER
            && let Some(last) = queue.back_mut()
            && last.kind == kind::POINTER
            && last.window == event.window
        {
            *last = event;
        } else if queue.len() < MAX_QUEUED {
            queue.push_back(event);
        } else {
            return;
        }
        self.signals.insert(owner);
    }

    fn notify_focus(&mut self, id: u32, gained: bool) {
        if let Some(i) = self.index(id) {
            let owner = self.frames[i].owner;
            self.queue(
                owner,
                Event {
                    window: id,
                    kind: kind::FOCUS,
                    pressed: gained,
                    ..Event::default()
                },
            );
        }
    }

    /// Moves the keyboard focus (and raises a focused window, restoring
    /// it if it was minimized).
    pub fn set_focus(&mut self, focus: Focus) {
        if let Focus::Window(id) = focus {
            let Some(i) = self.index(id) else {
                return;
            };
            let mut frame = self.frames.remove(i);
            frame.minimized = false;
            self.frames.push(frame);
        }
        if focus == self.focus {
            return;
        }
        if let Focus::Window(old) = self.focus {
            self.notify_focus(old, false);
        }
        self.focus = focus;
        self.focus_moved();
        if let Focus::Window(new) = focus {
            self.notify_focus(new, true);
        }
    }

    /// Opens a window for `owner` (app `app`, as Core named it); it is
    /// placed in the middle of the area, a step after the last one, on top,
    /// with the focus.
    pub fn open(
        &mut self,
        owner: u64,
        app: &str,
        title: &str,
        width: u16,
        height: u16,
    ) -> Result<u32, Status> {
        let (width, height) = (i32::from(width), i32::from(height));
        let owned = self.frames.iter().filter(|f| f.owner == owner).count();
        let pixels: usize = self
            .frames
            .iter()
            .map(|f| (f.width * f.height) as usize)
            .sum();
        if owned >= MAX_WINDOWS_PER_APP
            || self.frames.len() >= MAX_WINDOWS
            || pixels + (width * height) as usize > self.pixel_budget
        {
            return Err(Status::TooMany);
        }
        let step = (self.next_id - 1) as i32 % 8;
        let (w, h) = (width + 2, height + TITLE_HEIGHT + 1);
        let right = self.area.x + self.area.w;
        let bottom = self.area.y + self.area.h;
        let x = (self.area.x + (self.area.w - w) / 2 + step * CASCADE)
            .min(right - w)
            .max(self.area.x);
        let y = (self.area.y + (self.area.h - h) / 2 + step * CASCADE)
            .min(bottom - h)
            .max(self.area.y);
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.frames.push(Frame {
            id,
            owner,
            app: app.into(),
            title: title.into(),
            x,
            y,
            width,
            height,
            presented: false,
            minimized: false,
            min: None,
            restore: None,
        });
        self.set_focus(Focus::Window(id));
        Ok(id)
    }

    fn remove(&mut self, index: usize) {
        let frame = self.frames.remove(index);
        if self.drag.is_some_and(|(id, _, _)| id == frame.id) {
            self.drag = None;
        }
        if self.resize.is_some_and(|r| r.id == frame.id) {
            self.resize = None;
        }
        if self.focus == Focus::Window(frame.id) {
            self.focus = Focus::Terminal;
            self.focus_moved();
            self.focus_below();
        }
    }

    /// The focus goes to the topmost window still shown, else the
    /// Terminal.
    fn focus_below(&mut self) {
        let next = self
            .frames
            .iter()
            .rev()
            .find(|f| !f.minimized && Focus::Window(f.id) != self.focus)
            .map(|f| f.id);
        self.set_focus(next.map_or(Focus::Terminal, Focus::Window));
    }

    /// Hides a window until it is restored (`set_focus`); the focus moves
    /// on if it had it (ADR-0076).
    pub fn minimize(&mut self, id: u32) {
        let Some(index) = self.index(id) else {
            return;
        };
        self.frames[index].minimized = true;
        if self.drag.is_some_and(|(dragged, _, _)| dragged == id) {
            self.drag = None;
        }
        if self.resize.is_some_and(|r| r.id == id) {
            self.resize = None;
        }
        if self.focus == Focus::Window(id) {
            self.focus_below();
        }
    }

    /// `CLOSE` from the app.
    pub fn close(&mut self, owner: u64, id: u32) -> Result<(), Status> {
        let index = self.owned(owner, id)?;
        self.remove(index);
        Ok(())
    }

    /// The app's end was closed (it exited): all its windows go. Returns
    /// their ids.
    pub fn close_owner(&mut self, owner: u64) -> Vec<u32> {
        let mut closed = Vec::new();
        while let Some(index) = self.frames.iter().position(|f| f.owner == owner) {
            closed.push(self.frames[index].id);
            self.remove(index);
        }
        self.queues.remove(&owner);
        if self.may_copy == Some(owner) {
            self.may_copy = None;
        }
        if self.paste == Some(owner) {
            self.paste = None;
        }
        self.signals.remove(&owner);
        closed
    }

    /// `PRESENT` from the app.
    pub fn present(&mut self, owner: u64, id: u32) -> Result<(), Status> {
        let index = self.owned(owner, id)?;
        self.frames[index].presented = true;
        Ok(())
    }

    /// `EVENTS` from the app: up to `max` of its queued events.
    pub fn take_events(&mut self, owner: u64, max: usize) -> Vec<Event> {
        let Some(queue) = self.queues.get_mut(&owner) else {
            return Vec::new();
        };
        let count = max.min(queue.len());
        queue.drain(..count).collect()
    }

    /// A key typed on the keyboard.
    pub fn key(&mut self, byte: u8) -> KeyRoute {
        if byte == proto::KEY_NEXT_WINDOW {
            self.next_window();
            return KeyRoute::Consumed;
        }
        let paste = byte == proto::CTRL_V || byte == proto::KEY_PASTE;
        match self.focus {
            Focus::Terminal if byte == proto::KEY_PASTE => KeyRoute::TerminalPaste,
            Focus::Terminal => KeyRoute::Terminal(byte),
            Focus::Window(id) => {
                let Some(index) = self.index(id) else {
                    return KeyRoute::Terminal(byte);
                };
                let owner = self.frames[index].owner;
                if paste {
                    // The app gets the text through `PASTE`, never the key.
                    if self.clipboard.is_none() {
                        return KeyRoute::Consumed;
                    }
                    self.paste = Some(owner);
                    self.queue(
                        owner,
                        Event {
                            window: id,
                            kind: kind::PASTE,
                            ..Event::default()
                        },
                    );
                    return KeyRoute::Window(owner);
                }
                self.may_copy = Some(owner);
                self.queue(
                    owner,
                    Event {
                        window: id,
                        kind: kind::KEY,
                        key: byte,
                        ..Event::default()
                    },
                );
                KeyRoute::Window(owner)
            }
        }
    }

    /// The focus moved: what the user allowed the app that had it ends.
    fn focus_moved(&mut self) {
        self.may_copy = None;
        self.paste = None;
    }

    /// The app whose window has the focus.
    fn focused_owner(&self) -> Option<u64> {
        match self.focus {
            Focus::Window(id) => self.index(id).map(|i| self.frames[i].owner),
            Focus::Terminal => None,
        }
    }

    /// `COPY` from the app behind `owner` (ADR-0095): `text` goes on the
    /// clipboard if its window has the focus and the user gave it a key or
    /// a click since its last copy. Returns the bytes copied.
    pub fn copy(&mut self, owner: u64, text: &[u8]) -> Result<usize, Status> {
        let text = proto::clipboard_text(text).ok_or(Status::BadRequest)?;
        if self.focused_owner() != Some(owner) || self.may_copy != Some(owner) {
            return Err(Status::NotAllowed);
        }
        self.may_copy = None;
        self.clipboard = Some(String::from(text));
        Ok(text.len())
    }

    /// `PASTE` from the app behind `owner`: the clipboard's text, once
    /// after each paste the user made into its focused window.
    pub fn take_paste(&mut self, owner: u64) -> Result<&str, Status> {
        if self.paste != Some(owner) || self.focused_owner() != Some(owner) {
            return Err(Status::NotFound);
        }
        self.paste = None;
        self.clipboard.as_deref().ok_or(Status::NotFound)
    }

    /// What Ctrl+Shift+V types into the Terminal: the clipboard's first
    /// line, its control characters left out, at most
    /// [`TERMINAL_PASTE_MAX`] bytes; and whether anything was left behind.
    /// A line break would run a command the user has not read, so none is
    /// typed.
    pub fn terminal_paste(&self) -> Option<(String, bool)> {
        let text = self.clipboard.as_deref()?;
        let mut lines = text.lines();
        let mut first = String::new();
        let mut cut = false;
        for c in lines.next()?.chars().filter(|c| !c.is_control()) {
            if first.len() + c.len_utf8() > TERMINAL_PASTE_MAX {
                cut = true;
                break;
            }
            first.push(c);
        }
        let more = lines.any(|line| !line.trim().is_empty());
        Some((first, cut || more))
    }

    /// Bytes on the clipboard.
    pub fn clipboard_len(&self) -> usize {
        self.clipboard.as_ref().map_or(0, String::len)
    }

    /// The focus goes round: the Terminal, then the windows in the order
    /// they were opened.
    fn next_window(&mut self) {
        let mut ids: Vec<u32> = self.frames.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        let next = match self.focus {
            Focus::Terminal => ids.first().copied(),
            Focus::Window(id) => ids.iter().copied().find(|&other| other > id),
        };
        self.set_focus(next.map_or(Focus::Terminal, Focus::Window));
    }

    /// The topmost window shown at a point.
    fn at(&self, x: i32, y: i32) -> Option<usize> {
        self.frames
            .iter()
            .rposition(|f| !f.minimized && f.outer().contains(x, y))
    }

    /// The window whose edges a press at `x`, `y` would catch, and which:
    /// the topmost window near the point, if the point is on its edge.
    fn grip_at(&self, x: i32, y: i32) -> Option<(usize, u8)> {
        let index = self.frames.iter().rposition(|f| {
            let o = f.outer();
            !f.minimized
                && Rect::new(o.x - GRIP, o.y - GRIP, o.w + 2 * GRIP, o.h + 2 * GRIP).contains(x, y)
        })?;
        let edges = self.frames[index].edges_at(x, y);
        (edges != 0).then_some((index, edges))
    }

    /// The largest content a window may have: the area, the protocol's
    /// limit, and the pixels the other windows leave.
    fn largest(&self, id: u32) -> (i32, i32) {
        let others: usize = self
            .frames
            .iter()
            .filter(|f| f.id != id)
            .map(|f| (f.width * f.height) as usize)
            .sum();
        let width = (self.area.w - 2).min(i32::from(proto::MAX_WIDTH));
        let height = (self.area.h - TITLE_HEIGHT - 1).min(i32::from(proto::MAX_HEIGHT));
        let budget = self.pixel_budget.saturating_sub(others) as i32;
        (width, height.min(budget / width.max(1)))
    }

    /// Follows the pointer with the edges being dragged; `true` if the
    /// frame changed.
    fn resize_to(&mut self, resize: Resize, x: i32, y: i32) -> bool {
        let Some(index) = self.index(resize.id) else {
            self.resize = None;
            return false;
        };
        let (largest_w, largest_h) = self.largest(resize.id);
        let frame = &self.frames[index];
        let Some((min_w, min_h)) = frame.min else {
            return false;
        };
        let (x0, y0, w0, h0) = resize.start;
        let (dx, dy) = (x - resize.from.0, y - resize.from.1);
        let mut w = w0;
        let mut h = h0;
        if resize.edges & edge::RIGHT != 0 {
            w = w0 + dx;
        } else if resize.edges & edge::LEFT != 0 {
            w = w0 - dx;
        }
        if resize.edges & edge::BOTTOM != 0 {
            h = h0 + dy;
        } else if resize.edges & edge::TOP != 0 {
            // The title bar stays below the area's top.
            h = (h0 - dy).min(h0 + (y0 - self.area.y));
        }
        w = w.clamp(min_w, largest_w.max(min_w));
        h = h.clamp(min_h, largest_h.max(min_h));
        // What a left or top edge gives, the corner opposite keeps.
        let nx = if resize.edges & edge::LEFT != 0 {
            x0 + w0 - w
        } else {
            x0
        };
        let ny = if resize.edges & edge::TOP != 0 {
            y0 + h0 - h
        } else {
            y0
        };
        let frame = &mut self.frames[index];
        let changed = (frame.x, frame.y, frame.width, frame.height) != (nx, ny, w, h);
        (frame.x, frame.y, frame.width, frame.height) = (nx, ny, w, h);
        changed
    }

    /// A resize ended: the app is told its new size, if it changed.
    fn resized(&mut self, resize: Resize) {
        let Some(index) = self.index(resize.id) else {
            return;
        };
        let frame = &mut self.frames[index];
        if (frame.width, frame.height) != (resize.start.2, resize.start.3) {
            frame.restore = None;
            self.tell_size(index);
        }
    }

    /// Queues a [`kind::RESIZE`] event with the window's size.
    fn tell_size(&mut self, index: usize) {
        let frame = &self.frames[index];
        let (owner, id) = (frame.owner, frame.id);
        let (width, height) = (frame.width as i16, frame.height as i16);
        self.queue(
            owner,
            Event {
                window: id,
                kind: kind::RESIZE,
                x: width,
                y: height,
                ..Event::default()
            },
        );
    }

    /// Maximizes a resizable window to the area (in its middle, if the
    /// largest content is smaller), or puts it back where it was. `false`
    /// if it cannot be resized.
    pub fn zoom(&mut self, id: u32) -> bool {
        let Some(index) = self.index(id).filter(|&i| self.frames[i].resizable()) else {
            return false;
        };
        self.drag = None;
        self.resize = None;
        let (largest_w, largest_h) = self.largest(id);
        let area = self.area;
        let frame = &mut self.frames[index];
        let (min_w, min_h) = frame.min.unwrap_or_default();
        if let Some((x, y, w, h)) = frame.restore.take() {
            (frame.x, frame.y, frame.width, frame.height) = (x, y, w, h);
        } else {
            frame.restore = Some((frame.x, frame.y, frame.width, frame.height));
            let (w, h) = (largest_w.max(min_w), largest_h.max(min_h));
            frame.width = w;
            frame.height = h;
            frame.x = area.x + (area.w - (w + 2)) / 2;
            frame.y = area.y;
        }
        self.set_focus(Focus::Window(id));
        // `set_focus` moved it to the top.
        let top = self.frames.len() - 1;
        self.tell_size(top);
        true
    }

    /// A double click at `x`, `y`: on a resizable window's title bar (not
    /// its buttons), it maximizes or restores the window. `true` if so.
    pub fn double_click(&mut self, x: i32, y: i32) -> bool {
        let Some(index) = self.at(x, y) else {
            return false;
        };
        let frame = &self.frames[index];
        let on_buttons = [
            frame.close_button(),
            frame.minimize_button(),
            frame.zoom_button(),
        ]
        .iter()
        .any(|b| b.contains(x, y));
        if !frame.title_bar().contains(x, y) || on_buttons {
            return false;
        }
        let id = frame.id;
        self.zoom(id)
    }

    /// `RESIZABLE` from the app (ADR-0097): its window may be resized, down
    /// to `min_width × min_height` (at least the protocol's minimum, at
    /// most its size now).
    pub fn set_resizable(
        &mut self,
        owner: u64,
        id: u32,
        min_width: u16,
        min_height: u16,
    ) -> Result<(), Status> {
        let index = self.owned(owner, id)?;
        let frame = &mut self.frames[index];
        let (w, h) = (i32::from(min_width), i32::from(min_height));
        if w < i32::from(proto::MIN_WIDTH)
            || h < i32::from(proto::MIN_HEIGHT)
            || w > frame.width
            || h > frame.height
        {
            return Err(Status::BadRequest);
        }
        frame.min = Some((w, h));
        Ok(())
    }

    /// The size a window has now, for `RESIZE` (the app's pixels follow).
    pub fn size(&self, owner: u64, id: u32) -> Result<(u16, u16), Status> {
        let frame = &self.frames[self.owned(owner, id)?];
        Ok((frame.width as u16, frame.height as u16))
    }

    /// The pointer moved to `x`, `y`; `true` if the screen changes (a
    /// window was dragged).
    pub fn pointer_moved(&mut self, x: i32, y: i32) -> bool {
        if let Some(resize) = self.resize {
            return self.resize_to(resize, x, y);
        }
        if let Some((id, grab_x, grab_y)) = self.drag
            && let Some(index) = self.index(id)
        {
            let area = self.area;
            let frame = &mut self.frames[index];
            let w = frame.outer().w;
            // The title bar stays reachable.
            frame.x = (x - grab_x).clamp(area.x - w + 48, area.x + area.w - 48);
            frame.y = (y - grab_y).clamp(area.y, area.y + area.h - TITLE_HEIGHT);
            return true;
        }
        if let Focus::Window(id) = self.focus
            && let Some(index) = self.index(id)
            && self.at(x, y) == Some(index)
        {
            let frame = &self.frames[index];
            let content = frame.content();
            if content.contains(x, y) {
                let owner = frame.owner;
                self.queue(
                    owner,
                    Event {
                        window: id,
                        kind: kind::POINTER,
                        x: (x - content.x) as i16,
                        y: (y - content.y) as i16,
                        ..Event::default()
                    },
                );
            }
        }
        false
    }

    /// A pointer button went down or up at `x`, `y`. `true` if a window
    /// took it; otherwise the desktop behind the windows gets it.
    pub fn button(&mut self, button: u8, pressed: bool, x: i32, y: i32) -> bool {
        if !pressed && button == 1 && self.drag.take().is_some() {
            return true;
        }
        if !pressed
            && button == 1
            && let Some(resize) = self.resize.take()
        {
            self.resized(resize);
            return true;
        }
        if pressed
            && button == 1
            && let Some((index, edges)) = self.grip_at(x, y)
        {
            let frame = &self.frames[index];
            let resize = Resize {
                id: frame.id,
                edges,
                from: (x, y),
                start: (frame.x, frame.y, frame.width, frame.height),
            };
            self.set_focus(Focus::Window(resize.id));
            self.resize = Some(resize);
            return true;
        }
        let Some(index) = self.at(x, y) else {
            return false;
        };
        let frame = &self.frames[index];
        let (id, owner) = (frame.id, frame.owner);
        let content = frame.content();
        let on_close = frame.close_button().contains(x, y);
        let on_minimize = frame.minimize_button().contains(x, y);
        let on_zoom = frame.zoom_button().contains(x, y);
        let on_title = frame.title_bar().contains(x, y);
        let (grab_x, grab_y) = (x - frame.x, y - frame.y);
        if pressed && button == 1 && on_minimize {
            self.minimize(id);
            return true;
        }
        if pressed && button == 1 && on_zoom && frame.resizable() {
            self.zoom(id);
            return true;
        }
        if pressed {
            self.set_focus(Focus::Window(id));
        }
        if pressed && button == 1 && on_close {
            self.queue(
                owner,
                Event {
                    window: id,
                    kind: kind::CLOSE,
                    ..Event::default()
                },
            );
        } else if pressed && button == 1 && on_title {
            self.drag = Some((id, grab_x, grab_y));
        } else if content.contains(x, y) && self.focus == Focus::Window(id) {
            if pressed {
                self.may_copy = Some(owner);
            }
            self.queue(
                owner,
                Event {
                    window: id,
                    kind: kind::BUTTON,
                    button,
                    pressed,
                    x: (x - content.x) as i16,
                    y: (y - content.y) as i16,
                    ..Event::default()
                },
            );
        }
        true
    }
}

#[cfg(test)]
mod tests;
