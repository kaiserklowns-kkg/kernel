//! The window protocol (ADR-0059), between apps and the display service.
//!
//! An app given `window` finds `use windows` in its handle directory: a
//! client end of the display service's window endpoint, minted for it by
//! Oceans Core with a badge of its own. The display service asks Core whose
//! the badge is, so the frame it draws around every window names the app as
//! Core verified it; the app draws only inside, into shared memory.
//!
//! - **Pixels:** `OPEN` returns a memory object of `width × height`
//!   pixels, each a little-endian `u32` `0x00RRGGBB`, row after row. The
//!   app draws into it and calls `PRESENT`; the display copies it to the
//!   screen at its next frame.
//! - **Events:** the display queues what happens to the app's windows
//!   (keys while one has the focus, the pointer over a focused one, focus
//!   changes, a click on the close button) and signals the notification
//!   given at `OPEN`; `EVENTS` takes them.
//! - **Ending:** `CLOSE` closes one window; the end being closed (the app
//!   exiting) closes them all.
//! - **The clipboard** (ADR-0095): the system keeps it, as text. An app
//!   puts text there (`COPY`) only while its window has the focus, once
//!   for each key or click the user gave it. It never reads the clipboard
//!   when it likes: when the user pastes into its window (Ctrl+V, Ctrl+
//!   Shift+V), a [`kind::PASTE`] event comes, and `PASTE` then hands over
//!   the text, once.
//! - **Resizing** (ADR-0097): an app that can lay itself out at other
//!   sizes says so (`RESIZABLE`). The user may then drag the window's
//!   edges, or maximize it; when a resize ends, a [`kind::RESIZE`] event
//!   comes, and `RESIZE` hands over pixels of the new size. Until the app
//!   presents them, the old pixels are shown.

/// Operations (request labels).
pub mod op {
    /// data = `[bits u64][width u16][height u16][title]`, handles =
    /// `[notification]` (signalled with `bits` when events are queued) →
    /// data = `[window u32]`, handles = `[pixels]`.
    pub const OPEN: u64 = 1;
    /// data = `[window u32]`: the pixels are ready to be shown.
    pub const PRESENT: u64 = 2;
    /// → data = up to [`super::MAX_EVENTS`] events of
    /// [`super::Event::SIZE`] bytes, for any of the caller's windows.
    pub const EVENTS: u64 = 3;
    /// data = `[window u32]`: closes the window.
    pub const CLOSE: u64 = 4;
    /// data = text: a notification on the desktop, shown after the app's
    /// name (ADR-0065). Needs `notifications`; one per
    /// [`super::NOTIFY_INTERVAL_MS`] per app (`TooMany` sooner).
    pub const NOTIFY: u64 = 5;
    /// Puts text on the clipboard (ADR-0095): data = the text, or data =
    /// `[length u32]` and handles = `[memory]` holding it (for text longer
    /// than an inline message). 1 to [`super::MAX_CLIPBOARD`] bytes of
    /// UTF-8. `NotAllowed` unless the caller's window has the focus and
    /// the user gave it a key or click since its last copy.
    pub const COPY: u64 = 6;
    /// After a [`super::kind::PASTE`] event: → data = `[length u32]`,
    /// handles = `[memory]` (readable) holding the text. `NotFound` when
    /// no paste waits for the caller (each is taken once).
    pub const PASTE: u64 = 7;
    /// data = `[window u32][min width u16][min height u16]`: the window can
    /// be resized, down to that size (from [`super::MIN_WIDTH`] ×
    /// [`super::MIN_HEIGHT`] up to its size now).
    pub const RESIZABLE: u64 = 8;
    /// After a [`super::kind::RESIZE`] event: data = `[window u32]` →
    /// data = `[width u16][height u16]`, handles = `[pixels]`: new pixels
    /// at the window's size now, which replace the old ones.
    pub const RESIZE: u64 = 9;
    /// data = `[app id length u8][app id][name]`: opens the file `name`
    /// (in Home, as `folder/file.txt`) in the app with that id, or in the
    /// first app that opens its kind when the id is empty (ADR-0099). Once
    /// for each key or click the user gave the caller's focused window
    /// (`NotAllowed` otherwise); `NotFound` when no app (or not that one)
    /// opens it.
    pub const OPEN_FILE: u64 = 10;
    /// data = `name` → `ID\0NAME\0` for each app that opens it, the one
    /// `OPEN_FILE` would choose first (ADR-0099).
    pub const OPENERS: u64 = 11;
}

/// The most text the clipboard holds, in bytes.
pub const MAX_CLIPBOARD: usize = 64 * 1024;

/// Text acceptable on the clipboard: 1 to [`MAX_CLIPBOARD`] bytes of
/// UTF-8 with no control characters but line breaks and tabs.
pub fn clipboard_text(data: &[u8]) -> Option<&str> {
    let text = core::str::from_utf8(data).ok()?;
    let ok = !text.is_empty()
        && text.len() <= MAX_CLIPBOARD
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'));
    ok.then_some(text)
}

/// Longest notification text, in bytes.
pub const MAX_NOTIFICATION: usize = 120;
/// The shortest time between two notifications of one app.
pub const NOTIFY_INTERVAL_MS: u64 = 3000;

/// A notification's text, if acceptable: 1 to [`MAX_NOTIFICATION`] bytes
/// of UTF-8, one line, no control characters.
pub fn notification_text(data: &[u8]) -> Option<&str> {
    let text = core::str::from_utf8(data).ok()?;
    let ok = !text.trim().is_empty()
        && text.len() <= MAX_NOTIFICATION
        && !text.chars().any(char::is_control);
    ok.then_some(text.trim())
}

/// Reply labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    BadRequest = 1,
    /// Core does not know the caller as a running app with `window` (for
    /// windows) or `notifications` (for notifications); or, for `COPY`, its
    /// window has not the focus or the user gave it nothing since its last
    /// copy.
    NotAllowed = 2,
    /// The app has [`MAX_WINDOWS_PER_APP`] windows, or the screen is full;
    /// or it notified less than [`NOTIFY_INTERVAL_MS`] ago.
    TooMany = 3,
    /// The display could not make the pixel memory.
    NoMemory = 4,
    /// No such window of the caller's.
    NotFound = 5,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            2 => Self::NotAllowed,
            3 => Self::TooMany,
            4 => Self::NoMemory,
            5 => Self::NotFound,
            _ => Self::BadRequest,
        }
    }
}

/// Window content sizes accepted, in pixels.
pub const MIN_WIDTH: u16 = 64;
pub const MIN_HEIGHT: u16 = 32;
pub const MAX_WIDTH: u16 = 1024;
pub const MAX_HEIGHT: u16 = 768;
/// Longest title, in bytes (UTF-8, no control characters).
pub const MAX_TITLE: usize = 48;
pub const MAX_WINDOWS_PER_APP: usize = 4;
/// Events one `EVENTS` reply carries.
pub const MAX_EVENTS: usize = 20;
/// The byte keyboards send for Ctrl+Tab (`oceans_abi::display::
/// KEY_NEXT_WINDOW`): the focus moves on, and no app sees it.
pub const KEY_NEXT_WINDOW: u8 = 0x1e;
/// The bytes that paste (`oceans_abi::display::CTRL_V`, `KEY_PASTE`,
/// ADR-0095): in a window both do, and the app gets a [`kind::PASTE`]
/// event instead of the key; in the Terminal only `KEY_PASTE` (Ctrl+Shift+
/// V) does.
pub const CTRL_V: u8 = 0x16;
pub const KEY_PASTE: u8 = 0x8b;

/// Event kinds.
pub mod kind {
    /// A key byte typed while the window had the focus (`key`).
    pub const KEY: u8 = 1;
    /// The pointer moved over the focused window (`x`, `y` inside it).
    pub const POINTER: u8 = 2;
    /// A button pressed or released over the focused window (`button`,
    /// `pressed`, `x`, `y`).
    pub const BUTTON: u8 = 3;
    /// The window gained (`pressed`) or lost the focus.
    pub const FOCUS: u8 = 4;
    /// The user clicked the close button: the app should close it.
    pub const CLOSE: u8 = 5;
    /// The user pasted into the window (ADR-0095): `PASTE` takes the text.
    pub const PASTE: u8 = 6;
    /// The user resized the window (ADR-0097): `x` and `y` are its new
    /// width and height; `RESIZE` takes pixels of that size.
    pub const RESIZE: u8 = 7;
}

/// Something that happened to a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub window: u32,
    pub kind: u8,
    pub key: u8,
    pub button: u8,
    pub pressed: bool,
    pub x: i16,
    pub y: i16,
}

impl Event {
    pub const SIZE: usize = 12;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[..4].copy_from_slice(&self.window.to_le_bytes());
        out[4] = self.kind;
        out[5] = self.key;
        out[6] = self.button;
        out[7] = u8::from(self.pressed);
        out[8..10].copy_from_slice(&self.x.to_le_bytes());
        out[10..12].copy_from_slice(&self.y.to_le_bytes());
        out
    }

    /// `None` if too short or of an unknown kind.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::SIZE] = bytes.get(..Self::SIZE)?.try_into().ok()?;
        let event = Self {
            window: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            kind: bytes[4],
            key: bytes[5],
            button: bytes[6],
            pressed: bytes[7] != 0,
            x: i16::from_le_bytes([bytes[8], bytes[9]]),
            y: i16::from_le_bytes([bytes[10], bytes[11]]),
        };
        (kind::KEY..=kind::RESIZE)
            .contains(&event.kind)
            .then_some(event)
    }
}

/// An `OPEN` request's data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenRequest<'a> {
    pub bits: u64,
    pub width: u16,
    pub height: u16,
    pub title: &'a str,
}

/// Bytes of an encoded [`OpenRequest`], at most.
pub const OPEN_MAX: usize = 12 + MAX_TITLE;

impl<'a> OpenRequest<'a> {
    /// `None` if malformed or out of bounds.
    pub fn decode(data: &'a [u8]) -> Option<Self> {
        let bits = u64::from_le_bytes(data.get(..8)?.try_into().ok()?);
        let width = u16::from_le_bytes(data.get(8..10)?.try_into().ok()?);
        let height = u16::from_le_bytes(data.get(10..12)?.try_into().ok()?);
        let title = core::str::from_utf8(&data[12..]).ok()?;
        let fits = (MIN_WIDTH..=MAX_WIDTH).contains(&width)
            && (MIN_HEIGHT..=MAX_HEIGHT).contains(&height)
            && title.len() <= MAX_TITLE
            && !title.chars().any(char::is_control)
            && bits != 0;
        fits.then_some(Self {
            bits,
            width,
            height,
            title,
        })
    }

    /// The request's data and its length; `None` if `title` is too long.
    pub fn encode(&self) -> Option<([u8; OPEN_MAX], usize)> {
        if self.title.len() > MAX_TITLE {
            return None;
        }
        let mut out = [0u8; OPEN_MAX];
        out[..8].copy_from_slice(&self.bits.to_le_bytes());
        out[8..10].copy_from_slice(&self.width.to_le_bytes());
        out[10..12].copy_from_slice(&self.height.to_le_bytes());
        out[12..12 + self.title.len()].copy_from_slice(self.title.as_bytes());
        Some((out, 12 + self.title.len()))
    }
}

/// Bytes of a window's pixel memory.
pub fn pixel_bytes(width: u16, height: u16) -> usize {
    usize::from(width) * usize::from(height) * 4
}

/// A file name `OPEN_FILE` accepts (ADR-0099): a path inside Home, of
/// 1 to [`MAX_OPEN_NAME`] bytes, its parts separated by `/`, none empty,
/// `.` or `..`, with no control characters.
pub fn open_name(data: &[u8]) -> Option<&str> {
    let name = core::str::from_utf8(data).ok()?;
    let ok = !name.is_empty()
        && name.len() <= MAX_OPEN_NAME
        && !name.chars().any(char::is_control)
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    ok.then_some(name)
}

/// Longest name `OPEN_FILE` takes, in bytes.
pub const MAX_OPEN_NAME: usize = 200;
