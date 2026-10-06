//! The Oceans desktop (ADR-0057, ADR-0076): what it shows, where, and what
//! a click hits. Drawing is a pure function of this state.
//!
//! The layout (ADR-0076), familiar from other desktops:
//! - a wallpaper, and the **taskbar** along the bottom: the Start button,
//!   the Terminal and the open app windows in the middle, the system's
//!   state and the clock on the right;
//! - the **Start menu**, above the Start button: the installed apps as
//!   tiles, the Terminal first;
//! - the **Terminal**, a window in the middle of the screen that can be
//!   minimized and brought back from the taskbar;
//! - **app windows** (ADR-0059) in frames the system draws: the title bar
//!   names the app as Oceans Core verified it, with minimize and close
//!   buttons; the window with the keyboard focus has the focus colour;
//! - notifications in the bottom right corner, and system dialogs in the
//!   middle, over everything.
//!
//! Every position is a function of the screen's size, so the smoke test
//! finds things where the layout puts them.

use alloc::string::String;
use alloc::vec::Vec;

use oceans_window::{CLOSE_SIZE, Frame, TITLE_HEIGHT};

use crate::canvas::{Canvas, Font, Rect, Rgb};

// Design tokens (master spec §31): dark first, neutral surfaces, one
// restrained accent, semantic status colours. The same as the web
// experience's (ui/).
pub const BACKGROUND: Rgb = Rgb(0x0b_12_20);
pub const SURFACE: Rgb = Rgb(0x12_1a_2b);
pub const SURFACE_RAISED: Rgb = Rgb(0x18_23_3a);
pub const BORDER: Rgb = Rgb(0x26_32_4a);
pub const TEXT: Rgb = Rgb(0xe6_ed_f6);
pub const MUTED: Rgb = Rgb(0x8a_97_ab);
pub const ACCENT: Rgb = Rgb(0x3b_9e_ff);
pub const SUCCESS: Rgb = Rgb(0x3f_b9_50);
pub const DANGER: Rgb = Rgb(0xf0_52_4f);
pub const TERMINAL_BACKGROUND: Rgb = Rgb(0x07_0b_14);
pub const DIALOG_SURFACE: Rgb = Rgb(0x1b_26_3f);
/// The title bar of the window (or Terminal) with the keyboard focus.
pub const FOCUS_TITLE: Rgb = Rgb(0x1f_3a_5f);
/// The wallpaper: deep water, lighter towards the bottom.
pub const WALLPAPER_TOP: Rgb = Rgb(0x0a_10_1d);
pub const WALLPAPER_BOTTOM: Rgb = Rgb(0x10_2b_4c);
/// The taskbar.
pub const TASKBAR: Rgb = SURFACE;
/// App icons' colours, chosen by the app's id.
const ICON_COLOURS: [Rgb; 6] = [
    Rgb(0x3b_9e_ff),
    Rgb(0x7c_5c_ff),
    Rgb(0x2f_bf_9b),
    Rgb(0xf2_99_4a),
    Rgb(0xe5_48_7a),
    Rgb(0x56_c2_6a),
];

pub const TASKBAR_HEIGHT: i32 = 48;
/// A taskbar button (Start, the Terminal, a window): square.
pub const ITEM: i32 = 40;
const ITEM_GAP: i32 = 4;
const START_WIDTH: i32 = 560;
const START_HEIGHT: i32 = 420;
const TILE_COLUMNS: usize = 5;
const TILE_WIDTH: i32 = 102;
const TILE_HEIGHT: i32 = 100;
/// Tiles shown: the Terminal and up to 14 apps (three rows).
const MAX_TILES: usize = 15;
const DIALOG_WIDTH: i32 = 560;
const DIALOG_HEIGHT: i32 = 260;
const BUTTON_WIDTH: i32 = 120;
const BUTTON_HEIGHT: i32 = 36;

/// An installed app, as the Start menu shows it.
pub struct App {
    pub id: String,
    pub name: String,
    pub version: String,
    pub running: bool,
}

/// A short message in the corner.
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until_ms: u64,
}

/// What a dialog's answer goes to.
pub enum Question {
    /// A permission of app `id` (ADR-0047).
    Permission { id: String, permission: u8 },
    /// Installing a package proposed through the Store (ADR-0061).
    Install { number: u32, name: String },
}

/// A question to the user, drawn by the system.
pub struct Dialog {
    pub question: Question,
    pub title: &'static str,
    /// "Hello (app.oceans.hello 1.0.0, from Oceans Examples)"
    pub app: String,
    /// "asks to:"
    pub ask: &'static str,
    /// What is asked, in the system's words.
    pub description: String,
    /// One more line, muted (the app's reason, quoted; a description).
    pub note: String,
    pub deny: &'static str,
    pub allow: &'static str,
}

/// The console's text, mirrored in the Terminal window.
#[derive(Default)]
pub struct Terminal {
    pub cols: usize,
    pub rows: usize,
    pub col: usize,
    pub row: usize,
    pub generation: u64,
    pub cells: Vec<u8>,
}

#[derive(Default)]
pub struct Desktop {
    pub pointer: (i32, i32),
    pub apps: Vec<App>,
    pub toast: Option<Toast>,
    pub dialog: Option<Dialog>,
    /// "14:03"
    pub clock: String,
    /// "2026-10-05 UTC"
    pub date: String,
    pub status: String,
    pub terminal: Terminal,
    /// The keyboard types into the Terminal (no window has the focus).
    pub terminal_focused: bool,
    /// The Terminal is minimized to the taskbar.
    pub terminal_hidden: bool,
    pub start_open: bool,
}

/// An app window to draw.
pub struct WindowView<'a> {
    pub frame: &'a Frame,
    /// Its pixels, once the app has presented them.
    pub pixels: Option<*const u32>,
    pub focused: bool,
}

/// What a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The Start button.
    Start,
    /// The Terminal's tile in the Start menu.
    StartTerminal,
    /// An app's tile in the Start menu.
    StartApp(usize),
    /// Somewhere else while the Start menu is open: it closes.
    Outside,
    /// The Terminal's taskbar button.
    TaskbarTerminal,
    /// A window's taskbar button.
    TaskbarWindow(u32),
    /// The Terminal's minimize button.
    TerminalMinimize,
    /// The Terminal itself.
    Terminal,
    Allow,
    Deny,
    Nothing,
}

// ---- The layout, a function of the screen's size -------------------------

/// Where windows go: everything above the taskbar.
pub fn area(width: i32, height: i32) -> Rect {
    Rect::new(0, 0, width, height - TASKBAR_HEIGHT)
}

pub fn taskbar(width: i32, height: i32) -> Rect {
    Rect::new(0, height - TASKBAR_HEIGHT, width, TASKBAR_HEIGHT)
}

/// Taskbar button `index` of `count` (Start, the Terminal, then the
/// windows), centred.
pub fn taskbar_item(width: i32, height: i32, index: usize, count: usize) -> Rect {
    let total = count as i32 * ITEM + (count as i32 - 1).max(0) * ITEM_GAP;
    let x = (width - total) / 2 + index as i32 * (ITEM + ITEM_GAP);
    Rect::new(
        x,
        height - TASKBAR_HEIGHT + (TASKBAR_HEIGHT - ITEM) / 2,
        ITEM,
        ITEM,
    )
}

/// The Terminal window, in the middle of the area.
pub fn terminal(width: i32, height: i32) -> Rect {
    let area = area(width, height);
    let w = (area.w - 160).min(960);
    let h = (area.h - 96).min(600);
    Rect::new((area.w - w) / 2, (area.h - h) / 2, w, h)
}

pub fn terminal_minimize(width: i32, height: i32) -> Rect {
    let window = terminal(width, height);
    Rect::new(
        window.x + window.w - CLOSE_SIZE - 4,
        window.y + (TITLE_HEIGHT - CLOSE_SIZE) / 2,
        CLOSE_SIZE,
        CLOSE_SIZE,
    )
}

/// The Start menu, above the Start button.
pub fn start_menu(width: i32, height: i32) -> Rect {
    Rect::new(
        (width - START_WIDTH) / 2,
        height - TASKBAR_HEIGHT - 12 - START_HEIGHT,
        START_WIDTH,
        START_HEIGHT,
    )
}

/// Tile `index` of the Start menu (0: the Terminal; then the apps).
pub fn tile(width: i32, height: i32, index: usize) -> Rect {
    let menu = start_menu(width, height);
    let (col, row) = (index % TILE_COLUMNS, index / TILE_COLUMNS);
    Rect::new(
        menu.x + 24 + col as i32 * TILE_WIDTH,
        menu.y + 64 + row as i32 * TILE_HEIGHT,
        TILE_WIDTH,
        TILE_HEIGHT,
    )
}

/// A system dialog, in the middle of the area.
pub fn dialog(width: i32, height: i32) -> Rect {
    let area = area(width, height);
    Rect::new(
        (area.w - DIALOG_WIDTH) / 2,
        (area.h - DIALOG_HEIGHT) / 2,
        DIALOG_WIDTH,
        DIALOG_HEIGHT,
    )
}

pub fn allow(width: i32, height: i32) -> Rect {
    let d = dialog(width, height);
    Rect::new(d.x + 416, d.y + 204, BUTTON_WIDTH, BUTTON_HEIGHT)
}

pub fn deny(width: i32, height: i32) -> Rect {
    let d = dialog(width, height);
    Rect::new(d.x + 280, d.y + 204, BUTTON_WIDTH, BUTTON_HEIGHT)
}

/// The windows the taskbar shows, in the order they were opened.
pub fn taskbar_windows<'a>(windows: &'a [WindowView<'a>]) -> Vec<&'a WindowView<'a>> {
    let mut shown: Vec<&WindowView<'_>> = windows.iter().collect();
    shown.sort_unstable_by_key(|view| view.frame.id);
    shown
}

impl Desktop {
    /// What a click on the desktop (the taskbar, the Start menu, dialogs,
    /// the Terminal: everything but app windows) hits, on a `width ×
    /// height` screen; `windows` as the taskbar shows them.
    pub fn hit(&self, x: i32, y: i32, width: i32, height: i32, windows: &[u32]) -> Hit {
        if self.dialog.is_some() {
            // A modal question: only its buttons answer.
            return if allow(width, height).contains(x, y) {
                Hit::Allow
            } else if deny(width, height).contains(x, y) {
                Hit::Deny
            } else {
                Hit::Nothing
            };
        }
        let count = 2 + windows.len();
        if taskbar_item(width, height, 0, count).contains(x, y) {
            return Hit::Start;
        }
        if self.start_open {
            if start_menu(width, height).contains(x, y) {
                if tile(width, height, 0).contains(x, y) {
                    return Hit::StartTerminal;
                }
                let shown = self.apps.len().min(MAX_TILES - 1);
                return (0..shown)
                    .find(|&i| tile(width, height, i + 1).contains(x, y))
                    .map_or(Hit::Nothing, Hit::StartApp);
            }
            return Hit::Outside;
        }
        if taskbar_item(width, height, 1, count).contains(x, y) {
            return Hit::TaskbarTerminal;
        }
        if let Some(&id) = windows
            .iter()
            .enumerate()
            .find(|&(i, _)| taskbar_item(width, height, i + 2, count).contains(x, y))
            .map(|(_, id)| id)
        {
            return Hit::TaskbarWindow(id);
        }
        if !self.terminal_hidden {
            if terminal_minimize(width, height).contains(x, y) {
                return Hit::TerminalMinimize;
            }
            if terminal(width, height).contains(x, y) {
                return Hit::Terminal;
            }
        }
        Hit::Nothing
    }

    pub fn draw(&self, canvas: &mut Canvas, now_ms: u64, windows: &[WindowView<'_>]) {
        let (w, h) = (canvas.width, canvas.height);
        canvas.gradient(area(w, h), WALLPAPER_TOP, WALLPAPER_BOTTOM);

        if !self.terminal_hidden {
            self.draw_terminal(canvas);
        }

        // App windows, bottom to top.
        for view in windows.iter().filter(|view| !view.frame.minimized) {
            draw_window(canvas, view, self.pointer);
        }

        self.draw_taskbar(canvas, windows);
        if self.start_open {
            self.draw_start_menu(canvas);
        }

        // A notification, above the taskbar's right end.
        if let Some(toast) = self.toast.as_ref().filter(|t| t.until_ms > now_ms) {
            let r = Rect::new(w - 376, h - TASKBAR_HEIGHT - 12 - 56, 360, 56);
            canvas.darken(Rect::new(r.x + 2, r.y + 4, r.w, r.h), 90);
            canvas.round_fill(r, 8, SURFACE_RAISED);
            let mark = if toast.error { DANGER } else { ACCENT };
            canvas.round_fill(Rect::new(r.x + 10, r.y + 14, 4, r.h - 28), 2, mark);
            canvas.text(r.x + 24, r.y + 20, &toast.text, Font::Body, TEXT, r);
        }

        // A system dialog, over everything else.
        if let Some(dialog) = &self.dialog {
            self.draw_dialog(canvas, dialog);
        }

        canvas.cursor(self.pointer.0, self.pointer.1, TEXT, BACKGROUND);
    }

    /// The Terminal: the console's text, the rows that fit, ending at the
    /// cursor's.
    fn draw_terminal(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        let window = terminal(w, h);
        canvas.darken(
            Rect::new(window.x + 4, window.y + 8, window.w, window.h),
            110,
        );
        canvas.fill(window, TERMINAL_BACKGROUND);
        canvas.outline(window, BORDER);
        let title = Rect::new(window.x + 1, window.y + 1, window.w - 2, TITLE_HEIGHT - 1);
        canvas.fill(
            title,
            if self.terminal_focused {
                FOCUS_TITLE
            } else {
                SURFACE_RAISED
            },
        );
        canvas.text(
            window.x + 12,
            window.y + 6,
            "Terminal",
            Font::Strong,
            TEXT,
            title,
        );
        title_button(
            canvas,
            terminal_minimize(w, h),
            Glyph::Minimize,
            self.pointer,
        );
        let body = Rect::new(window.x + 8, window.y + 36, window.w - 16, window.h - 44);
        let t = &self.terminal;
        if t.cols > 0 {
            let visible = (body.h / Font::Mono.height()).max(1) as usize;
            let last = (t.row + 1).min(t.rows);
            let first = last.saturating_sub(visible);
            for (line, row) in (first..last).enumerate() {
                let cells = &t.cells[row * t.cols..(row + 1) * t.cols];
                let text = core::str::from_utf8(cells).unwrap_or("").trim_end();
                let y = body.y + line as i32 * Font::Mono.height();
                canvas.text(body.x, y, text, Font::Mono, TEXT, body);
                if row == t.row {
                    let x = body.x + t.col as i32 * Font::Mono.advance();
                    canvas.fill(
                        Rect::new(x, y + 2, Font::Mono.advance(), Font::Mono.height() - 2),
                        ACCENT,
                    );
                }
            }
        }
    }

    fn draw_taskbar(&self, canvas: &mut Canvas, windows: &[WindowView<'_>]) {
        let (w, h) = (canvas.width, canvas.height);
        let bar = taskbar(w, h);
        canvas.fill(bar, TASKBAR);
        canvas.fill(Rect::new(0, bar.y, w, 1), BORDER);
        let shown = taskbar_windows(windows);
        let count = 2 + shown.len();
        let pointer = self.pointer;
        let hovered = |r: Rect| r.contains(pointer.0, pointer.1) && self.dialog.is_none();

        // Start.
        let start = taskbar_item(w, h, 0, count);
        if self.start_open || hovered(start) {
            canvas.round_fill(start, 8, SURFACE_RAISED);
        }
        oceans_logo(canvas, start);

        // The Terminal.
        let item = taskbar_item(w, h, 1, count);
        if hovered(item) {
            canvas.round_fill(item, 8, SURFACE_RAISED);
        }
        terminal_icon(canvas, inset(item, 6));
        indicator(
            canvas,
            item,
            !self.terminal_hidden,
            self.terminal_focused && !self.terminal_hidden,
        );

        // The windows.
        for (i, view) in shown.iter().enumerate() {
            let item = taskbar_item(w, h, i + 2, count);
            if hovered(item) {
                canvas.round_fill(item, 8, SURFACE_RAISED);
            }
            app_icon(canvas, inset(item, 6), &view.frame.app);
            indicator(canvas, item, !view.frame.minimized, view.focused);
        }

        // The system's state and the clock, on the right.
        let right = w - 16;
        let time_x = right - canvas.measure(&self.clock, Font::Strong);
        canvas.text(time_x, bar.y + 6, &self.clock, Font::Strong, TEXT, bar);
        let date_x = right - canvas.measure(&self.date, Font::Body);
        canvas.text(date_x, bar.y + 25, &self.date, Font::Body, MUTED, bar);
        let status_x = time_x.min(date_x) - 24 - canvas.measure(&self.status, Font::Body);
        canvas.text(status_x, bar.y + 16, &self.status, Font::Body, MUTED, bar);
        canvas.text(16, bar.y + 16, "Oceans", Font::Strong, MUTED, bar);
    }

    fn draw_start_menu(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        let menu = start_menu(w, h);
        canvas.darken(Rect::new(menu.x + 4, menu.y + 8, menu.w, menu.h), 120);
        canvas.round_fill(
            Rect::new(menu.x - 1, menu.y - 1, menu.w + 2, menu.h + 2),
            13,
            BORDER,
        );
        canvas.round_fill(menu, 12, SURFACE);
        canvas.text(menu.x + 24, menu.y + 22, "Apps", Font::Title, TEXT, menu);
        let pointer = self.pointer;
        let tile_frame = |canvas: &mut Canvas, index: usize| {
            let t = tile(w, h, index);
            if t.contains(pointer.0, pointer.1) && self.dialog.is_none() {
                canvas.round_fill(inset(t, 2), 8, SURFACE_RAISED);
            }
            t
        };
        // The Terminal first.
        let t = tile_frame(canvas, 0);
        terminal_icon(canvas, icon_in(t));
        tile_label(canvas, t, "Terminal");
        for (i, app) in self.apps.iter().take(MAX_TILES - 1).enumerate() {
            let t = tile_frame(canvas, i + 1);
            let icon = icon_in(t);
            app_icon(canvas, icon, &app.name);
            if app.running {
                canvas.round_fill(
                    Rect::new(icon.x + icon.w - 8, icon.y - 2, 10, 10),
                    5,
                    SUCCESS,
                );
            }
            tile_label(canvas, t, &app.name);
        }
        let footer_y = menu.y + menu.h - 48;
        canvas.fill(Rect::new(menu.x + 1, footer_y, menu.w - 2, 1), BORDER);
        let note = if self.apps.is_empty() {
            String::from("No apps installed yet: the Store, or `app install`")
        } else if self.apps.len() > MAX_TILES - 1 {
            alloc::format!(
                "{} apps installed; more in the Terminal: app list",
                self.apps.len()
            )
        } else {
            alloc::format!(
                "{} app{} installed",
                self.apps.len(),
                if self.apps.len() == 1 { "" } else { "s" }
            )
        };
        canvas.text(menu.x + 24, footer_y + 16, &note, Font::Body, MUTED, menu);
    }

    fn draw_dialog(&self, canvas: &mut Canvas, dialog: &Dialog) {
        let (w, h) = (canvas.width, canvas.height);
        let r = self::dialog(w, h);
        canvas.dim(Rgb(0), 140);
        canvas.round_fill(Rect::new(r.x - 1, r.y - 1, r.w + 2, r.h + 2), 13, ACCENT);
        canvas.round_fill(r, 12, DIALOG_SURFACE);
        let (x, mut y) = (r.x + 24, r.y + 20);
        canvas.text(x, y, dialog.title, Font::Title, TEXT, r);
        y += 36;
        canvas.text(x, y, &dialog.app, Font::Body, MUTED, r);
        y += 26;
        canvas.text(x, y, dialog.ask, Font::Body, TEXT, r);
        y += 22;
        canvas.text(x + 16, y, &dialog.description, Font::Strong, TEXT, r);
        if !dialog.note.is_empty() {
            y += 30;
            canvas.text(x, y, &dialog.note, Font::Body, MUTED, r);
        }
        for (button, label, primary) in [
            (deny(w, h), dialog.deny, false),
            (allow(w, h), dialog.allow, true),
        ] {
            let hovered = button.contains(self.pointer.0, self.pointer.1);
            let fill = match (primary, hovered) {
                (true, false) => ACCENT,
                (true, true) => Rgb(0x5a_b0_ff),
                (false, false) => SURFACE_RAISED,
                (false, true) => BORDER,
            };
            canvas.round_fill(button, 8, fill);
            let tx = button.x + (button.w - canvas.measure(label, Font::Strong)) / 2;
            let color = if primary { BACKGROUND } else { TEXT };
            canvas.text(tx, button.y + 10, label, Font::Strong, color, button);
        }
    }
}

fn inset(r: Rect, by: i32) -> Rect {
    Rect::new(r.x + by, r.y + by, r.w - 2 * by, r.h - 2 * by)
}

/// The icon of a Start menu tile: 48 pixels, centred at its top.
fn icon_in(tile: Rect) -> Rect {
    Rect::new(tile.x + (tile.w - 48) / 2, tile.y + 12, 48, 48)
}

/// A tile's name, centred under its icon, cut to fit.
fn tile_label(canvas: &mut Canvas, tile: Rect, name: &str) {
    let room = tile.w - 8;
    let mut shown = String::from(name);
    if canvas.measure(&shown, Font::Body) > room {
        // Shortened, with an ellipsis of dots.
        while !shown.is_empty()
            && canvas.measure(&shown, Font::Body) + canvas.measure("..", Font::Body) > room
        {
            shown.pop();
        }
        shown.push_str("..");
    }
    let width = canvas.measure(&shown, Font::Body);
    canvas.text(
        tile.x + (tile.w - width) / 2,
        tile.y + 68,
        &shown,
        Font::Body,
        TEXT,
        tile,
    );
}

/// The open/focused mark under a taskbar button.
fn indicator(canvas: &mut Canvas, item: Rect, open: bool, focused: bool) {
    if !open {
        return;
    }
    let (width, colour) = if focused { (16, ACCENT) } else { (6, MUTED) };
    canvas.round_fill(
        Rect::new(item.x + (item.w - width) / 2, item.y + item.h - 3, width, 3),
        1,
        colour,
    );
}

/// An app's icon: a rounded square in its colour (chosen by its name, which
/// the taskbar and the Start menu both know), with its initial.
fn app_icon(canvas: &mut Canvas, r: Rect, name: &str) {
    let hash = name.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    });
    canvas.round_fill(r, r.w / 4, ICON_COLOURS[hash % ICON_COLOURS.len()]);
    // The first letter, Thai included (ADR-0077).
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .into_iter()
        .collect();
    let font = if r.w >= 40 { Font::Title } else { Font::Strong };
    let width = canvas.measure(&initial, font);
    canvas.text(
        r.x + (r.w - width) / 2,
        r.y + (r.h - font.height()) / 2,
        &initial,
        font,
        TEXT,
        r,
    );
}

/// The Terminal's icon: a dark square with a prompt.
fn terminal_icon(canvas: &mut Canvas, r: Rect) {
    canvas.round_fill(r, r.w / 4, BORDER);
    canvas.round_fill(inset(r, 1), r.w / 4 - 1, TERMINAL_BACKGROUND);
    let font = if r.w >= 40 { Font::Title } else { Font::Strong };
    let width = canvas.measure(">_", font);
    canvas.text(
        r.x + (r.w - width) / 2,
        r.y + (r.h - font.height()) / 2,
        ">_",
        font,
        ACCENT,
        r,
    );
}

/// The Oceans mark on the Start button: two waves.
fn oceans_logo(canvas: &mut Canvas, item: Rect) {
    const WAVE: [i32; 10] = [0, -1, -2, -2, -1, 0, 1, 2, 2, 1];
    let x0 = item.x + (item.w - 20) / 2;
    for (line, y0) in [item.y + 15, item.y + 24].into_iter().enumerate() {
        let colour = if line == 0 { ACCENT } else { Rgb(0x7c_c4_ff) };
        for dx in 0..20 {
            let y = y0 + WAVE[dx as usize % WAVE.len()];
            canvas.fill(Rect::new(x0 + dx, y, 1, 3), colour);
        }
    }
}

enum Glyph {
    Minimize,
    Close,
}

/// A title bar button: minimize (a line) or close (an x).
fn title_button(canvas: &mut Canvas, r: Rect, glyph: Glyph, pointer: (i32, i32)) {
    let hovered = r.contains(pointer.0, pointer.1);
    if hovered {
        canvas.round_fill(
            r,
            4,
            match glyph {
                Glyph::Close => DANGER,
                Glyph::Minimize => BORDER,
            },
        );
    }
    let colour = if hovered { TEXT } else { MUTED };
    match glyph {
        Glyph::Minimize => canvas.fill(Rect::new(r.x + 6, r.y + r.h / 2, r.w - 12, 2), colour),
        Glyph::Close => {
            for i in 0..(r.w - 12) {
                canvas.fill(Rect::new(r.x + 6 + i, r.y + 6 + i, 2, 1), colour);
                canvas.fill(Rect::new(r.x + r.w - 8 - i, r.y + 6 + i, 2, 1), colour);
            }
        }
    }
}

/// An app window: a shadow, the frame the system draws, and the app's
/// pixels inside.
fn draw_window(canvas: &mut Canvas, view: &WindowView<'_>, pointer: (i32, i32)) {
    let frame = view.frame;
    let outer = frame.outer();
    canvas.darken(Rect::new(outer.x + 4, outer.y + 8, outer.w, outer.h), 110);
    canvas.fill(outer, TERMINAL_BACKGROUND);
    canvas.outline(outer, if view.focused { ACCENT } else { BORDER });
    let bar = frame.title_bar();
    let bar_inside = Rect::new(bar.x + 1, bar.y + 1, bar.w - 2, bar.h - 1);
    canvas.fill(
        bar_inside,
        if view.focused {
            FOCUS_TITLE
        } else {
            SURFACE_RAISED
        },
    );
    // The app's verified name first, then its own title.
    let minimize = frame.minimize_button();
    let text_clip = Rect::new(bar.x, bar.y, minimize.x - bar.x - 4, bar.h);
    let end = canvas.text(
        bar.x + 10,
        bar.y + 6,
        &frame.app,
        Font::Strong,
        TEXT,
        text_clip,
    );
    if !frame.title.is_empty() {
        canvas.text(
            end + 8,
            bar.y + 6,
            &frame.title,
            Font::Body,
            MUTED,
            text_clip,
        );
    }
    title_button(canvas, minimize, Glyph::Minimize, pointer);
    title_button(canvas, frame.close_button(), Glyph::Close, pointer);
    let content = frame.content();
    match view.pixels {
        Some(pixels) if frame.presented => {
            canvas.blit(content, pixels, frame.width as usize);
        }
        _ => canvas.fill(content, BACKGROUND),
    }
}
