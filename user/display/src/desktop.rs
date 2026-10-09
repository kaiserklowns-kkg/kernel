//! The Oceans desktop (ADR-0057, ADR-0076, ADR-0078): what it shows, where,
//! and what a click hits. Drawing is a pure function of this state.
//!
//! The look (ADR-0078): light, translucent and rounded, after the macOS
//! Big Sur design language, drawn in Oceans' own shapes and colours:
//! - an ocean wallpaper (a deep-to-shallow gradient with waves);
//! - the **menu bar** along the top: the Oceans mark (it opens the apps),
//!   the app in use, the system's state and the date and time;
//! - the **dock**, a floating translucent shelf at the bottom: the apps
//!   button, the Terminal and the open windows, with a dot under what is
//!   open and the name in a label over the one pointed at;
//! - the **apps** panel above the dock: the Terminal and the installed
//!   apps as tiles, and Restart and Shut Down (ADR-0085), each confirmed
//!   in a system dialog;
//! - **windows** (the Terminal's too) with light title bars, the title in
//!   the middle and round buttons at the left: close, minimize and
//!   resize (not yet available, drawn disabled);
//! - notifications at the top right, system dialogs in the middle.
//!
//! Every position is a function of the screen's size, so the smoke test
//! finds things where the layout puts them.

use alloc::string::String;
use alloc::vec::Vec;

use oceans_window::{BUTTON_STEP, CLOSE_SIZE, Frame, TITLE_HEIGHT};

use crate::canvas::{Canvas, Font, Rect, Rgb};

// Design tokens (master spec §31; ADR-0078): light surfaces, dark text,
// one accent, the window buttons' signal colours.
pub const WHITE: Rgb = Rgb(0xff_ff_ff);
pub const BLACK: Rgb = Rgb(0x00_00_00);
pub const TEXT: Rgb = Rgb(0x1d_1d_1f);
pub const MUTED: Rgb = Rgb(0x6e_6e_73);
pub const ACCENT: Rgb = Rgb(0x2f_7c_f6);
pub const SUCCESS: Rgb = Rgb(0x34_c7_59);
pub const CLOSE: Rgb = Rgb(0xff_5f_57);
pub const MINIMIZE: Rgb = Rgb(0xfe_bc_2e);
/// The zoom button of a window that can be resized (ADR-0097).
pub const ZOOM: Rgb = Rgb(0x28_c8_40);
/// A window button that does nothing (yet), or any button of a window
/// without the focus.
pub const BUTTON_OFF: Rgb = Rgb(0xd1_d1_d6);
/// Title bars: of the window (or Terminal) with the keyboard focus, and
/// of the others.
pub const FOCUS_TITLE: Rgb = Rgb(0xe3_e3_e8);
pub const IDLE_TITLE: Rgb = Rgb(0xf6_f6_f8);
pub const WINDOW_BORDER: Rgb = Rgb(0xb4_b4_bc);
pub const TERMINAL_BACKGROUND: Rgb = Rgb(0x1c_1c_1e);
pub const TERMINAL_TEXT: Rgb = Rgb(0xe8_e8_ed);
/// The apps panel.
pub const PANEL: Rgb = Rgb(0xf2_f2_f7);
pub const DIALOG_SURFACE: Rgb = Rgb(0xf9_f9_fb);
pub const BUTTON_SECONDARY: Rgb = Rgb(0xe3_e3_e8);
/// App icons' colours, chosen by the app's name.
const ICON_COLOURS: [Rgb; 6] = [
    Rgb(0x2f_7c_f6),
    Rgb(0x8e_5c_f7),
    Rgb(0x20_b2_8f),
    Rgb(0xf5_8f_3b),
    Rgb(0xe8_4a_7f),
    Rgb(0x3f_b6_5a),
];

pub const MENU_HEIGHT: i32 = 28;
/// A dock icon: square.
pub const ICON: i32 = 48;
const DOCK_PADDING: i32 = 8;
const DOCK_GAP: i32 = 8;
const DOCK_HEIGHT: i32 = ICON + 2 * DOCK_PADDING;
const DOCK_MARGIN: i32 = 8;
/// The screen's bottom band the dock keeps for itself.
pub const DOCK_RESERVE: i32 = DOCK_HEIGHT + 2 * DOCK_MARGIN;
const PANEL_WIDTH: i32 = 560;
const PANEL_HEIGHT: i32 = 420;
const TILE_COLUMNS: usize = 5;
const TILE_WIDTH: i32 = 102;
const TILE_HEIGHT: i32 = 100;
/// Tiles shown: the Terminal and up to 14 apps (three rows).
const MAX_TILES: usize = 15;
const DIALOG_WIDTH: i32 = 560;
const DIALOG_HEIGHT: i32 = 260;
const BUTTON_WIDTH: i32 = 120;
const BUTTON_HEIGHT: i32 = 36;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

/// An installed app, as the apps panel shows it.
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
    /// Switching off or restarting (ADR-0085): `oceans_rt::power::OFF`
    /// or `RESTART`.
    Power(u64),
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
    /// "Tue 6 Oct  01:14" (UTC).
    pub clock: String,
    pub status: String,
    pub terminal: Terminal,
    /// The keyboard types into the Terminal (no window has the focus).
    pub terminal_focused: bool,
    /// The Terminal is minimized to the dock.
    pub terminal_hidden: bool,
    /// The apps panel is open.
    pub start_open: bool,
    /// This desktop may switch the machine off (`grant = power`): the apps
    /// panel shows Restart and Shut Down.
    pub can_power: bool,
    /// The system volume, as Core says (ADR-0100): `None` without a sound
    /// device. The speaker in the menu bar shows it.
    pub volume: Option<(u8, bool)>,
    /// The sound panel is open below the speaker.
    pub volume_open: bool,
    /// Until when (ms since boot) the level shows over the desktop, after a
    /// volume key (ADR-0101); 0: not shown.
    pub volume_shown_until: u64,
    /// What the player with the media keys plays (ADR-0103): its app,
    /// state (`proto::playing`) and title.
    pub now_playing: Option<(String, u8, String)>,
}

/// An app window to draw.
pub struct WindowView<'a> {
    pub frame: &'a Frame,
    /// Its pixels and their width and height, once the app has presented
    /// them (a resized window's may still have the old size).
    pub pixels: Option<(*const u32, usize, usize)>,
    pub focused: bool,
}

/// What a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The apps button (in the dock) or the Oceans mark (in the menu bar).
    Start,
    /// The Terminal's tile in the apps panel.
    StartTerminal,
    /// An app's tile in the apps panel.
    StartApp(usize),
    /// Restart and Shut Down, in the apps panel.
    Restart,
    ShutDown,
    /// Somewhere else while the apps panel is open: it closes.
    Outside,
    /// The Terminal in the dock.
    TaskbarTerminal,
    /// A window in the dock.
    TaskbarWindow(u32),
    /// The Terminal's minimize button.
    TerminalMinimize,
    /// The Terminal itself.
    Terminal,
    /// The speaker in the menu bar (ADR-0100).
    Volume,
    /// The sound panel's slider, at that level.
    VolumeLevel(u8),
    VolumeMute,
    /// A button of what plays (ADR-0103): the media key it presses.
    Media(u8),
    /// Somewhere else in the sound panel.
    VolumePanel,
    Allow,
    Deny,
    Nothing,
}

// ---- The layout, a function of the screen's size -------------------------

/// Where windows go: between the menu bar and the dock.
pub fn area(width: i32, height: i32) -> Rect {
    Rect::new(0, MENU_HEIGHT, width, height - MENU_HEIGHT - DOCK_RESERVE)
}

pub fn menu_bar(width: i32) -> Rect {
    Rect::new(0, 0, width, MENU_HEIGHT)
}

/// The Oceans mark at the menu bar's left: it opens the apps panel.
pub fn menu_mark() -> Rect {
    Rect::new(8, 0, 36, MENU_HEIGHT)
}

/// What the clock and the system's state take at the menu bar's right,
/// at most: the speaker stays in one place left of them (ADR-0100).
const CLOCK_SLOT: i32 = 128;
const STATUS_SLOT: i32 = 76;

/// The speaker in the menu bar (ADR-0100).
pub fn volume_button(width: i32) -> Rect {
    Rect::new(
        width - 14 - CLOCK_SLOT - 20 - STATUS_SLOT - 16 - 28,
        2,
        28,
        MENU_HEIGHT - 4,
    )
}

/// The sound panel, below the speaker.
pub fn volume_panel(width: i32) -> Rect {
    let button = volume_button(width);
    Rect::new(button.x + button.w - 260, MENU_HEIGHT + 6, 260, 106)
}

/// The whole sound panel: the volume, and below it what plays when a
/// player says (ADR-0103).
pub fn sound_panel(width: i32, media: bool) -> Rect {
    let top = volume_panel(width);
    Rect::new(top.x, top.y, top.w, if media { top.h + MEDIA_HEIGHT } else { top.h })
}

/// The media keys the panel's buttons press: Previous, Play/Pause, Next.
const MEDIA_KEYS: [u8; 3] = [
    oceans_window::proto::KEY_PREVIOUS,
    oceans_window::proto::KEY_PLAY_PAUSE,
    oceans_window::proto::KEY_NEXT,
];

/// Height the panel grows by for what plays.
const MEDIA_HEIGHT: i32 = 76;

/// The panel's Previous, Play/Pause and Next buttons (0, 1, 2).
pub fn media_button(width: i32, which: usize) -> Rect {
    let top = volume_panel(width);
    let y = top.y + top.h + 40;
    match which {
        0 => Rect::new(top.x + 16, y, 60, 26),
        1 => Rect::new(top.x + 84, y, 92, 26),
        _ => Rect::new(top.x + 184, y, 60, 26),
    }
}

/// The panel's slider: its track, and the strip a click on it takes.
pub fn volume_track(width: i32) -> Rect {
    let panel = volume_panel(width);
    Rect::new(panel.x + 16, panel.y + 50, panel.w - 32, 6)
}

fn volume_track_target(width: i32) -> Rect {
    let track = volume_track(width);
    Rect::new(track.x - 8, track.y - 12, track.w + 16, track.h + 24)
}

pub fn volume_mute(width: i32) -> Rect {
    let panel = volume_panel(width);
    Rect::new(panel.x + 16, panel.y + 70, 96, 26)
}

/// The level (0 to 100) at `x` along the slider.
pub fn volume_at(width: i32, x: i32) -> u8 {
    let track = volume_track(width);
    ((x - track.x) * 100 / track.w.max(1)).clamp(0, 100) as u8
}

/// The dock holding `count` icons, centred over the bottom edge.
pub fn dock(width: i32, height: i32, count: usize) -> Rect {
    let count = count as i32;
    let w = count * ICON + (count - 1).max(0) * DOCK_GAP + 2 * (DOCK_PADDING + 4);
    Rect::new(
        (width - w) / 2,
        height - DOCK_MARGIN - DOCK_HEIGHT,
        w,
        DOCK_HEIGHT,
    )
}

/// Dock icon `index` of `count` (the apps button, the Terminal, then the
/// windows).
pub fn dock_item(width: i32, height: i32, index: usize, count: usize) -> Rect {
    let d = dock(width, height, count);
    Rect::new(
        d.x + DOCK_PADDING + 4 + index as i32 * (ICON + DOCK_GAP),
        d.y + DOCK_PADDING,
        ICON,
        ICON,
    )
}

/// The Terminal window, in the middle of the area.
pub fn terminal(width: i32, height: i32) -> Rect {
    let area = area(width, height);
    let w = (area.w - 160).min(960);
    let h = (area.h - 96).min(600);
    Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
}

/// The Terminal's title bar buttons: close (disabled: the shell stays),
/// minimize, resize (disabled).
fn terminal_button(width: i32, height: i32, index: i32) -> Rect {
    let window = terminal(width, height);
    Rect::new(
        window.x + 6 + index * BUTTON_STEP,
        window.y + (TITLE_HEIGHT - CLOSE_SIZE) / 2,
        CLOSE_SIZE,
        CLOSE_SIZE,
    )
}

pub fn terminal_minimize(width: i32, height: i32) -> Rect {
    terminal_button(width, height, 1)
}

/// The apps panel, above the dock.
pub fn start_menu(width: i32, height: i32) -> Rect {
    Rect::new(
        (width - PANEL_WIDTH) / 2,
        height - DOCK_RESERVE - 4 - PANEL_HEIGHT,
        PANEL_WIDTH,
        PANEL_HEIGHT,
    )
}

/// Tile `index` of the apps panel (0: the Terminal; then the apps).
pub fn tile(width: i32, height: i32, index: usize) -> Rect {
    let panel = start_menu(width, height);
    let (col, row) = (index % TILE_COLUMNS, index / TILE_COLUMNS);
    Rect::new(
        panel.x + 24 + col as i32 * TILE_WIDTH,
        panel.y + 64 + row as i32 * TILE_HEIGHT,
        TILE_WIDTH,
        TILE_HEIGHT,
    )
}

/// Restart (0) and Shut Down (1), at the right of the apps panel's
/// footer.
pub fn power_button(width: i32, height: i32, index: i32) -> Rect {
    let panel = start_menu(width, height);
    let (w, gap) = (104, 8);
    Rect::new(
        panel.x + panel.w - 24 - 2 * w - gap + index * (w + gap),
        panel.y + panel.h - 40,
        w,
        30,
    )
}

/// A system dialog, in the middle of the area.
pub fn dialog(width: i32, height: i32) -> Rect {
    let area = area(width, height);
    Rect::new(
        area.x + (area.w - DIALOG_WIDTH) / 2,
        area.y + (area.h - DIALOG_HEIGHT) / 2,
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

/// The windows the dock shows, in the order they were opened.
pub fn taskbar_windows<'a>(windows: &'a [WindowView<'a>]) -> Vec<&'a WindowView<'a>> {
    let mut shown: Vec<&WindowView<'_>> = windows.iter().collect();
    shown.sort_unstable_by_key(|view| view.frame.id);
    shown
}

/// The menu bar's clock: "Tue 6 Oct  01:14", from milliseconds since 1970
/// (UTC).
pub fn clock_text(unix_ms: u64) -> String {
    let days = (unix_ms / 86_400_000) as u32;
    let minutes = unix_ms / 60_000;
    let (_, month, day) = oceans_package::date::civil(days);
    alloc::format!(
        "{} {day} {}  {:02}:{:02}",
        DAYS[days as usize % 7],
        MONTHS[(month as usize).saturating_sub(1) % 12],
        minutes / 60 % 24,
        minutes % 60
    )
}

// ---- The wallpaper ------------------------------------------------------

/// sin(x), close enough for drawing (Bhaskara's approximation, folded to
/// the whole circle; no `std` here).
fn wave_sin(x: f32) -> f32 {
    const PI: f32 = core::f32::consts::PI;
    let mut x = x % (2.0 * PI);
    if x < 0.0 {
        x += 2.0 * PI;
    }
    let (x, sign) = if x > PI { (x - PI, -1.0) } else { (x, 1.0) };
    sign * 16.0 * x * (PI - x) / (5.0 * PI * PI - 4.0 * x * (PI - x))
}

fn mix(from: Rgb, to: Rgb, t: f32) -> Rgb {
    to.over(from, (t.clamp(0.0, 1.0) * 255.0) as u32)
}

/// The ocean wallpaper (ADR-0078): night sky to shallow water, and three
/// waves rolling in, nearer ones darker. Drawn once.
pub fn wallpaper(width: i32, height: i32) -> Vec<u32> {
    const STOPS: [(f32, Rgb); 4] = [
        (0.0, Rgb(0x16_1f_5c)),
        (0.42, Rgb(0x3a_5f_c8)),
        (0.7, Rgb(0x3f_a2_d9)),
        (1.0, Rgb(0x8b_dc_e6)),
    ];
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    let mut pixels = alloc::vec![0u32; w * h];
    for (y, row) in pixels.chunks_mut(w).enumerate() {
        let t = y as f32 / h as f32;
        let i = STOPS
            .iter()
            .rposition(|&(at, _)| at <= t)
            .unwrap_or(0)
            .min(2);
        let ((a, from), (b, to)) = (STOPS[i], STOPS[i + 1]);
        row.fill(mix(from, to, (t - a) / (b - a)).0);
    }
    // Waves, from the farthest to the nearest.
    let waves: [(f32, f32, f32, f32, Rgb, u32); 3] = [
        (0.58, 16.0, 1.4, 0.3, Rgb(0x2c_6f_c4), 120),
        (0.70, 20.0, 1.0, 2.1, Rgb(0x1f_55_a8), 140),
        (0.82, 24.0, 0.8, 4.0, Rgb(0x15_3f_86), 170),
    ];
    for (base, amplitude, cycles, phase, colour, alpha) in waves {
        for x in 0..w {
            let angle = x as f32 / w as f32 * cycles * 2.0 * core::f32::consts::PI + phase;
            let top = (base * h as f32 + amplitude * wave_sin(angle)) as usize;
            for y in top.min(h)..h {
                let at = y * w + x;
                // Deeper, a little darker.
                let depth = ((y - top) as f32 / h as f32 * 2.0).min(1.0);
                let shade = mix(colour, Rgb(0x0b_25_55), depth * 0.6);
                pixels[at] = shade.over(Rgb(pixels[at]), alpha).0;
            }
            // The crest catches the light.
            for (dy, light) in [(0usize, 70u32), (1, 35)] {
                let y = top + dy;
                if y < h {
                    let at = y * w + x;
                    pixels[at] = WHITE.over(Rgb(pixels[at]), light).0;
                }
            }
        }
    }
    pixels
}

impl Desktop {
    /// What a click on the desktop (the menu bar, the dock, the apps panel,
    /// dialogs, the Terminal: everything but app windows) hits, on a
    /// `width × height` screen; `windows` as the dock shows them.
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
        if dock_item(width, height, 0, count).contains(x, y) || menu_mark().contains(x, y) {
            return Hit::Start;
        }
        // The speaker and its panel (ADR-0100).
        if self.volume.is_some() && volume_button(width).contains(x, y) {
            return Hit::Volume;
        }
        let media = self.now_playing.is_some();
        if self.volume_open && sound_panel(width, media).contains(x, y) {
            return if let Some(which) = (0..3).find(|&i| media && media_button(width, i).contains(x, y)) {
                Hit::Media(MEDIA_KEYS[which])
            } else if volume_track_target(width).contains(x, y) {
                Hit::VolumeLevel(volume_at(width, x))
            } else if volume_mute(width).contains(x, y) {
                Hit::VolumeMute
            } else {
                Hit::VolumePanel
            };
        }
        if self.start_open {
            if start_menu(width, height).contains(x, y) {
                if tile(width, height, 0).contains(x, y) {
                    return Hit::StartTerminal;
                }
                if self.can_power {
                    if power_button(width, height, 0).contains(x, y) {
                        return Hit::Restart;
                    }
                    if power_button(width, height, 1).contains(x, y) {
                        return Hit::ShutDown;
                    }
                }
                let shown = self.apps.len().min(MAX_TILES - 1);
                return (0..shown)
                    .find(|&i| tile(width, height, i + 1).contains(x, y))
                    .map_or(Hit::Nothing, Hit::StartApp);
            }
            return Hit::Outside;
        }
        if dock_item(width, height, 1, count).contains(x, y) {
            return Hit::TaskbarTerminal;
        }
        if let Some(&id) = windows
            .iter()
            .enumerate()
            .find(|&(i, _)| dock_item(width, height, i + 2, count).contains(x, y))
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
        canvas.draw_wallpaper();

        if !self.terminal_hidden {
            self.draw_terminal(canvas);
        }

        // App windows, bottom to top.
        for view in windows.iter().filter(|view| !view.frame.minimized) {
            draw_window(canvas, view, self.pointer);
        }

        self.draw_menu_bar(canvas, windows);
        self.draw_dock(canvas, windows);
        if self.start_open {
            self.draw_apps_panel(canvas);
        }
        if self.volume_open
            && let Some(volume) = self.volume
        {
            self.draw_volume_panel(canvas, volume);
        }
        if self.volume_shown_until > now_ms
            && let Some(volume) = self.volume
        {
            draw_volume_shown(canvas, volume);
        }

        // A notification, at the top right.
        if let Some(toast) = self.toast.as_ref().filter(|t| t.until_ms > now_ms) {
            let w = canvas.width;
            let r = Rect::new(w - 12 - 360, MENU_HEIGHT + 8, 360, 56);
            canvas.shadow(r, 10, 70);
            canvas.round_fill(r, 12, DIALOG_SURFACE);
            let mark = if toast.error { CLOSE } else { ACCENT };
            canvas.circle(r.x + 22, r.y + r.h / 2, 7, mark);
            canvas.text(r.x + 40, r.y + 19, &toast.text, Font::Body, TEXT, r);
        }

        // A system dialog, over everything else.
        if let Some(dialog) = &self.dialog {
            self.draw_dialog(canvas, dialog);
        }

        canvas.cursor(self.pointer.0, self.pointer.1, WHITE, BLACK);
    }

    /// The Terminal: the console's text, the rows that fit, ending at the
    /// cursor's.
    fn draw_terminal(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        let window = terminal(w, h);
        let focused = self.terminal_focused;
        chrome(canvas, window, focused);
        canvas.fill(
            Rect::new(
                window.x + 1,
                window.y + TITLE_HEIGHT,
                window.w - 2,
                window.h - TITLE_HEIGHT - 1,
            ),
            TERMINAL_BACKGROUND,
        );
        title_text(
            canvas,
            Rect::new(window.x, window.y, window.w, TITLE_HEIGHT),
            "Terminal",
            "",
            window.x + 6 + 3 * BUTTON_STEP,
        );
        let buttons = [
            (terminal_button(w, h, 0), None),
            (terminal_button(w, h, 1), focused.then_some(MINIMIZE)),
            (terminal_button(w, h, 2), None),
        ];
        traffic_lights(canvas, &buttons, self.pointer);
        let body = Rect::new(window.x + 10, window.y + 36, window.w - 20, window.h - 44);
        let t = &self.terminal;
        if t.cols > 0 {
            let visible = (body.h / Font::Mono.height()).max(1) as usize;
            let last = (t.row + 1).min(t.rows);
            let first = last.saturating_sub(visible);
            for (line, row) in (first..last).enumerate() {
                let cells = &t.cells[row * t.cols..(row + 1) * t.cols];
                let text = core::str::from_utf8(cells).unwrap_or("").trim_end();
                let y = body.y + line as i32 * Font::Mono.height();
                canvas.text(body.x, y, text, Font::Mono, TERMINAL_TEXT, body);
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

    fn draw_menu_bar(&self, canvas: &mut Canvas, windows: &[WindowView<'_>]) {
        let w = canvas.width;
        let bar = menu_bar(w);
        canvas.tint(bar, 0, WHITE, 190);
        canvas.tint(Rect::new(0, MENU_HEIGHT - 1, w, 1), 0, BLACK, 25);
        let mark = menu_mark();
        if self.start_open || mark.contains(self.pointer.0, self.pointer.1) {
            canvas.tint(Rect::new(mark.x, 3, mark.w, MENU_HEIGHT - 6), 6, BLACK, 25);
        }
        oceans_mark(canvas, mark, TEXT);
        // The app in use.
        let current = windows
            .iter()
            .find(|view| view.focused && !view.frame.minimized)
            .map(|view| view.frame.app.as_str())
            .or((self.terminal_focused && !self.terminal_hidden).then_some("Terminal"))
            .unwrap_or("Oceans");
        canvas.text(mark.x + mark.w + 10, 6, current, Font::Strong, TEXT, bar);
        // The date and time, then the system's state, from the right.
        let clock_x = w - 14 - canvas.measure(&self.clock, Font::Body);
        canvas.text(clock_x, 6, &self.clock, Font::Body, TEXT, bar);
        let status_x = clock_x - 24 - canvas.measure(&self.status, Font::Body);
        canvas.text(status_x, 6, &self.status, Font::Body, TEXT, bar);
        // The speaker (ADR-0100), in one place left of them.
        if let Some((level, muted)) = self.volume {
            let button = volume_button(w);
            if self.volume_open || button.contains(self.pointer.0, self.pointer.1) {
                canvas.tint(button, 6, BLACK, 25);
            }
            speaker(canvas, button, level, muted);
        }
    }

    /// The sound panel (ADR-0100): the level, a slider and Mute.
    fn draw_volume_panel(&self, canvas: &mut Canvas, (level, muted): (u8, bool)) {
        let w = canvas.width;
        let whole = sound_panel(w, self.now_playing.is_some());
        canvas.shadow(whole, 12, 70);
        canvas.round_fill(whole, 12, DIALOG_SURFACE);
        let panel = volume_panel(w);
        canvas.text(
            panel.x + 16,
            panel.y + 12,
            "Sound",
            Font::Strong,
            TEXT,
            panel,
        );
        let shown = if muted {
            String::from("Muted")
        } else {
            alloc::format!("{level}%")
        };
        let x = panel.x + panel.w - 16 - canvas.measure(&shown, Font::Body);
        canvas.text(x, panel.y + 12, &shown, Font::Body, MUTED, panel);
        let track = volume_track(w);
        canvas.round_fill(track, 3, BUTTON_SECONDARY);
        let filled = track.w * i32::from(level) / 100;
        let ink = if muted { MUTED } else { ACCENT };
        canvas.round_fill(Rect::new(track.x, track.y, filled, track.h), 3, ink);
        canvas.circle(track.x + filled, track.y + track.h / 2, 8, WINDOW_BORDER);
        canvas.circle(track.x + filled, track.y + track.h / 2, 7, WHITE);
        let mute = volume_mute(w);
        canvas.round_fill(mute, 7, if muted { ACCENT } else { BUTTON_SECONDARY });
        let label = if muted { "Unmute" } else { "Mute" };
        let ink = if muted { WHITE } else { TEXT };
        let lx = mute.x + (mute.w - canvas.measure(label, Font::Body)) / 2;
        canvas.text(lx, mute.y + 4, label, Font::Body, ink, mute);
        // What plays (ADR-0103): the title, the app, and its buttons.
        if let Some((app, state, title)) = &self.now_playing {
            canvas.fill(
                Rect::new(panel.x + 16, panel.y + panel.h, panel.w - 32, 1),
                BUTTON_SECONDARY,
            );
            let row = Rect::new(panel.x + 16, panel.y + panel.h + 10, panel.w - 32, 22);
            let app_x = row.x + row.w - canvas.measure(app, Font::Body);
            canvas.text(app_x, row.y, app, Font::Body, MUTED, row);
            let title_clip = Rect::new(row.x, row.y, app_x - row.x - 8, row.h);
            canvas.text(row.x, row.y, title, Font::Strong, TEXT, title_clip);
            let playing = *state == oceans_window::proto::playing::PLAYING;
            let labels = ["Prev", if playing { "Pause" } else { "Play" }, "Next"];
            for (which, label) in labels.iter().enumerate() {
                let button = media_button(w, which);
                let primary = which == 1;
                canvas.round_fill(button, 7, if primary { ACCENT } else { BUTTON_SECONDARY });
                let ink = if primary { WHITE } else { TEXT };
                let lx = button.x + (button.w - canvas.measure(label, Font::Body)) / 2;
                canvas.text(lx, button.y + 4, label, Font::Body, ink, button);
            }
        }
    }

    fn draw_dock(&self, canvas: &mut Canvas, windows: &[WindowView<'_>]) {
        let (w, h) = (canvas.width, canvas.height);
        let shown = taskbar_windows(windows);
        let count = 2 + shown.len();
        let d = dock(w, h, count);
        canvas.shadow(d, 10, 50);
        canvas.tint(Rect::new(d.x - 1, d.y - 1, d.w + 2, d.h + 2), 19, BLACK, 30);
        canvas.tint(d, 18, WHITE, 120);

        let pointer = self.pointer;
        let mut label: Option<(Rect, String)> = None;
        let mut item = |canvas: &mut Canvas, index: usize, name: &str, open: bool| {
            let r = dock_item(w, h, index, count);
            if r.contains(pointer.0, pointer.1) && self.dialog.is_none() {
                label = Some((r, String::from(name)));
            }
            if open {
                canvas.circle(r.x + r.w / 2, d.y + d.h - 4, 2, TEXT);
            }
            r
        };

        let start = item(canvas, 0, "Apps", self.start_open);
        apps_icon(canvas, start);
        let term = item(canvas, 1, "Terminal", !self.terminal_hidden);
        terminal_icon(canvas, term);
        for (i, view) in shown.iter().enumerate() {
            let r = item(canvas, i + 2, &view.frame.app, true);
            app_icon(canvas, r, &view.frame.app);
            if view.frame.minimized {
                // Put away: the icon is a little faded.
                canvas.tint(r, 12, WHITE, 90);
            }
        }

        // The name of what the pointer is on, over the dock.
        if let Some((r, name)) = label {
            let width = canvas.measure(&name, Font::Body) + 20;
            let tip = Rect::new(r.x + (r.w - width) / 2, d.y - 34, width, 26);
            canvas.tint(tip, 8, Rgb(0x2c_2c_2e), 230);
            canvas.text(tip.x + 10, tip.y + 5, &name, Font::Body, WHITE, tip);
        }
    }

    fn draw_apps_panel(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        let panel = start_menu(w, h);
        canvas.shadow(panel, 16, 90);
        canvas.tint(
            Rect::new(panel.x - 1, panel.y - 1, panel.w + 2, panel.h + 2),
            17,
            BLACK,
            35,
        );
        canvas.round_fill(panel, 16, PANEL);
        canvas.text(panel.x + 24, panel.y + 22, "Apps", Font::Title, TEXT, panel);
        let pointer = self.pointer;
        let tile_frame = |canvas: &mut Canvas, index: usize| {
            let t = tile(w, h, index);
            if t.contains(pointer.0, pointer.1) && self.dialog.is_none() {
                canvas.tint(inset(t, 2), 10, BLACK, 18);
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
                canvas.circle(icon.x + icon.w - 2, icon.y + 2, 5, SUCCESS);
            }
            tile_label(canvas, t, &app.name);
        }
        let footer_y = panel.y + panel.h - 48;
        canvas.tint(
            Rect::new(panel.x + 16, footer_y, panel.w - 32, 1),
            0,
            BLACK,
            30,
        );
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
        canvas.text(panel.x + 24, footer_y + 16, &note, Font::Body, MUTED, panel);
        if self.can_power {
            for (index, label) in [(0, "Restart"), (1, "Shut Down")] {
                let button = power_button(w, h, index);
                let hovered = button.contains(pointer.0, pointer.1) && self.dialog.is_none();
                let fill = if hovered {
                    Rgb(0xd5_d5_db)
                } else {
                    BUTTON_SECONDARY
                };
                canvas.round_fill(button, 8, fill);
                let tx = button.x + (button.w - canvas.measure(label, Font::Body)) / 2;
                canvas.text(tx, button.y + 6, label, Font::Body, TEXT, button);
            }
        }
    }

    fn draw_dialog(&self, canvas: &mut Canvas, dialog: &Dialog) {
        let (w, h) = (canvas.width, canvas.height);
        let r = self::dialog(w, h);
        canvas.dim(BLACK, 100);
        canvas.shadow(r, 18, 120);
        canvas.round_fill(r, 14, DIALOG_SURFACE);
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
                (true, true) => Rgb(0x52_93_f8),
                (false, false) => BUTTON_SECONDARY,
                (false, true) => Rgb(0xd5_d5_db),
            };
            canvas.round_fill(button, 8, fill);
            let tx = button.x + (button.w - canvas.measure(label, Font::Strong)) / 2;
            let colour = if primary { WHITE } else { TEXT };
            canvas.text(tx, button.y + 9, label, Font::Strong, colour, button);
        }
    }
}

fn inset(r: Rect, by: i32) -> Rect {
    Rect::new(r.x + by, r.y + by, r.w - 2 * by, r.h - 2 * by)
}

/// The icon of an apps panel tile: 48 pixels, centred at its top.
fn icon_in(tile: Rect) -> Rect {
    Rect::new(tile.x + (tile.w - ICON) / 2, tile.y + 12, ICON, ICON)
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

/// An app's icon: a rounded square in its colour (chosen by its name, which
/// the dock and the apps panel both know), with its initial.
fn app_icon(canvas: &mut Canvas, r: Rect, name: &str) {
    let hash = name.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    });
    let colour = ICON_COLOURS[hash % ICON_COLOURS.len()];
    canvas.round_fill(r, r.w / 4, colour);
    // A lighter top half: the icon catches the light.
    canvas.tint(Rect::new(r.x, r.y, r.w, r.h / 2), r.w / 4, WHITE, 28);
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
        r.y + (r.h - font.height()) / 2 - 1,
        &initial,
        font,
        WHITE,
        r,
    );
}

/// The Terminal's icon: a dark rounded square with a prompt.
fn terminal_icon(canvas: &mut Canvas, r: Rect) {
    canvas.round_fill(r, r.w / 4, Rgb(0x3a_3a_3c));
    canvas.round_fill(inset(r, 2), r.w / 4 - 2, TERMINAL_BACKGROUND);
    let font = if r.w >= 40 { Font::Title } else { Font::Strong };
    let width = canvas.measure(">_", font);
    canvas.text(
        r.x + (r.w - width) / 2,
        r.y + (r.h - font.height()) / 2 - 1,
        ">_",
        font,
        Rgb(0x004c_d964),
        r,
    );
}

/// The apps button's icon: a grid of dots on the accent.
fn apps_icon(canvas: &mut Canvas, r: Rect) {
    canvas.round_fill(r, r.w / 4, ACCENT);
    canvas.tint(Rect::new(r.x, r.y, r.w, r.h / 2), r.w / 4, WHITE, 35);
    let step = r.w / 4;
    for row in 1..4 {
        for col in 1..4 {
            canvas.circle(r.x + col * step, r.y + row * step, 3, WHITE);
        }
    }
}

/// Where the level shows after a volume key (ADR-0101): above the dock, in
/// the middle.
pub fn volume_shown(width: i32, height: i32) -> Rect {
    Rect::new((width - 260) / 2, height - DOCK_RESERVE - 64, 260, 48)
}

/// The level over the desktop (ADR-0101): a speaker and a bar.
fn draw_volume_shown(canvas: &mut Canvas, (level, muted): (u8, bool)) {
    let r = volume_shown(canvas.width, canvas.height);
    canvas.shadow(r, 12, 70);
    canvas.round_fill(r, 14, DIALOG_SURFACE);
    speaker(canvas, Rect::new(r.x + 8, r.y + 12, 28, 24), level, muted);
    let track = Rect::new(r.x + 48, r.y + 21, r.w - 64, 6);
    canvas.round_fill(track, 3, BUTTON_SECONDARY);
    if !muted {
        let filled = track.w * i32::from(level) / 100;
        canvas.round_fill(Rect::new(track.x, track.y, filled, track.h), 3, ACCENT);
    }
}

/// A speaker centred in `r` (ADR-0100): a body and a cone, then up to three
/// bars for the level, or a cross when muted.
fn speaker(canvas: &mut Canvas, r: Rect, level: u8, muted: bool) {
    let (cx, cy) = (r.x + r.w / 2 - 6, r.y + r.h / 2);
    canvas.fill(Rect::new(cx - 5, cy - 3, 4, 7), TEXT);
    for i in 0..5 {
        canvas.fill(Rect::new(cx - 1 + i, cy - 3 - i, 1, 7 + 2 * i), TEXT);
    }
    if muted || level == 0 {
        for i in 0..7 {
            canvas.fill(Rect::new(cx + 7 + i, cy - 3 + i, 2, 1), TEXT);
            canvas.fill(Rect::new(cx + 13 - i, cy - 3 + i, 2, 1), TEXT);
        }
        return;
    }
    let bars = match level {
        0..=33 => 1,
        34..=66 => 2,
        _ => 3,
    };
    for i in 0..bars {
        let h = 4 + 3 * i;
        canvas.fill(Rect::new(cx + 7 + 3 * i, cy - h / 2, 2, h), TEXT);
    }
}

/// The Oceans mark (two waves), centred in `r`, in `colour`.
fn oceans_mark(canvas: &mut Canvas, r: Rect, colour: Rgb) {
    const WAVE: [i32; 10] = [0, -1, -2, -2, -1, 0, 1, 2, 2, 1];
    let x0 = r.x + (r.w - 20) / 2;
    let y0 = r.y + r.h / 2 - 4;
    for line in 0..2 {
        for dx in 0..20 {
            let y = y0 + line * 8 + WAVE[dx as usize % WAVE.len()];
            canvas.fill(Rect::new(x0 + dx, y, 1, 2), colour);
        }
    }
}

/// A window's frame (the Terminal's too): a soft shadow, a rounded border
/// and the title bar's colour.
fn chrome(canvas: &mut Canvas, outer: Rect, focused: bool) {
    canvas.shadow(outer, 14, if focused { 120 } else { 60 });
    canvas.round_fill(outer, 10, WINDOW_BORDER);
    canvas.round_fill(
        inset(outer, 1),
        9,
        if focused { FOCUS_TITLE } else { IDLE_TITLE },
    );
}

/// The title, centred in the bar (clear of the buttons at the left): the
/// app's verified name, then its own title.
fn title_text(canvas: &mut Canvas, bar: Rect, app: &str, title: &str, left: i32) {
    let gap = if title.is_empty() { 0 } else { 8 };
    let width = canvas.measure(app, Font::Strong) + gap + canvas.measure(title, Font::Body);
    let clip = Rect::new(left, bar.y, bar.x + bar.w - 8 - left, bar.h);
    let x = (bar.x + (bar.w - width) / 2).max(left);
    let end = canvas.text(x, bar.y + 5, app, Font::Strong, TEXT, clip);
    if !title.is_empty() {
        canvas.text(end + gap, bar.y + 5, title, Font::Body, MUTED, clip);
    }
}

/// The round title bar buttons: each in its colour, or disabled (`None`).
/// Pointed at, the group shows what each does.
fn traffic_lights(canvas: &mut Canvas, buttons: &[(Rect, Option<Rgb>)], pointer: (i32, i32)) {
    let hovered = buttons
        .iter()
        .any(|(r, _)| r.contains(pointer.0, pointer.1));
    for (i, &(r, colour)) in buttons.iter().enumerate() {
        let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
        canvas.circle(cx, cy, 6, colour.unwrap_or(BUTTON_OFF));
        if hovered && colour.is_some() {
            let mark = Rgb(0x4a_1c_14);
            match i {
                0 => {
                    for d in -2..=2 {
                        canvas.fill(Rect::new(cx + d, cy + d, 1, 1), mark);
                        canvas.fill(Rect::new(cx + d, cy - d, 1, 1), mark);
                    }
                }
                _ => canvas.fill(Rect::new(cx - 3, cy, 7, 1), mark),
            }
        }
    }
}

/// An app window: the frame the system draws, and the app's pixels inside.
fn draw_window(canvas: &mut Canvas, view: &WindowView<'_>, pointer: (i32, i32)) {
    let frame = view.frame;
    chrome(canvas, frame.outer(), view.focused);
    title_text(
        canvas,
        frame.title_bar(),
        &frame.app,
        &frame.title,
        frame.zoom_button().x + CLOSE_SIZE + 8,
    );
    let lit = |colour| view.focused.then_some(colour);
    traffic_lights(
        canvas,
        &[
            (frame.close_button(), lit(CLOSE)),
            (frame.minimize_button(), lit(MINIMIZE)),
            (
                frame.zoom_button(),
                frame.resizable().then_some(ZOOM).and_then(lit),
            ),
        ],
        pointer,
    );
    let content = frame.content();
    match view.pixels {
        Some((pixels, width, height)) if frame.presented => {
            // Pixels of another size (a resize the app has not caught up
            // with): as much as fits, the rest in the windows' colour.
            let w = content.w.min(width as i32);
            let h = content.h.min(height as i32);
            canvas.blit(Rect::new(content.x, content.y, w, h), pixels, width);
            canvas.fill(
                Rect::new(content.x + w, content.y, content.w - w, content.h),
                IDLE_TITLE,
            );
            canvas.fill(
                Rect::new(content.x, content.y + h, w, content.h - h),
                IDLE_TITLE,
            );
        }
        _ => canvas.fill(content, WHITE),
    }
}
