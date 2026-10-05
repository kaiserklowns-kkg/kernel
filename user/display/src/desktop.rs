//! The Oceans desktop (ADR-0057): what it shows, where, and what a click
//! hits. Drawing is a pure function of this state.
//!
//! The layout is anchored at the top left, so the launcher and dialogs sit
//! in the same place on every screen size. App windows (ADR-0059) float
//! over the Terminal, each in a frame the system draws: its title bar names
//! the app as Oceans Core verified it, and the window with the keyboard
//! focus (or the Terminal) has the focus colour.

use alloc::string::String;
use alloc::vec::Vec;

use oceans_window::{CLOSE_SIZE, Frame};

use crate::canvas::{Canvas, Font, Rect, Rgb};

// Design tokens (master spec §31): dark first, neutral surfaces, one
// restrained accent, semantic status colours.
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

pub const BAR_HEIGHT: i32 = 36;
pub const LAUNCHER_X: i32 = 16;
pub const LAUNCHER_WIDTH: i32 = 260;
pub const ITEMS_TOP: i32 = 96;
pub const ITEM_HEIGHT: i32 = 40;
pub const DIALOG: Rect = Rect::new(300, 120, 560, 260);
pub const ALLOW: Rect = Rect::new(716, 324, 120, 36);
pub const DENY: Rect = Rect::new(580, 324, 120, 36);

/// An installed app, as the launcher shows it.
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
    pub clock: String,
    pub status: String,
    pub terminal: Terminal,
    /// The keyboard types into the Terminal (no window has the focus).
    pub terminal_focused: bool,
}

/// An app window to draw.
pub struct WindowView<'a> {
    pub frame: &'a Frame,
    /// Its pixels, once the app has presented them.
    pub pixels: Option<*const u32>,
    pub focused: bool,
}

/// What a click landed on.
pub enum Hit {
    App(usize),
    Terminal,
    Allow,
    Deny,
    Nothing,
}

pub fn item_rect(index: usize) -> Rect {
    Rect::new(
        LAUNCHER_X + 8,
        ITEMS_TOP + index as i32 * ITEM_HEIGHT,
        LAUNCHER_WIDTH - 16,
        ITEM_HEIGHT - 4,
    )
}

fn launcher(height: i32) -> Rect {
    Rect::new(
        LAUNCHER_X,
        BAR_HEIGHT + 16,
        LAUNCHER_WIDTH,
        height - BAR_HEIGHT - 32,
    )
}

/// The Terminal window, which is also where app windows are placed.
pub fn terminal(width: i32, height: i32) -> Rect {
    let x = LAUNCHER_X + LAUNCHER_WIDTH + 16;
    Rect::new(x, BAR_HEIGHT + 16, width - x - 16, height - BAR_HEIGHT - 32)
}

impl Desktop {
    /// What a click on the desktop itself (behind every app window) hits,
    /// on a `width × height` screen.
    pub fn hit(&self, x: i32, y: i32, width: i32, height: i32) -> Hit {
        if self.dialog.is_some() {
            // A modal question: only its buttons answer.
            return if ALLOW.contains(x, y) {
                Hit::Allow
            } else if DENY.contains(x, y) {
                Hit::Deny
            } else {
                Hit::Nothing
            };
        }
        if let Some(app) = (0..self.apps.len()).find(|&i| item_rect(i).contains(x, y)) {
            return Hit::App(app);
        }
        if terminal(width, height).contains(x, y) {
            return Hit::Terminal;
        }
        Hit::Nothing
    }

    pub fn draw(&self, canvas: &mut Canvas, now_ms: u64, windows: &[WindowView<'_>]) {
        let (w, h) = (canvas.width, canvas.height);
        let screen = Rect::new(0, 0, w, h);
        canvas.fill(screen, BACKGROUND);

        // The Oceans Bar.
        let bar = Rect::new(0, 0, w, BAR_HEIGHT);
        canvas.fill(bar, SURFACE);
        canvas.fill(Rect::new(0, BAR_HEIGHT - 1, w, 1), BORDER);
        canvas.text(16, 9, "Oceans", Font::Strong, TEXT, bar);
        let clock_x = (w - Font::Strong.advance() * self.clock.len() as i32) / 2;
        canvas.text(clock_x, 9, &self.clock, Font::Strong, TEXT, bar);
        let status_x = w - 16 - Font::Body.advance() * self.status.len() as i32;
        canvas.text(status_x, 9, &self.status, Font::Body, MUTED, bar);

        // The launcher.
        let panel = launcher(h);
        canvas.fill(panel, SURFACE);
        canvas.outline(panel, BORDER);
        canvas.text(panel.x + 16, panel.y + 12, "Apps", Font::Title, TEXT, panel);
        if self.apps.is_empty() {
            canvas.text(
                panel.x + 16,
                ITEMS_TOP + 8,
                "No apps installed",
                Font::Body,
                MUTED,
                panel,
            );
        }
        for (i, app) in self.apps.iter().enumerate() {
            let item = item_rect(i);
            if item.y + item.h > panel.y + panel.h {
                break;
            }
            let hovered = item.contains(self.pointer.0, self.pointer.1) && self.dialog.is_none();
            canvas.fill(item, if hovered { SURFACE_RAISED } else { SURFACE });
            let dot = if app.running { SUCCESS } else { BORDER };
            canvas.fill(Rect::new(item.x + 10, item.y + 14, 8, 8), dot);
            let end = canvas.text(
                item.x + 28,
                item.y + 10,
                &app.name,
                Font::Strong,
                TEXT,
                item,
            );
            canvas.text(end + 8, item.y + 10, &app.version, Font::Body, MUTED, item);
        }

        // The Terminal: the console's text, the rows that fit, ending at
        // the cursor's.
        let window = terminal(w, h);
        canvas.fill(window, TERMINAL_BACKGROUND);
        canvas.outline(window, BORDER);
        let title = Rect::new(window.x, window.y, window.w, 28);
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
        let body = Rect::new(window.x + 8, window.y + 36, window.w - 16, window.h - 44);
        let t = &self.terminal;
        if t.cols > 0 {
            let visible = (body.h / Font::Body.height()).max(1) as usize;
            let last = (t.row + 1).min(t.rows);
            let first = last.saturating_sub(visible);
            for (line, row) in (first..last).enumerate() {
                let cells = &t.cells[row * t.cols..(row + 1) * t.cols];
                let text = core::str::from_utf8(cells).unwrap_or("").trim_end();
                let y = body.y + line as i32 * Font::Body.height();
                canvas.text(body.x, y, text, Font::Body, TEXT, body);
                if row == t.row {
                    let x = body.x + t.col as i32 * Font::Body.advance();
                    canvas.fill(
                        Rect::new(x, y + 2, Font::Body.advance(), Font::Body.height() - 2),
                        ACCENT,
                    );
                }
            }
        }

        // App windows, bottom to top.
        for view in windows {
            draw_window(canvas, view, self.pointer);
        }

        // A notification.
        if let Some(toast) = self.toast.as_ref().filter(|t| t.until_ms > now_ms) {
            let r = Rect::new(w - 376, BAR_HEIGHT + 12, 360, 48);
            canvas.fill(r, SURFACE_RAISED);
            canvas.outline(r, BORDER);
            canvas.fill(
                Rect::new(r.x, r.y, 4, r.h),
                if toast.error { DANGER } else { ACCENT },
            );
            canvas.text(r.x + 16, r.y + 16, &toast.text, Font::Body, TEXT, r);
        }

        // A permission dialog, over everything else.
        if let Some(dialog) = &self.dialog {
            canvas.dim(Rgb(0), 140);
            canvas.fill(DIALOG, DIALOG_SURFACE);
            canvas.outline(DIALOG, ACCENT);
            let (x, mut y) = (DIALOG.x + 24, DIALOG.y + 20);
            canvas.text(x, y, dialog.title, Font::Title, TEXT, DIALOG);
            y += 36;
            canvas.text(x, y, &dialog.app, Font::Body, MUTED, DIALOG);
            y += 26;
            canvas.text(x, y, dialog.ask, Font::Body, TEXT, DIALOG);
            y += 22;
            canvas.text(x + 16, y, &dialog.description, Font::Strong, TEXT, DIALOG);
            if !dialog.note.is_empty() {
                y += 30;
                canvas.text(x, y, &dialog.note, Font::Body, MUTED, DIALOG);
            }
            for (button, label, primary) in
                [(DENY, dialog.deny, false), (ALLOW, dialog.allow, true)]
            {
                let hovered = button.contains(self.pointer.0, self.pointer.1);
                let fill = match (primary, hovered) {
                    (true, false) => ACCENT,
                    (true, true) => Rgb(0x5a_b0_ff),
                    (false, false) => SURFACE_RAISED,
                    (false, true) => BORDER,
                };
                canvas.fill(button, fill);
                canvas.outline(button, if primary { ACCENT } else { BORDER });
                let tx = button.x + (button.w - Font::Strong.advance() * label.len() as i32) / 2;
                let color = if primary { BACKGROUND } else { TEXT };
                canvas.text(tx, button.y + 10, label, Font::Strong, color, button);
            }
        }

        canvas.cursor(self.pointer.0, self.pointer.1, TEXT, BACKGROUND);
    }
}

/// An app window: the frame the system draws, and the app's pixels inside.
fn draw_window(canvas: &mut Canvas, view: &WindowView<'_>, pointer: (i32, i32)) {
    let frame = view.frame;
    let outer = frame.outer();
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
    let close = frame.close_button();
    let text_clip = Rect::new(bar.x, bar.y, close.x - bar.x - 4, bar.h);
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
    let hovered = close.contains(pointer.0, pointer.1);
    if hovered {
        canvas.fill(close, DANGER);
    }
    let glyph_x = close.x + (CLOSE_SIZE - Font::Strong.advance()) / 2;
    canvas.text(
        glyph_x,
        close.y + (CLOSE_SIZE - Font::Strong.height()) / 2,
        "x",
        Font::Strong,
        if hovered { TEXT } else { MUTED },
        close,
    );
    let content = frame.content();
    match view.pixels {
        Some(pixels) if frame.presented => {
            canvas.blit(content, pixels, frame.width as usize);
        }
        _ => canvas.fill(content, BACKGROUND),
    }
}
