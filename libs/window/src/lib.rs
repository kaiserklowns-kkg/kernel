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
    /// Where it was before it was maximized or tiled (`x`, `y`, `width`,
    /// `height`): the zoom button, a double click, a shortcut or a drag
    /// away put it back.
    pub restore: Option<(i32, i32, i32, i32)>,
    /// Where it was put (ADR-0107): a half or a quarter of the area, all
    /// of it, or the whole screen. `None`: where the user left it.
    pub tile: Option<Tile>,
}

/// Where a window can be put (ADR-0107).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// The whole area (between the menu bar and the dock).
    Maximized,
    /// The whole screen, with no frame: the menu bar and the dock give
    /// way while it has the focus.
    FullScreen,
}

impl Frame {
    /// Full screen: no border or title bar, only the app's pixels.
    pub fn full_screen(&self) -> bool {
        self.tile == Some(Tile::FullScreen)
    }

    /// Everything the window covers: border, title bar and content.
    pub fn outer(&self) -> Rect {
        if self.full_screen() {
            return self.content();
        }
        Rect::new(
            self.x,
            self.y,
            self.width + 2,
            self.height + TITLE_HEIGHT + 1,
        )
    }

    /// Empty in full screen.
    pub fn title_bar(&self) -> Rect {
        if self.full_screen() {
            return Rect::new(self.x, self.y, 0, 0);
        }
        Rect::new(self.x, self.y, self.width + 2, TITLE_HEIGHT)
    }

    /// At the left of the title bar (ADR-0078). Empty in full screen, as
    /// are the other buttons.
    pub fn close_button(&self) -> Rect {
        if self.full_screen() {
            return Rect::new(self.x, self.y, 0, 0);
        }
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
        let size = close.w;
        Rect::new(close.x + BUTTON_STEP, close.y, size, size)
    }

    /// Right of the minimize button: maximizes and restores a window that
    /// can be resized (ADR-0097); drawn disabled on one that cannot.
    pub fn zoom_button(&self) -> Rect {
        let minimize = self.minimize_button();
        let size = minimize.w;
        Rect::new(minimize.x + BUTTON_STEP, minimize.y, size, size)
    }

    /// Where the app's pixels go.
    pub fn content(&self) -> Rect {
        if self.full_screen() {
            return Rect::new(self.x, self.y, self.width, self.height);
        }
        Rect::new(self.x + 1, self.y + TITLE_HEIGHT, self.width, self.height)
    }

    pub fn resizable(&self) -> bool {
        self.min.is_some()
    }

    /// The edges (`edge::*`) a press at `x`, `y` would drag: within
    /// [`GRIP`] outside the frame, or on its border; never the title bar.
    pub fn edges_at(&self, x: i32, y: i32) -> u8 {
        if !self.resizable() || self.minimized || self.full_screen() {
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
/// A title bar dragged this close to the screen's side, or to the area's
/// top, puts the window there when let go (ADR-0107).
pub const SNAP_EDGE: i32 = 4;
/// At the side, this close to the area's top or bottom: a quarter.
pub const SNAP_CORNER: i32 = 80;
/// How far a tiled window's title bar is dragged before it comes loose.
const LOOSEN: i32 = 6;

/// The shortcuts for the focused window (ADR-0107): Super with the
/// arrows, and Super+F.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKey {
    /// The left half; from the right half, back where it was.
    Left,
    Right,
    /// All of the area.
    Up,
    /// Back where it was; a window where the user left it is minimized.
    Down,
    /// Full screen, or back.
    FullScreen,
}

impl TileKey {
    pub fn of(byte: u8) -> Option<Self> {
        match byte {
            proto::KEY_TILE_LEFT => Some(Self::Left),
            proto::KEY_TILE_RIGHT => Some(Self::Right),
            proto::KEY_TILE_UP => Some(Self::Up),
            proto::KEY_TILE_DOWN => Some(Self::Down),
            proto::KEY_FULL_SCREEN => Some(Self::FullScreen),
            _ => None,
        }
    }
}
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
    /// A volume key (ADR-0101), whoever has the focus: the desktop
    /// changes the system volume.
    Volume(VolumeKey),
}

/// A window that asked for the media keys (ADR-0102), and what it said
/// it plays (ADR-0103).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Player {
    owner: u64,
    id: u32,
    state: u8,
    title: String,
}

/// What the desktop shows of the player with the media keys (ADR-0103).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NowPlaying<'a> {
    pub app: &'a str,
    /// `proto::playing`.
    pub state: u8,
    pub title: &'a str,
}

/// The volume keys (ADR-0101).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKey {
    Mute,
    Down,
    Up,
}

impl VolumeKey {
    /// The volume key a byte is, if it is one.
    pub fn of(byte: u8) -> Option<Self> {
        match byte {
            proto::KEY_MUTE => Some(Self::Mute),
            proto::KEY_VOLUME_DOWN => Some(Self::Down),
            proto::KEY_VOLUME_UP => Some(Self::Up),
            _ => None,
        }
    }

    /// The volume (level 0 to 100, muted) after this key: Mute turns it
    /// on or off; Down and Up move it by [`VOLUME_STEP`] and unmute, as
    /// other systems do.
    pub fn apply(self, (level, muted): (u8, bool)) -> (u8, bool) {
        match self {
            Self::Mute => (level, !muted),
            Self::Down => (level.saturating_sub(VOLUME_STEP), false),
            Self::Up => (level.saturating_add(VOLUME_STEP).min(100), false),
        }
    }
}

/// How far one press of Volume Up or Down moves the level.
pub const VOLUME_STEP: u8 = 5;

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
    /// Where the drag's press was: a tiled window comes loose once the
    /// pointer has moved from it (ADR-0107).
    drag_from: (i32, i32),
    /// Where the window dragged would be put if let go now (ADR-0107).
    snap: Option<Tile>,
    /// The whole screen, for full screen (ADR-0107); the area until set.
    screen: Rect,
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
    /// The app that may open a file in another (ADR-0099), on the same
    /// terms.
    may_open: Option<u64>,
    /// The app the user pasted into, until it takes the text.
    paste: Option<u64>,
    /// The windows that asked for the media keys (ADR-0102), the one
    /// that gets them last.
    media: Vec<Player>,
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
            drag_from: (0, 0),
            snap: None,
            screen: area,
            resize: None,
            pixel_budget,
            clipboard: None,
            may_copy: None,
            may_open: None,
            paste: None,
            media: Vec::new(),
        }
    }

    /// The windows, bottom to top.
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// The whole screen, which full screen covers (ADR-0107).
    pub fn set_screen(&mut self, screen: Rect) {
        self.screen = screen;
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
            // The player the user looked at last has the media keys.
            if let Some(at) = self.media.iter().position(|p| p.id == new) {
                let entry = self.media.remove(at);
                self.media.push(entry);
            }
        }
    }

    /// `MEDIA_KEYS` (ADR-0102): the media keys go to `owner`'s window `id`
    /// from now on.
    pub fn want_media_keys(&mut self, owner: u64, id: u32) -> Result<(), Status> {
        self.owned(owner, id)?;
        self.media.retain(|p| p.owner != owner);
        self.media.push(Player {
            owner,
            id,
            state: proto::playing::STOPPED,
            title: String::new(),
        });
        Ok(())
    }

    /// The app the media keys go to, if any asked.
    pub fn media_owner(&self) -> Option<u64> {
        self.media.last().map(|p| p.owner)
    }

    /// `NOW_PLAYING` (ADR-0103): what `owner`'s window `id` plays, if it
    /// asked for the media keys.
    pub fn set_now_playing(&mut self, owner: u64, id: u32, data: &[u8]) -> Result<(), Status> {
        let (state, title) = proto::now_playing(data).ok_or(Status::BadRequest)?;
        let player = self
            .media
            .iter_mut()
            .find(|p| p.owner == owner && p.id == id)
            .ok_or(Status::NotAllowed)?;
        player.state = state;
        player.title = String::from(title);
        Ok(())
    }

    /// What the player with the media keys plays, once it has said
    /// (ADR-0103): its app's name (as Core verified it), the state and the
    /// title.
    pub fn now_playing(&self) -> Option<NowPlaying<'_>> {
        let player = self.media.last()?;
        if player.title.is_empty() {
            return None;
        }
        let frame = self.frames.iter().find(|f| f.id == player.id)?;
        Some(NowPlaying {
            app: &frame.app,
            state: player.state,
            title: &player.title,
        })
    }

    /// A media key: to the window that asked last, as a key event, focused
    /// or not (no input of the app's: it allows no copy or open).
    pub fn media_key(&mut self, byte: u8) -> KeyRoute {
        let Some((owner, id)) = self.media.last().map(|p| (p.owner, p.id)) else {
            return KeyRoute::Consumed;
        };
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
            tile: None,
        });
        self.set_focus(Focus::Window(id));
        Ok(id)
    }

    fn remove(&mut self, index: usize) {
        let frame = self.frames.remove(index);
        self.media.retain(|p| p.id != frame.id);
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
        if self.may_open == Some(owner) {
            self.may_open = None;
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
        if let Some(key) = VolumeKey::of(byte) {
            return KeyRoute::Volume(key);
        }
        if let Some(key) = TileKey::of(byte) {
            self.tile_key(key);
            return KeyRoute::Consumed;
        }
        if matches!(
            byte,
            proto::KEY_PLAY_PAUSE | proto::KEY_STOP | proto::KEY_PREVIOUS | proto::KEY_NEXT
        ) {
            return self.media_key(byte);
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
                self.user_acted(owner);
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
        self.may_open = None;
        self.paste = None;
    }

    /// The user gave `owner`'s focused window a key or a click: it may
    /// copy, and open a file, once each.
    fn user_acted(&mut self, owner: u64) {
        self.may_copy = Some(owner);
        self.may_open = Some(owner);
    }

    /// `OPEN_FILE` from the app behind `owner` (ADR-0099): allowed once
    /// for each key or click the user gave its focused window.
    pub fn take_open(&mut self, owner: u64) -> Result<(), Status> {
        if self.focused_owner() != Some(owner) || self.may_open != Some(owner) {
            return Err(Status::NotAllowed);
        }
        self.may_open = None;
        Ok(())
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
        self.largest_in(id, self.area.w - 2, self.area.h - TITLE_HEIGHT - 1)
    }

    /// The largest content up to `width × height`: the protocol's limit,
    /// and the pixels the other windows leave.
    fn largest_in(&self, id: u32, width: i32, height: i32) -> (i32, i32) {
        let others: usize = self
            .frames
            .iter()
            .filter(|f| f.id != id)
            .map(|f| (f.width * f.height) as usize)
            .sum();
        let width = width.min(i32::from(proto::MAX_WIDTH));
        let height = height.min(i32::from(proto::MAX_HEIGHT));
        let budget = self.pixel_budget.saturating_sub(others) as i32;
        (width, height.min(budget / width.max(1)))
    }

    /// Where window `id` goes for `tile` (`x`, `y`, content `width`,
    /// `height`): its part of the area (of the screen, full screen), at
    /// least its smallest size; in that part's middle if it cannot fill
    /// it.
    fn tile_geometry(&self, id: u32, tile: Tile) -> Option<(i32, i32, i32, i32)> {
        let frame = &self.frames[self.index(id)?];
        let (min_w, min_h) = frame.min?;
        if tile == Tile::FullScreen {
            let s = self.screen;
            let (w, h) = self.largest_in(id, s.w, s.h);
            let (w, h) = (w.max(min_w), h.max(min_h));
            return Some((s.x + (s.w - w) / 2, s.y + (s.h - h) / 2, w, h));
        }
        let a = self.area;
        let (half_w, half_h) = (a.w / 2, a.h / 2);
        // The part's outer rectangle.
        let part = match tile {
            Tile::Left => Rect::new(a.x, a.y, half_w, a.h),
            Tile::Right => Rect::new(a.x + a.w - half_w, a.y, half_w, a.h),
            Tile::TopLeft => Rect::new(a.x, a.y, half_w, half_h),
            Tile::TopRight => Rect::new(a.x + a.w - half_w, a.y, half_w, half_h),
            Tile::BottomLeft => Rect::new(a.x, a.y + a.h - half_h, half_w, half_h),
            Tile::BottomRight => Rect::new(a.x + a.w - half_w, a.y + a.h - half_h, half_w, half_h),
            Tile::Maximized | Tile::FullScreen => a,
        };
        let (w, h) = self.largest_in(id, part.w - 2, part.h - TITLE_HEIGHT - 1);
        let (w, h) = (w.max(min_w), h.max(min_h));
        let x = part.x + (part.w - (w + 2)).max(0) / 2;
        let y = part.y + (part.h - (h + TITLE_HEIGHT + 1)).max(0) / 2;
        Some((x, y, w, h))
    }

    /// Puts window `id` in `tile`'s place, or (`None`) back where it was
    /// before (ADR-0107). Only a window that can be resized moves; `false`
    /// if it did not.
    pub fn tile(&mut self, id: u32, tile: Option<Tile>) -> bool {
        let Some(index) = self.index(id).filter(|&i| self.frames[i].resizable()) else {
            return false;
        };
        self.drag = None;
        self.resize = None;
        self.snap = None;
        match tile {
            None => {
                let frame = &mut self.frames[index];
                let Some((x, y, w, h)) = frame.restore.take() else {
                    return false;
                };
                (frame.x, frame.y, frame.width, frame.height) = (x, y, w, h);
                frame.tile = None;
            }
            Some(tile) => {
                let Some((x, y, w, h)) = self.tile_geometry(id, tile) else {
                    return false;
                };
                let frame = &mut self.frames[index];
                if frame.tile.is_none() {
                    frame.restore = Some((frame.x, frame.y, frame.width, frame.height));
                }
                (frame.x, frame.y, frame.width, frame.height) = (x, y, w, h);
                frame.tile = Some(tile);
                frame.minimized = false;
            }
        }
        self.set_focus(Focus::Window(id));
        // `set_focus` moved it to the top.
        let top = self.frames.len() - 1;
        self.tell_size(top);
        true
    }

    /// The window shown full screen with the focus (ADR-0107): the
    /// desktop then draws no menu bar or dock.
    pub fn full_screen(&self) -> Option<u32> {
        let Focus::Window(id) = self.focus else {
            return None;
        };
        let frame = &self.frames[self.index(id)?];
        (frame.full_screen() && !frame.minimized).then_some(id)
    }

    /// Where the window dragged would go if let go now (ADR-0107): its
    /// outer rectangle, for the desktop to show.
    pub fn snap_preview(&self) -> Option<Rect> {
        let (id, _, _) = self.drag?;
        let (x, y, w, h) = self.tile_geometry(id, self.snap?)?;
        Some(Rect::new(x, y, w + 2, h + TITLE_HEIGHT + 1))
    }

    /// The place a title bar dragged to `x`, `y` would take: a half at
    /// the screen's left or right edge, a quarter near its corners, all
    /// of the area at its top.
    fn snap_at(&self, x: i32, y: i32) -> Option<Tile> {
        let (a, s) = (self.area, self.screen);
        let left = x <= s.x + SNAP_EDGE;
        let right = x >= s.x + s.w - 1 - SNAP_EDGE;
        let top = y < a.y + SNAP_CORNER;
        let bottom = y >= a.y + a.h - SNAP_CORNER;
        match (left, right, top, bottom) {
            (true, _, true, _) => Some(Tile::TopLeft),
            (true, _, _, true) => Some(Tile::BottomLeft),
            (true, ..) => Some(Tile::Left),
            (_, true, true, _) => Some(Tile::TopRight),
            (_, true, _, true) => Some(Tile::BottomRight),
            (_, true, ..) => Some(Tile::Right),
            _ if y <= a.y + SNAP_EDGE => Some(Tile::Maximized),
            _ => None,
        }
    }

    /// A shortcut for the focused window (ADR-0107); `true` if a window
    /// moved.
    fn tile_key(&mut self, key: TileKey) -> bool {
        let Focus::Window(id) = self.focus else {
            return false;
        };
        let Some(index) = self.index(id) else {
            return false;
        };
        let frame = &self.frames[index];
        let now = frame.tile;
        if !frame.resizable() {
            // A window of a fixed size can only be put away.
            if key == TileKey::Down {
                self.minimize(id);
                return true;
            }
            return false;
        }
        match key {
            TileKey::Left if now == Some(Tile::Right) => self.tile(id, None),
            TileKey::Left => self.tile(id, Some(Tile::Left)),
            TileKey::Right if now == Some(Tile::Left) => self.tile(id, None),
            TileKey::Right => self.tile(id, Some(Tile::Right)),
            TileKey::Up => self.tile(id, Some(Tile::Maximized)),
            TileKey::Down if now.is_some() => self.tile(id, None),
            TileKey::Down => {
                self.minimize(id);
                true
            }
            TileKey::FullScreen if now == Some(Tile::FullScreen) => self.tile(id, None),
            TileKey::FullScreen => self.tile(id, Some(Tile::FullScreen)),
        }
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
            // Resized by hand: it is where the user left it now.
            frame.restore = None;
            frame.tile = None;
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
    /// Tiled (ADR-0107) windows are put back too.
    pub fn zoom(&mut self, id: u32) -> bool {
        let Some(index) = self.index(id) else {
            return false;
        };
        if self.frames[index].tile.is_some() {
            self.tile(id, None)
        } else {
            self.tile(id, Some(Tile::Maximized))
        }
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
        if let Some((id, mut grab_x, grab_y)) = self.drag
            && let Some(index) = self.index(id)
        {
            let (fx, fy) = self.drag_from;
            let moved = (x - fx).abs() > LOOSEN || (y - fy).abs() > LOOSEN;
            // A tiled window dragged away goes back to its size, the grab
            // as far along its title bar as before (ADR-0107).
            if moved
                && self.frames[index].tile.is_some()
                && let Some((_, _, w, h)) = self.frames[index].restore
            {
                let frame = &mut self.frames[index];
                let before = frame.outer().w.max(1);
                grab_x = grab_x * (w + 2) / before;
                (frame.width, frame.height) = (w, h);
                frame.restore = None;
                frame.tile = None;
                self.drag = Some((id, grab_x, grab_y));
                self.tell_size(index);
            }
            // Not yet loose: it stays in its place.
            if self.frames[index].tile.is_some() {
                return false;
            }
            self.snap = if self.frames[index].resizable() && self.frames[index].tile.is_none() {
                self.snap_at(x, y)
            } else {
                None
            };
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
        if !pressed
            && button == 1
            && let Some((id, _, _)) = self.drag.take()
        {
            // Let go at an edge: the window takes that place (ADR-0107).
            if let Some(tile) = self.snap.take() {
                self.tile(id, Some(tile));
            }
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
            self.drag_from = (x, y);
            self.snap = None;
        } else if content.contains(x, y) && self.focus == Focus::Window(id) {
            if pressed {
                self.user_acted(owner);
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
