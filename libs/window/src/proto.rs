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
    /// data = `[window u32]` and a [`super::Pixels`], handles = `[pixels]`.
    /// The size asked for is in logical pixels (ADR-0136); the pixels are
    /// the screen's, at the scale the reply says.
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
    /// data = a [`super::Pixels`], handles = `[pixels]`: new pixels
    /// at the window's size now, and the scale now (ADR-0136: it comes
    /// after the user changes the scale too), which replace the old ones.
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
    /// data = `[window u32]`: the media keys (Play/Pause, Stop, Previous,
    /// Next; ADR-0102) come to this window as `KEY` events from now on,
    /// whatever has the focus, until another app asks for them; when the
    /// user focuses a window of an app that asked, they go to it again.
    pub const MEDIA_KEYS: u64 = 12;
    /// data = `[window u32][state u8][title]`: what the player plays, for
    /// the desktop to show (ADR-0103): [`super::playing`] state, and a
    /// title of 1 to [`super::MAX_NOW_PLAYING`] bytes (none when stopped
    /// and empty). Only for a window that asked for the media keys.
    pub const NOW_PLAYING: u64 = 13;
    /// → a [`super::Screen`] (ADR-0136): the screen's pixels, the scale in
    /// use and the one the screen's density suggests, for Settings to offer
    /// the scales that fit.
    pub const SCREEN: u64 = 14;
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
/// Up to a 4K screen, so a window can fill one (ADR-0107; 1024 × 768
/// before). The display service's pixel budget bounds what all windows
/// take together.
pub const MAX_WIDTH: u16 = 3840;
pub const MAX_HEIGHT: u16 = 2160;
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
/// The volume keys (`oceans_abi::display::KEY_MUTE`, `KEY_VOLUME_DOWN`,
/// `KEY_VOLUME_UP`, ADR-0101): the desktop's, never an app's.
pub const KEY_MUTE: u8 = 0x8c;
pub const KEY_VOLUME_DOWN: u8 = 0x8d;
pub const KEY_VOLUME_UP: u8 = 0x8e;
/// The media keys (`oceans_abi::display::KEY_PLAY_PAUSE` … `KEY_NEXT`,
/// ADR-0102): to the app that asked for them (`MEDIA_KEYS`).
pub const KEY_PLAY_PAUSE: u8 = 0xa0;
pub const KEY_STOP: u8 = 0xa1;
pub const KEY_PREVIOUS: u8 = 0xa2;
pub const KEY_NEXT: u8 = 0xa3;
/// Super with Left, Right, Up and Down, and Super+F
/// (`oceans_abi::display::KEY_TILE_LEFT` … `KEY_FULL_SCREEN`, ADR-0107):
/// the focused window to a half, all of the area, back, full screen. The
/// desktop's, never an app's.
pub const KEY_TILE_LEFT: u8 = 0xa4;
pub const KEY_TILE_RIGHT: u8 = 0xa5;
pub const KEY_TILE_UP: u8 = 0xa6;
pub const KEY_TILE_DOWN: u8 = 0xa7;
pub const KEY_FULL_SCREEN: u8 = 0xa8;
/// Super pressed and let go alone (`oceans_abi::display::KEY_SEARCH`,
/// ADR-0108): the desktop's launcher search. The desktop's, never an
/// app's.
pub const KEY_SEARCH: u8 = 0xa9;
/// Super+L (`oceans_abi::display::KEY_LOCK`, ADR-0110): the desktop's
/// lock screen. Never an app's.
pub const KEY_LOCK: u8 = 0xaa;
/// Super+Space (`oceans_abi::display::KEY_LAYOUT`, ADR-0114): the next
/// keyboard layout. The desktop's, never an app's.
pub const KEY_LAYOUT: u8 = 0xab;
/// Up and Down (`oceans_abi::display::KEY_UP`, `KEY_DOWN`): the search
/// results they choose between while the launcher is open.
pub const KEY_UP: u8 = 0x80;
pub const KEY_DOWN: u8 = 0x81;
/// Left and Right (`KEY_LEFT`, `KEY_RIGHT`): the account they choose
/// between at the lock screen (ADR-0115).
pub const KEY_LEFT: u8 = 0x82;
pub const KEY_RIGHT: u8 = 0x83;

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
    /// The theme to draw with (ADR-0113): `key` the appearance (0 light,
    /// 1 dark), `button` the accent (`oceans_draw::theme::ACCENTS`). Sent
    /// when the window opens and whenever the user changes it.
    pub const THEME: u8 = 8;
    /// A character typed in a layout other than English (ADR-0114): `x`
    /// and `y` are the low and high 16 bits of its Unicode scalar value.
    /// Printable keys come so instead of as `KEY` while such a layout is
    /// in use; the others still come as `KEY`.
    pub const TEXT: u8 = 9;
    /// The wheel turned over the window (ADR-0138), whether it has the
    /// focus or not: `x` and `y` where the pointer is inside it; `key`
    /// the detents down the page (an `i8`: positive scrolls towards its
    /// end, as a wheel turned towards the user does), `button` those to
    /// the right.
    pub const WHEEL: u8 = 10;
}

impl Event {
    /// A `TEXT` event's character.
    pub fn character(&self) -> Option<char> {
        if self.kind != kind::TEXT {
            return None;
        }
        char::from_u32(u32::from(self.x as u16) | u32::from(self.y as u16) << 16)
    }

    /// A `WHEEL` event's detents: down the page, and to the right.
    pub fn wheel(&self) -> Option<(i32, i32)> {
        (self.kind == kind::WHEEL)
            .then(|| (i32::from(self.key as i8), i32::from(self.button as i8)))
    }
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
        (kind::KEY..=kind::WHEEL)
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

/// A window's pixels, as `OPEN` and `RESIZE` hand them over (ADR-0136):
/// `width × height` of the screen's pixels, for content of the window's
/// logical size drawn at `scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u16,
    pub height: u16,
    pub scale: crate::Scale,
}

impl Pixels {
    pub const SIZE: usize = 6;

    /// The pixels for content of `width × height` logical pixels at
    /// `scale`.
    pub fn of(width: u16, height: u16, scale: crate::Scale) -> Self {
        let device = |v: u16| scale.len(i32::from(v)).clamp(1, i32::from(u16::MAX)) as u16;
        Self {
            width: device(width),
            height: device(height),
            scale,
        }
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[..2].copy_from_slice(&self.width.to_le_bytes());
        out[2..4].copy_from_slice(&self.height.to_le_bytes());
        out[4..].copy_from_slice(&self.scale.percent().to_le_bytes());
        out
    }

    /// `None` if short or of a scale not offered.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let word = |i: usize| Some(u16::from_le_bytes(bytes.get(i..i + 2)?.try_into().ok()?));
        Some(Self {
            width: word(0)?,
            height: word(2)?,
            scale: crate::Scale::new(word(4)?)?,
        })
    }

    pub fn bytes(&self) -> usize {
        pixel_bytes(self.width, self.height)
    }
}

/// The screen, as `SCREEN` says (ADR-0136).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Its own pixels.
    pub width: u16,
    pub height: u16,
    /// The scale drawn at, and the one automatic chooses for it.
    pub scale: crate::Scale,
    pub automatic: crate::Scale,
}

impl Screen {
    pub const SIZE: usize = 8;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        for (i, word) in [
            self.width,
            self.height,
            self.scale.percent(),
            self.automatic.percent(),
        ]
        .into_iter()
        .enumerate()
        {
            out[2 * i..2 * i + 2].copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let word = |i: usize| Some(u16::from_le_bytes(bytes.get(i..i + 2)?.try_into().ok()?));
        Some(Self {
            width: word(0)?,
            height: word(2)?,
            scale: crate::Scale::new(word(4)?)?,
            automatic: crate::Scale::new(word(6)?)?,
        })
    }

    /// The scales it can take (`crate::Scale::choices`).
    pub fn choices(&self) -> impl Iterator<Item = crate::Scale> {
        crate::Scale::choices(i32::from(self.width), i32::from(self.height))
    }
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

/// What a player reports (`NOW_PLAYING`, ADR-0103).
pub mod playing {
    pub const STOPPED: u8 = 0;
    pub const PLAYING: u8 = 1;
    pub const PAUSED: u8 = 2;
}

/// Longest title a player reports, in bytes.
pub const MAX_NOW_PLAYING: usize = 80;

/// A `NOW_PLAYING` request's state and title, if acceptable: a known state,
/// and a title of UTF-8 on one line, at most [`MAX_NOW_PLAYING`] bytes
/// (empty only when stopped).
pub fn now_playing(data: &[u8]) -> Option<(u8, &str)> {
    let (&state, title) = data.split_first()?;
    let title = core::str::from_utf8(title).ok()?;
    let ok = state <= playing::PAUSED
        && title.len() <= MAX_NOW_PLAYING
        && !title.chars().any(char::is_control)
        && (!title.trim().is_empty() || state == playing::STOPPED);
    ok.then_some((state, title.trim()))
}
