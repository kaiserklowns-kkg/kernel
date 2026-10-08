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

    /// Right of the minimize button: resizing, not yet available (drawn
    /// disabled, takes no clicks).
    pub fn zoom_button(&self) -> Rect {
        let minimize = self.minimize_button();
        Rect::new(minimize.x + BUTTON_STEP, minimize.y, CLOSE_SIZE, CLOSE_SIZE)
    }

    /// Where the app's pixels go.
    pub fn content(&self) -> Rect {
        Rect::new(self.x + 1, self.y + TITLE_HEIGHT, self.width, self.height)
    }
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
        });
        self.set_focus(Focus::Window(id));
        Ok(id)
    }

    fn remove(&mut self, index: usize) {
        let frame = self.frames.remove(index);
        if self.drag.is_some_and(|(id, _, _)| id == frame.id) {
            self.drag = None;
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

    /// The pointer moved to `x`, `y`; `true` if the screen changes (a
    /// window was dragged).
    pub fn pointer_moved(&mut self, x: i32, y: i32) -> bool {
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
        let Some(index) = self.at(x, y) else {
            return false;
        };
        let frame = &self.frames[index];
        let (id, owner) = (frame.id, frame.owner);
        let content = frame.content();
        let on_close = frame.close_button().contains(x, y);
        let on_minimize = frame.minimize_button().contains(x, y);
        let on_title = frame.title_bar().contains(x, y);
        let (grab_x, grab_y) = (x - frame.x, y - frame.y);
        if pressed && button == 1 && on_minimize {
            self.minimize(id);
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
