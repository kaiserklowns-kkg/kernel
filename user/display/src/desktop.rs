//! The Oceans desktop (ADR-0057, ADR-0076, ADR-0078): what it shows, where,
//! and what a click hits. Drawing is a pure function of this state.
//!
//! The look (ADR-0078): light, translucent and rounded, after the macOS
//! Big Sur design language, drawn in Oceans' own shapes and colours:
//! - an ocean wallpaper, a photograph (ADR-0117);
//! - the **top bar** (ADR-0117), clear over the wallpaper: the time at the
//!   left; the app in use, the layout, the speaker and the state at the right;
//! - **Wave** (ADR-0117), Oceans' own taskbar: one round button at the
//!   bottom's middle, a dot for each open app; with the pointer on it the
//!   Terminal, the kept apps and the windows rise over it along a crest,
//!   a bar under what is open and the name over the one pointed at;
//! - **Launchpad** (ADR-0117), from Wave's button or Super: the
//!   Terminal and the installed apps in a grid across the screen, the
//!   search at the top, and Lock, Restart and Shut Down (ADR-0085) in the
//!   corner, each confirmed in a system dialog;
//! - **windows** (the Terminal's too) with light title bars, the title in
//!   the middle and round buttons at the left: close, minimize and
//!   resize (not yet available, drawn disabled);
//! - notifications at the top right, system dialogs in the middle.
//!
//! Every position is a function of the screen's size, so the smoke test
//! finds things where the layout puts them. Positions and sizes are in
//! logical pixels (ADR-0136): the canvas draws them at the scale.

use alloc::string::String;
use alloc::vec::Vec;

use oceans_draw::{Surface, icon};
use oceans_wifi::Security;
use oceans_wifi::state::{Listed, Ssid};
use oceans_window::{BUTTON_STEP, CLOSE_SIZE, Frame, TITLE_HEIGHT, dock};

use crate::canvas::{Canvas, Font, Rect, Rgb};

// Design tokens (master spec §31; ADR-0078): surfaces, text, one accent,
// the window buttons' signal colours. Those that follow the theme
// (ADR-0113: light or dark, the user's accent) are functions of it.
pub const WHITE: Rgb = Rgb(0xff_ff_ff);
pub const BLACK: Rgb = Rgb(0x00_00_00);
/// Dark text on what stays white whatever the theme (the lock screen's
/// field).
pub const INK: Rgb = Rgb(0x1d_1d_1f);
pub const CLOSE: Rgb = Rgb(0xff_5f_57);
pub const MINIMIZE: Rgb = Rgb(0xfe_bc_2e);
/// The zoom button of a window that can be resized (ADR-0097).
pub const ZOOM: Rgb = Rgb(0x28_c8_40);
pub const TERMINAL_BACKGROUND: Rgb = Rgb(0x1c_1c_1e);
pub const TERMINAL_TEXT: Rgb = Rgb(0xe8_e8_ed);

/// The colours that follow the theme (ADR-0113).
pub mod colour {
    use super::{Rgb, WHITE};

    fn theme() -> oceans_draw::theme::Theme {
        oceans_draw::theme::current()
    }

    pub fn text() -> Rgb {
        theme().text
    }
    pub fn muted() -> Rgb {
        theme().muted
    }
    pub fn accent() -> Rgb {
        theme().accent
    }
    /// The search result Enter opens (ADR-0108).
    pub fn chosen() -> Rgb {
        theme().selected
    }
    pub fn success() -> Rgb {
        theme().success
    }
    /// A window button that does nothing (yet), or any button of a window
    /// without the focus.
    pub fn button_off() -> Rgb {
        theme().button_off
    }
    /// Title bars: of the window (or Terminal) with the keyboard focus, and
    /// of the others.
    pub fn focus_title() -> Rgb {
        theme().title_focused
    }
    pub fn idle_title() -> Rgb {
        theme().title_idle
    }
    pub fn window_border() -> Rgb {
        theme().border
    }
    /// Launchpad.
    pub fn panel() -> Rgb {
        theme().panel
    }
    pub fn dialog_surface() -> Rgb {
        theme().dialog
    }
    pub fn button_secondary() -> Rgb {
        theme().button
    }
    /// What holds content: fields, a window before its app draws.
    pub fn surface() -> Rgb {
        theme().surface
    }
    /// The tint of the menu bar and the dock over the wallpaper: light glass,
    /// or dark.
    pub fn glass() -> Rgb {
        match theme().appearance {
            oceans_draw::theme::Appearance::Light => WHITE,
            oceans_draw::theme::Appearance::Dark => Rgb(0x1e_1e_20),
        }
    }
}
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
/// Wave (ADR-0117): the button, how far it sits from the bottom, the gap
/// between icons when open, how far above the button they float, and how
/// much higher the middle rises.
const WAVE_BUTTON: i32 = 48;
const WAVE_MARGIN: i32 = 12;
const WAVE_GAP: i32 = 14;
const WAVE_LIFT: i32 = 14;
const WAVE_CREST: i32 = 18;
/// The screen's bottom band windows leave to Wave's button.
pub const DOCK_RESERVE: i32 = 80;
/// Launchpad (ADR-0117): the grid of apps across the screen.
const TILE_WIDTH: i32 = 120;
const TILE_HEIGHT: i32 = 112;
/// Launchpad's search field and its results.
const SEARCH_WIDTH: i32 = 440;
const FOUND_WIDTH: i32 = 600;
const DIALOG_WIDTH: i32 = 560;
const DIALOG_HEIGHT: i32 = 260;
const BUTTON_WIDTH: i32 = 120;
const BUTTON_HEIGHT: i32 = 36;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

/// An installed app, as Launchpad shows it.
pub struct App {
    pub id: String,
    pub name: String,
    pub version: String,
    pub running: bool,
}

/// The network as the desktop shows it (ADR-0118): connected (an address
/// is configured), and the address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    pub connected: bool,
    pub address: String,
}

/// Wi-Fi as the driver's files say (ADR-0130): what it is doing, and the
/// networks of its last scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wifi {
    pub state: WifiState,
    pub networks: Vec<Listed>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WifiState {
    /// No Wi-Fi to use (the chip did not come up).
    #[default]
    Off,
    Idle,
    Scanning,
    Joining(String),
    Joined {
        name: String,
        rssi: i8,
    },
    Failed {
        name: String,
        reason: String,
    },
}

/// A network's password being typed in the Wi-Fi panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiEntry {
    pub ssid: Ssid,
    pub name: String,
    pub password: String,
}

/// A network's name as text (the font has Latin letters; anything else
/// shows as a replacement).
pub fn ssid_name(ssid: &[u8]) -> String {
    String::from_utf8_lossy(ssid).into_owned()
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
    /// Where the pointer is, in logical pixels: what it points at.
    pub pointer: (i32, i32),
    /// And in the screen's own pixels (ADR-0136), where its arrow is drawn:
    /// it moves by them, however large the scale.
    pub pointer_device: (i32, i32),
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
    /// How many of the console's rows the Terminal shows back from the
    /// cursor's (ADR-0138): the wheel turns it, a key brings it back.
    pub terminal_back: usize,
    /// Launchpad is open.
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
    /// Where the window dragged would go if let go now (ADR-0107).
    pub snap: Option<Rect>,
    /// A window with the focus covers the screen (ADR-0107): no menu bar
    /// or dock.
    pub full_screen: bool,
    /// What is typed into Launchpad's search (ADR-0108); empty: the
    /// tiles show.
    pub search: String,
    /// What it found, best first, and the one Enter opens.
    pub found: Vec<Found>,
    pub chosen: usize,
    /// The apps kept in the dock (ADR-0109), by id, as Core keeps them.
    pub pinned: Vec<String>,
    /// The lock screen (ADR-0110).
    pub lock: Lock,
    /// What is typed into it (shown as dots), and what it says.
    pub lock_input: String,
    pub lock_note: String,
    /// The user's name, as Core keeps it.
    pub user_name: String,
    /// A password is set: Launchpad offers Lock (ADR-0112).
    pub password_set: bool,
    /// The accounts (ADR-0115), by id and name, the first first; the one
    /// chosen at the lock screen, and the one in use.
    pub users: Vec<(String, String)>,
    pub chosen_user: usize,
    pub current_user: usize,
    /// The account in use administers: the Terminal (the system's console)
    /// is its alone.
    pub admin: bool,
    /// Wave is open (ADR-0117): its icons stand over the button while the
    /// pointer is there.
    pub wave_open: bool,
    /// The network (ADR-0118), if the desktop can read it.
    pub network: Option<Network>,
    /// Wi-Fi (ADR-0130), on a machine whose Wi-Fi driver runs.
    pub wifi: Option<Wifi>,
    /// Control Center shows the Wi-Fi panel in place of its modules.
    pub wifi_open: bool,
    /// A password being typed there.
    pub wifi_entry: Option<WifiEntry>,
    /// The notifications (ADR-0119), newest first, at most
    /// [`MAX_NOTICES`]; how many came since they were last looked at;
    /// whether their panel is open.
    pub notices: Vec<Notice>,
    pub unread: usize,
    pub notices_open: bool,
    /// The desktop's icon chosen by a click (ADR-0120); a second click
    /// on it opens it.
    pub icon_chosen: Option<usize>,
    /// The user's menu under its icon is open (ADR-0121).
    pub user_menu_open: bool,
    /// The time and the date in words, for the notifications' panel.
    pub clock_time: String,
    pub clock_date: String,
    /// The keyboard layout typed letters follow (ADR-0114).
    pub layout: oceans_window::layout::Layout,
}

/// An app's notification (ADR-0065, ADR-0119), kept for its panel: the
/// app's verified name, what it said, and when (milliseconds since boot).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub app: String,
    pub text: String,
    pub at_ms: u64,
}

/// The notifications kept (ADR-0119); older ones go.
pub const MAX_NOTICES: usize = 30;

/// The app Home opens in, on the desktop (ADR-0120).
const FILES_APP: &str = "app.oceans.files";
/// The desktop's icons, at most: Home and every app kept in Wave.
const MAX_DESKTOP_ITEMS: usize = 1 + oceans_window::dock::MAX_PINNED;
/// The cards the panel shows, newest first.
const NOTICE_CARDS: usize = 6;

/// Whether the screen is locked (ADR-0110).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lock {
    /// Until Core says whether there is a password: nothing shows yet.
    #[default]
    Starting,
    Locked,
    Open,
}

/// A window, as the dock needs it: its id and its app's name.
pub type DockWindow = (u32, String);

/// A search result (ADR-0108).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// An installed app, by its index in `apps`.
    App(usize),
    /// A file of Home, by its path there.
    File(String),
}

/// Results shown at most, apps and files together.
pub const MAX_FOUND: usize = 9;

/// An app window to draw.
pub struct WindowView<'a> {
    pub frame: &'a Frame,
    /// Its pixels and their width and height, once the app has presented
    /// them (a resized window's may still have the old size).
    pub pixels: Option<(*const u32, usize, usize)>,
    pub focused: bool,
}

/// What the user's menu does (ADR-0121), as macOS's Fast User Switching
/// menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAction {
    /// The screen locks.
    Lock,
    /// The lock screen, to choose another account.
    Switch,
    /// Settings, where the accounts are.
    Settings,
}

/// What a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Wave's button (ADR-0117): it opens Launchpad.
    Start,
    /// The Terminal's tile in Launchpad.
    StartTerminal,
    /// An app's tile in Launchpad.
    StartApp(usize),
    /// A search result in Launchpad (ADR-0108).
    Found(usize),
    /// Lock, in Launchpad (ADR-0112).
    Lock,
    /// Restart and Shut Down, in Launchpad.
    Restart,
    ShutDown,
    /// Somewhere else while Launchpad is open: it closes.
    Outside,
    /// The Terminal in the dock.
    TaskbarTerminal,
    /// A window in the dock.
    TaskbarWindow(u32),
    /// An app kept in the dock (ADR-0109), by its index among those shown.
    DockPinned(usize),
    /// The Terminal's minimize button.
    TerminalMinimize,
    /// The Terminal itself.
    Terminal,
    /// The keyboard layout, in the top bar (ADR-0114) or Control Center's
    /// tile (ADR-0118).
    Layout,
    /// A status icon in the top bar (ADR-0118): Control Center opens or
    /// closes.
    Volume,
    /// Control Center's slider, at that level (ADR-0100).
    VolumeLevel(u8),
    VolumeMute,
    /// A button of what plays (ADR-0103): the media key it presses.
    Media(u8),
    /// A round toggle in Control Center (ADR-0118): 0 the network, 1 dark,
    /// 2 lock, 3 Settings.
    Toggle(u8),
    /// Control Center's accent tile: the next accent.
    Accent,
    /// The bell (ADR-0119): the notifications' panel opens or closes.
    Clock,
    /// The panel's Clear, and anywhere else in it.
    ClearNotices,
    NoticePanel,
    /// An icon on the desktop (ADR-0120), by its place.
    DesktopIcon(usize),
    /// The user's icon (ADR-0121): the user's menu opens or closes.
    User,
    /// An item of the user's menu.
    UserItem(UserAction),
    /// The Wi-Fi panel (ADR-0130): back to Control Center, a new scan, a
    /// network (by its place in the list), the password field, Join,
    /// Cancel.
    WifiBack,
    WifiScan,
    WifiNetwork(usize),
    WifiField,
    WifiJoin,
    WifiCancel,
    /// Somewhere else in Control Center.
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

/// Where the top bar's right side ends (ADR-0117): its last item's right
/// edge. The Oceans logo is Wave's, not the bar's.
fn bar_end(width: i32) -> i32 {
    width - 14
}

/// The status icons at the top bar's right (ADR-0118, ADR-0121), as
/// macOS shows them, from the end in: the bell ([`ICON_BELL`]), Control
/// Center's, the network, the user, the speaker.
pub fn status_icon(width: i32, index: i32) -> Rect {
    Rect::new(bar_end(width) - 28 - index * 32, 2, 28, MENU_HEIGHT - 4)
}

pub const ICON_BELL: i32 = 0;
pub const ICON_CONTROL: i32 = 1;
pub const ICON_NETWORK: i32 = 2;
pub const ICON_USER: i32 = 3;
pub const ICON_SPEAKER: i32 = 4;

/// The speaker in the top bar (ADR-0100).
pub fn volume_button(width: i32) -> Rect {
    status_icon(width, ICON_SPEAKER)
}

/// The keyboard layout's name in the menu bar (ADR-0114), left of the
/// speaker: a click goes to the next layout.
pub fn layout_button(width: i32) -> Rect {
    let speaker = volume_button(width);
    Rect::new(speaker.x - 8 - 32, 2, 32, MENU_HEIGHT - 4)
}

/// The bell at the top bar's right end (ADR-0119, ADR-0121): it opens the
/// notifications, its count of new ones on it.
pub fn bell_button(width: i32) -> Rect {
    status_icon(width, ICON_BELL)
}

/// The user's menu (ADR-0121), under the user's icon: the account's name,
/// then its items.
pub fn user_menu(width: i32, items: usize) -> Rect {
    let icon = status_icon(width, ICON_USER);
    let w = 220;
    Rect::new(
        (icon.x + icon.w - w).min(width - 12 - w),
        MENU_HEIGHT + 6,
        w,
        44 + items as i32 * 32 + 8,
    )
}

/// Item `index` of the user's menu.
pub fn user_menu_item(width: i32, items: usize, index: usize) -> Rect {
    let menu = user_menu(width, items);
    Rect::new(menu.x + 6, menu.y + 44 + index as i32 * 32, menu.w - 12, 30)
}

/// Icon `index` on the desktop (ADR-0120), in a column down its right
/// side under the top bar: a 48-pixel picture and its name under it. A
/// column ends above the dock; the next starts left of it (ADR-0137).
pub fn desktop_icon(width: i32, height: i32, index: usize) -> Rect {
    let rows = ((height - MENU_HEIGHT - 14 - DOCK_RESERVE) / 100).max(1) as usize;
    let (column, row) = ((index / rows) as i32, (index % rows) as i32);
    Rect::new(
        width - 16 - 96 - column * 104,
        MENU_HEIGHT + 14 + row * 100,
        96,
        92,
    )
}

/// The notifications' panel (ADR-0119) under the bell, at the right as
/// macOS has it, as tall as the cards it shows (`count` notifications
/// kept; one row says there are none).
pub fn notice_panel(width: i32, count: usize) -> Rect {
    let cards = count.clamp(1, NOTICE_CARDS) as i32;
    // Room under the cards to say how many more there are.
    let foot = if count > NOTICE_CARDS { 26 } else { 8 };
    Rect::new(
        width - 12 - 360,
        MENU_HEIGHT + 6,
        360,
        140 + cards * 68 + foot,
    )
}

/// Card `index` in the panel, newest first.
pub fn notice_card(width: i32, count: usize, index: usize) -> Rect {
    let panel = notice_panel(width, count);
    Rect::new(
        panel.x + 12,
        panel.y + 140 + index as i32 * 68,
        panel.w - 24,
        60,
    )
}

/// Clear, which lets every notification go.
pub fn notice_clear(width: i32, count: usize) -> Rect {
    let panel = notice_panel(width, count);
    Rect::new(panel.x + panel.w - 12 - 72, panel.y + 106, 72, 26)
}

/// Control Center (ADR-0118), under the top bar's right end: modules on
/// a panel, as phones have them. The whole of it.
pub fn control_center(width: i32) -> Rect {
    Rect::new(width - 12 - CC_WIDTH, MENU_HEIGHT + 6, CC_WIDTH, CC_HEIGHT)
}

const CC_WIDTH: i32 = 340;
const CC_HEIGHT: i32 = 318;
/// A half-width module, and the gap between modules.
const CC_HALF: i32 = 153;
const CC_GAP: i32 = 10;

/// Control Center's modules: 0 the round toggles, 1 what plays, 2 the
/// keyboard, 3 the accent, 4 the sound.
pub fn cc_module(width: i32, which: usize) -> Rect {
    let cc = control_center(width);
    let (x0, y0) = (cc.x + 12, cc.y + 12);
    let right = x0 + CC_HALF + CC_GAP;
    match which {
        0 => Rect::new(x0, y0, CC_HALF, 140),
        1 => Rect::new(right, y0, CC_HALF, 140),
        2 => Rect::new(x0, y0 + 150, CC_HALF, 56),
        3 => Rect::new(right, y0 + 150, CC_HALF, 56),
        _ => Rect::new(x0, y0 + 216, 2 * CC_HALF + CC_GAP, 52),
    }
}

/// The round toggles (ADR-0118): 0 the network, 1 dark, 2 lock, 3
/// Settings; 44 pixels each, two by two.
pub fn cc_toggle(width: i32, index: usize) -> Rect {
    let m = cc_module(width, 0);
    let (col, row) = ((index % 2) as i32, (index / 2) as i32);
    let cx = m.x + m.w / 4 + col * m.w / 2;
    let cy = m.y + 34 + row * 66;
    Rect::new(cx - 22, cy - 22, 44, 44)
}

// ---- The Wi-Fi panel (ADR-0130), in Control Center's place ----------------

/// Its back button, top left.
pub fn wifi_back(width: i32) -> Rect {
    let cc = control_center(width);
    Rect::new(cc.x + 12, cc.y + 12, 32, 30)
}

/// Scan, top right.
pub fn wifi_scan(width: i32) -> Rect {
    let cc = control_center(width);
    Rect::new(cc.x + cc.w - 12 - 64, cc.y + 14, 64, 26)
}

/// The networks' rows, under the status line.
const WIFI_ROW: i32 = 34;
const WIFI_LIST_TOP: i32 = 82;
pub const WIFI_ROWS: usize = ((CC_HEIGHT - WIFI_LIST_TOP - 12) / WIFI_ROW) as usize;

pub fn wifi_row(width: i32, index: usize) -> Rect {
    let cc = control_center(width);
    Rect::new(
        cc.x + 12,
        cc.y + WIFI_LIST_TOP + index as i32 * WIFI_ROW,
        cc.w - 24,
        WIFI_ROW - 2,
    )
}

/// The password field, and Cancel and Join under it.
pub fn wifi_field(width: i32) -> Rect {
    let cc = control_center(width);
    Rect::new(cc.x + 16, cc.y + WIFI_LIST_TOP + 30, cc.w - 32, 34)
}

pub fn wifi_button(width: i32, join: bool) -> Rect {
    let field = wifi_field(width);
    let w = 92;
    let x = if join {
        field.x + field.w - w
    } else {
        field.x + field.w - 2 * w - 10
    };
    Rect::new(x, field.y + field.h + 14, w, 30)
}

/// The media keys the buttons press: Previous, Play/Pause, Next.
const MEDIA_KEYS: [u8; 3] = [
    oceans_window::proto::KEY_PREVIOUS,
    oceans_window::proto::KEY_PLAY_PAUSE,
    oceans_window::proto::KEY_NEXT,
];

/// What plays: Previous, Play/Pause, Next (0, 1, 2), across the module's
/// foot.
pub fn media_button(width: i32, which: usize) -> Rect {
    let m = cc_module(width, 1);
    let step = (m.w - 24) / 3;
    Rect::new(m.x + 12 + which as i32 * step, m.y + m.h - 44, step, 34)
}

/// The sound module's slider: a thick track right of the speaker.
pub fn volume_track(width: i32) -> Rect {
    let m = cc_module(width, 4);
    Rect::new(m.x + 54, m.y + 15, m.w - 68, 22)
}

fn volume_track_target(width: i32) -> Rect {
    let track = volume_track(width);
    Rect::new(track.x - 8, track.y - 8, track.w + 16, track.h + 16)
}

/// The speaker at the sound module's left: it mutes and unmutes.
pub fn volume_mute(width: i32) -> Rect {
    let m = cc_module(width, 4);
    Rect::new(m.x + 8, m.y + 8, 38, 36)
}

/// The level (0 to 100) at `x` along the slider.
pub fn volume_at(width: i32, x: i32) -> u8 {
    let track = volume_track(width);
    ((x - track.x) * 100 / track.w.max(1)).clamp(0, 100) as u8
}

/// Wave (ADR-0117): the one round button at the bottom's middle, all the
/// desktop shows of its taskbar until the pointer comes to it.
pub fn wave_button(width: i32, height: i32) -> Rect {
    Rect::new(
        (width - WAVE_BUTTON) / 2,
        height - WAVE_MARGIN - WAVE_BUTTON,
        WAVE_BUTTON,
        WAVE_BUTTON,
    )
}

/// Icon `index` of `count` when Wave is open (the Terminal, then the kept
/// apps and the windows): in a row over the button that rises to its
/// middle like a wave's crest.
pub fn wave_item(width: i32, height: i32, index: usize, count: usize) -> Rect {
    let button = wave_button(width, height);
    let step = ICON + WAVE_GAP;
    // From the middle, in halves of a step: -(count - 1) .. count - 1.
    let half = 2 * index as i32 - (count as i32 - 1);
    let x = width / 2 + half * step / 2 - ICON / 2;
    // The crest: the middle highest, falling off as a parabola.
    let reach = (count as i32).max(2);
    let rise = WAVE_CREST - WAVE_CREST * half * half / (reach * reach);
    let y = button.y - WAVE_LIFT - ICON - rise;
    Rect::new(x, y, ICON, ICON)
}

/// Where the pointer keeps Wave open: its icons and the button, with room
/// to move between them.
pub fn wave_zone(width: i32, height: i32, count: usize) -> Rect {
    let button = wave_button(width, height);
    let first = wave_item(width, height, 0, count.max(1));
    let last = wave_item(width, height, count.max(1) - 1, count.max(1));
    let top = (0..count.max(1))
        .map(|i| wave_item(width, height, i, count.max(1)).y)
        .min()
        .unwrap_or(first.y);
    let left = first.x.min(button.x) - 24;
    let right = (last.x + last.w).max(button.x + button.w) + 24;
    Rect::new(left, top - 40, right - left, height - (top - 40))
}

/// The Terminal window, in the middle of the area: 960 × 600 on a small
/// screen, seven tenths of the width and three quarters of the height of
/// a larger one (ADR-0137).
pub fn terminal(width: i32, height: i32) -> Rect {
    let area = area(width, height);
    let w = (area.w - 160).min((area.w * 7 / 10).max(960));
    let h = (area.h - 96).min((area.h * 3 / 4).max(600));
    Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
}

/// The Terminal's title bar buttons, from the right end (ADR-0117):
/// close (disabled: the shell stays), minimize, resize (disabled).
fn terminal_button(width: i32, height: i32, index: i32) -> Rect {
    let window = terminal(width, height);
    Rect::new(
        window.x + window.w - 6 - CLOSE_SIZE - index * BUTTON_STEP,
        window.y + (TITLE_HEIGHT - CLOSE_SIZE) / 2,
        CLOSE_SIZE,
        CLOSE_SIZE,
    )
}

pub fn terminal_minimize(width: i32, height: i32) -> Rect {
    terminal_button(width, height, 1)
}

/// Launchpad (ADR-0117): the apps across the whole screen under the top
/// bar, over the windows.
pub fn start_menu(width: i32, height: i32) -> Rect {
    Rect::new(0, MENU_HEIGHT, width, height - MENU_HEIGHT)
}

/// Launchpad's grid (ADR-0137): as many columns as fit with 200 pixels
/// kept free at either side (seven on a 1280-wide screen, from three to
/// ten), as many rows as fit above Restart and Shut Down.
fn tile_grid(width: i32, height: i32) -> (usize, usize) {
    let field = search_field(width, height);
    let top = field.y + field.h + 40;
    let bottom = power_button(width, height, 0).y - 16;
    let (columns, rows) = oceans_draw::layout::grid(
        (width - 2 * 200, bottom - top),
        (TILE_WIDTH, TILE_HEIGHT),
        3,
        10,
    );
    (columns as usize, rows as usize)
}

/// The tiles Launchpad shows: the Terminal and the apps that fit.
fn max_tiles(width: i32, height: i32) -> usize {
    let (columns, rows) = tile_grid(width, height);
    (columns * rows).max(1)
}

/// Tile `index` of Launchpad (0: the Terminal; then the apps), in a grid
/// centred under the search field.
pub fn tile(width: i32, height: i32, index: usize) -> Rect {
    let field = search_field(width, height);
    let (columns, _) = tile_grid(width, height);
    let grid = columns as i32 * TILE_WIDTH;
    let (col, row) = (index % columns, index / columns);
    Rect::new(
        (width - grid) / 2 + col as i32 * TILE_WIDTH,
        field.y + field.h + 40 + row as i32 * TILE_HEIGHT,
        TILE_WIDTH,
        TILE_HEIGHT,
    )
}

/// Lock, in Launchpad's corner (ADR-0112): left of Restart, or where
/// Restart would be on a desktop that cannot switch the machine off.
pub fn lock_button(width: i32, height: i32, can_power: bool) -> Rect {
    let first = power_button(width, height, if can_power { -1 } else { 1 });
    Rect::new(first.x, first.y, first.w, first.h)
}

/// The lock screen's password field (ADR-0110), in the middle.
pub fn lock_field(width: i32, height: i32) -> Rect {
    Rect::new((width - 320) / 2, height / 2, 320, 44)
}

/// The picture of account `index` of `count` at the lock screen
/// (ADR-0115): 72 pixels, in a row centred above the field.
pub fn lock_avatar(width: i32, height: i32, index: usize, count: usize) -> Rect {
    const SIZE: i32 = 72;
    const GAP: i32 = 36;
    let count = count.max(1) as i32;
    let row = count * SIZE + (count - 1) * GAP;
    let x = (width - row) / 2 + index as i32 * (SIZE + GAP);
    Rect::new(x, height / 2 - 130, SIZE, SIZE)
}

/// Launchpad's search field (ADR-0108), at the top in the middle.
pub fn search_field(width: i32, height: i32) -> Rect {
    let sheet = start_menu(width, height);
    Rect::new((width - SEARCH_WIDTH) / 2, sheet.y + 36, SEARCH_WIDTH, 36)
}

/// Search result `index`, a row under the field where the tiles are.
pub fn found_row(width: i32, height: i32, index: usize) -> Rect {
    let field = search_field(width, height);
    Rect::new(
        (width - FOUND_WIDTH) / 2,
        field.y + field.h + 24 + index as i32 * 38,
        FOUND_WIDTH,
        34,
    )
}

/// Restart (0) and Shut Down (1), at Launchpad's bottom right (Lock, at
/// -1, before them).
pub fn power_button(width: i32, height: i32, index: i32) -> Rect {
    let (w, gap) = (104, 8);
    Rect::new(
        width - 32 - 2 * w - gap + index * (w + gap),
        height - WAVE_MARGIN - 38,
        w,
        34,
    )
}

/// A system dialog, in the middle of the area, never wider than it.
pub fn dialog(width: i32, height: i32) -> Rect {
    let area = area(width, height);
    let w = DIALOG_WIDTH.min(area.w - 32);
    Rect::new(
        area.x + (area.w - w) / 2,
        area.y + (area.h - DIALOG_HEIGHT) / 2,
        w,
        DIALOG_HEIGHT,
    )
}

/// The dialog's buttons, from its right end (ADR-0137).
pub fn allow(width: i32, height: i32) -> Rect {
    let d = dialog(width, height);
    Rect::new(
        d.x + d.w - 24 - BUTTON_WIDTH,
        d.y + 204,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
    )
}

pub fn deny(width: i32, height: i32) -> Rect {
    let a = allow(width, height);
    Rect::new(a.x - 16 - BUTTON_WIDTH, a.y, BUTTON_WIDTH, BUTTON_HEIGHT)
}

/// The windows the dock shows, in the order they were opened.
pub fn taskbar_windows<'a>(windows: &'a [WindowView<'a>]) -> Vec<&'a WindowView<'a>> {
    let mut shown: Vec<&WindowView<'_>> = windows.iter().collect();
    shown.sort_unstable_by_key(|view| view.frame.id);
    shown
}

/// The time and the date in words (ADR-0119): ("01:14", "Tuesday 6
/// October"), from milliseconds since 1970 (UTC).
pub fn clock_long(unix_ms: u64) -> (String, String) {
    const DAY_NAMES: [&str; 7] = [
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
    ];
    const MONTH_NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let days = (unix_ms / 86_400_000) as u32;
    let minutes = unix_ms / 60_000;
    let (_, month, day) = oceans_package::date::civil(days);
    (
        alloc::format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60),
        alloc::format!(
            "{} {day} {}",
            DAY_NAMES[days as usize % 7],
            MONTH_NAMES[(month as usize).saturating_sub(1) % 12]
        ),
    )
}

/// How long ago, as a notification's card says it: "now", "5 min ago",
/// "2 h ago".
pub fn ago(then_ms: u64, now_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    match minutes {
        0 => String::from("now"),
        1..=59 => alloc::format!("{minutes} min ago"),
        _ => alloc::format!("{} h ago", minutes / 60),
    }
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

/// The wallpaper (ADR-0117): a photograph of the ocean (`photo`, the PNG
/// the boot image brings as a module), cut to the screen's shape from its
/// middle and scaled to fill it. Made once; the drawn one without the
/// picture, or if it cannot be read.
pub fn wallpaper(width: i32, height: i32, photo: Option<&[u8]>) -> Vec<u32> {
    let (w, h) = (width.max(1) as u32, height.max(1) as u32);
    let Some(Ok((_, photo))) = photo.map(oceans_image::decode) else {
        return drawn_wallpaper(width, height);
    };
    // The largest part of the photograph with the screen's shape.
    let (pw, ph) = (u64::from(photo.width), u64::from(photo.height));
    let (cw, ch) = if pw * u64::from(h) > ph * u64::from(w) {
        (ph * u64::from(w) / u64::from(h), ph)
    } else {
        (pw, pw * u64::from(h) / u64::from(w))
    };
    let (cx, cy) = ((pw - cw) / 2, (ph - ch) / 2);
    let mut pixels = Vec::with_capacity((cw * ch) as usize);
    for row in cy..cy + ch {
        let start = (row * pw + cx) as usize;
        pixels.extend_from_slice(&photo.pixels[start..start + cw as usize]);
    }
    drop(photo);
    let cut = oceans_image::Image {
        width: cw as u32,
        height: ch as u32,
        pixels,
    };
    let mut out = oceans_image::scale(&cut, w, h);
    for pixel in &mut out {
        *pixel &= 0x00ff_ffff;
    }
    out
}

/// The drawn ocean (ADR-0078): night sky to shallow water, and three
/// waves rolling in, nearer ones darker.
fn drawn_wallpaper(width: i32, height: i32) -> Vec<u32> {
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
    /// The wheel over the Terminal (ADR-0138): `up` detents away from the
    /// user show earlier rows, three a detent, as far back as the console
    /// keeps. `true` if what it shows changed.
    pub fn scroll_terminal(&mut self, up: i32) -> bool {
        let before = self.terminal_back;
        let most = self.terminal.row;
        self.terminal_back =
            (self.terminal_back as i64 + i64::from(up) * 3).clamp(0, most as i64) as usize;
        self.terminal_back != before
    }

    /// The apps kept in Wave that are installed (ADR-0109): id and name,
    /// in their order.
    pub fn kept(&self) -> Vec<(&str, &str)> {
        self.pinned
            .iter()
            .filter_map(|id| {
                self.apps
                    .iter()
                    .find(|a| &a.id == id)
                    .map(|a| (a.id.as_str(), a.name.as_str()))
            })
            .collect()
    }

    /// The user's menu (ADR-0121): Lock Screen with a password, Switch User
    /// with other accounts, and Settings.
    pub fn user_items(&self) -> Vec<(&'static str, UserAction)> {
        let mut items = Vec::new();
        if self.password_set {
            items.push(("Lock Screen", UserAction::Lock));
        }
        if self.users.len() > 1 {
            items.push(("Switch User...", UserAction::Switch));
        }
        items.push(("Users & Accounts...", UserAction::Settings));
        items
    }

    /// What the desktop holds (ADR-0120), in a column at its right: Home
    /// (it opens in Files), then the apps kept in Wave; each a label and
    /// the app it opens.
    pub fn desktop_items(&self) -> Vec<(String, String)> {
        let mut items = Vec::new();
        if self.apps.iter().any(|a| a.id == FILES_APP) {
            items.push((String::from("Home"), String::from(FILES_APP)));
        }
        for (id, name) in self.kept() {
            if id != FILES_APP {
                items.push((String::from(name), String::from(id)));
            }
        }
        items.truncate(MAX_DESKTOP_ITEMS);
        items
    }

    /// Wave opens when the pointer reaches its button and stays open while
    /// the pointer is over it or its icons (ADR-0117); Launchpad, a dialog
    /// or the lock screen close it. `true` if that changed.
    pub fn track_wave(&mut self, width: i32, height: i32, windows: &[DockWindow]) -> bool {
        let (x, y) = self.pointer;
        let count = 1 + self.dock_entries(windows).len();
        let open = if self.start_open || self.dialog.is_some() || self.lock != Lock::Open {
            false
        } else if wave_button(width, height).contains(x, y) {
            true
        } else {
            self.wave_open && wave_zone(width, height, count).contains(x, y)
        };
        let changed = open != self.wave_open;
        self.wave_open = open;
        changed
    }

    /// Wave's icons after the Terminal (ADR-0109, ADR-0117).
    pub fn dock_entries(&self, windows: &[DockWindow]) -> Vec<dock::Entry> {
        let kept: Vec<&str> = self.kept().into_iter().map(|(_, name)| name).collect();
        let windows: Vec<(u32, &str)> = windows.iter().map(|(id, a)| (*id, a.as_str())).collect();
        dock::entries(&kept, &windows)
    }

    /// What a click on the desktop (the top bar, Wave, Launchpad, the
    /// panels, dialogs, the Terminal, the desktop's icons: everything but
    /// app windows) hits, on a `width × height` screen; `windows` as Wave
    /// shows them.
    pub fn hit(&self, x: i32, y: i32, width: i32, height: i32, windows: &[DockWindow]) -> Hit {
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
        let entries = self.dock_entries(windows);
        // Wave (ADR-0117): its button opens Launchpad, as Super
        // does; its icons, while it is open, are the Terminal's, the kept
        // apps' and the windows'.
        let count = 1 + entries.len();
        if wave_button(width, height).contains(x, y) {
            return Hit::Start;
        }
        if self.wave_open && !self.start_open {
            if wave_item(width, height, 0, count).contains(x, y) {
                return Hit::TaskbarTerminal;
            }
            if let Some(&entry) = entries
                .iter()
                .enumerate()
                .find(|&(i, _)| wave_item(width, height, i + 1, count).contains(x, y))
                .map(|(_, entry)| entry)
            {
                return match entry {
                    dock::Entry::Pinned(index) => Hit::DockPinned(index),
                    dock::Entry::Window(id) => Hit::TaskbarWindow(id),
                };
            }
        }
        if layout_button(width).contains(x, y) {
            return Hit::Layout;
        }
        // The bell and its notifications (ADR-0119).
        if bell_button(width).contains(x, y) {
            return Hit::Clock;
        }
        let count = self.notices.len();
        if self.notices_open && notice_panel(width, count).contains(x, y) {
            return if count > 0 && notice_clear(width, count).contains(x, y) {
                Hit::ClearNotices
            } else {
                Hit::NoticePanel
            };
        }
        // The user's icon and menu (ADR-0121).
        if status_icon(width, ICON_USER).contains(x, y) {
            return Hit::User;
        }
        let items = self.user_items();
        if self.user_menu_open && user_menu(width, items.len()).contains(x, y) {
            return (0..items.len())
                .find(|&i| user_menu_item(width, items.len(), i).contains(x, y))
                .map_or(Hit::NoticePanel, |i| Hit::UserItem(items[i].1));
        }
        // The other status icons open Control Center (ADR-0118).
        if [ICON_CONTROL, ICON_NETWORK, ICON_SPEAKER]
            .iter()
            .any(|&i| status_icon(width, i).contains(x, y))
        {
            return Hit::Volume;
        }
        if self.volume_open && self.wifi_open && control_center(width).contains(x, y) {
            if wifi_back(width).contains(x, y) {
                return Hit::WifiBack;
            }
            if self.wifi_entry.is_some() {
                return if wifi_field(width).contains(x, y) {
                    Hit::WifiField
                } else if wifi_button(width, true).contains(x, y) {
                    Hit::WifiJoin
                } else if wifi_button(width, false).contains(x, y) {
                    Hit::WifiCancel
                } else {
                    Hit::VolumePanel
                };
            }
            if wifi_scan(width).contains(x, y) {
                return Hit::WifiScan;
            }
            let shown = self
                .wifi
                .as_ref()
                .map_or(0, |w| w.networks.len().min(WIFI_ROWS));
            return (0..shown)
                .find(|&i| wifi_row(width, i).contains(x, y))
                .map_or(Hit::VolumePanel, Hit::WifiNetwork);
        }
        if self.volume_open && control_center(width).contains(x, y) {
            let media = self.now_playing.is_some();
            return if let Some(i) = (0..4).find(|&i| cc_toggle(width, i).contains(x, y)) {
                Hit::Toggle(i as u8)
            } else if let Some(which) =
                (0..3).find(|&i| media && media_button(width, i).contains(x, y))
            {
                Hit::Media(MEDIA_KEYS[which])
            } else if cc_module(width, 2).contains(x, y) {
                Hit::Layout
            } else if cc_module(width, 3).contains(x, y) {
                Hit::Accent
            } else if self.volume.is_some() && volume_track_target(width).contains(x, y) {
                Hit::VolumeLevel(volume_at(width, x))
            } else if self.volume.is_some() && volume_mute(width).contains(x, y) {
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
                if self.password_set && lock_button(width, height, self.can_power).contains(x, y) {
                    return Hit::Lock;
                }
                if self.can_power {
                    if power_button(width, height, 0).contains(x, y) {
                        return Hit::Restart;
                    }
                    if power_button(width, height, 1).contains(x, y) {
                        return Hit::ShutDown;
                    }
                }
                if !self.search.is_empty() {
                    return (0..self.found.len())
                        .find(|&i| found_row(width, height, i).contains(x, y))
                        .map_or(Hit::Nothing, Hit::Found);
                }
                if search_field(width, height).contains(x, y) {
                    return Hit::Nothing;
                }
                // As Launchpad does: a click between the icons closes it.
                let shown = self.apps.len().min(max_tiles(width, height) - 1);
                return (0..shown)
                    .find(|&i| tile(width, height, i + 1).contains(x, y))
                    .map_or(Hit::Outside, Hit::StartApp);
            }
            return Hit::Outside;
        }
        if !self.terminal_hidden {
            if terminal_minimize(width, height).contains(x, y) {
                return Hit::TerminalMinimize;
            }
            if terminal(width, height).contains(x, y) {
                return Hit::Terminal;
            }
        }
        // The desktop's icons (ADR-0120), under everything else.
        let items = self.desktop_items().len();
        if let Some(index) = (0..items).find(|&i| desktop_icon(width, height, i).contains(x, y)) {
            return Hit::DesktopIcon(index);
        }
        Hit::Nothing
    }

    pub fn draw(&self, canvas: &mut Canvas, now_ms: u64, windows: &[WindowView<'_>]) {
        canvas.draw_wallpaper();
        // Locked (ADR-0110): the lock screen alone; no window, the Terminal,
        // a notification or a dialog shows.
        if self.lock != Lock::Open {
            return self.draw_lock_screen(canvas);
        }

        // The desktop's icons (ADR-0120), on the wallpaper under it all.
        self.draw_desktop_icons(canvas);

        if !self.terminal_hidden {
            self.draw_terminal(canvas);
        }

        // App windows, bottom to top.
        for view in windows.iter().filter(|view| !view.frame.minimized) {
            draw_window(canvas, view, self.pointer);
        }

        // Where a dragged window would go (ADR-0107): a tinted sheet.
        if let Some(r) = self.snap {
            let inset = Rect::new(r.x + 4, r.y + 4, r.w - 8, r.h - 8);
            canvas.tint(inset, 12, colour::accent(), 70);
        }

        // Launchpad under Wave's button, which closes it again (ADR-0117).
        if self.start_open {
            self.draw_apps_panel(canvas);
        }
        if !self.full_screen {
            self.draw_menu_bar(canvas, windows);
            self.draw_dock(canvas, windows);
        }
        // Control Center (ADR-0118), with or without a sound device.
        if self.volume_open {
            self.draw_control_center(canvas);
        }
        // The notifications (ADR-0119), under the bell.
        if self.notices_open {
            self.draw_notices(canvas, now_ms);
        }
        // The user's menu (ADR-0121).
        if self.user_menu_open {
            self.draw_user_menu(canvas);
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
            canvas.round_fill(r, 12, colour::dialog_surface());
            let mark = if toast.error { CLOSE } else { colour::accent() };
            canvas.circle(r.x + 22, r.y + r.h / 2, 7, mark);
            canvas.text(
                r.x + 40,
                r.y + 19,
                &toast.text,
                Font::Body,
                colour::text(),
                r,
            );
        }

        // A system dialog, over everything else.
        if let Some(dialog) = &self.dialog {
            self.draw_dialog(canvas, dialog);
        }

        let (x, y) = self.pointer_device;
        canvas.cursor(x, y, WHITE, BLACK);
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
            window.x + window.w - 6 - 3 * BUTTON_STEP,
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
            // Ending at the cursor's row, or as many rows back as the
            // wheel turned (ADR-0138).
            let end = (t.row + 1).min(t.rows);
            let back = self.terminal_back.min(end.saturating_sub(visible));
            let last = end - back;
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
                        colour::accent(),
                    );
                }
            }
        }
    }

    /// The top bar (ADR-0117): no band of its own, the wallpaper through
    /// it under a soft shade, in white. At the left the time and date; at
    /// the right, from the end in, the Oceans logo, the system's state,
    /// the speaker, the keyboard layout and the app in use.
    fn draw_menu_bar(&self, canvas: &mut Canvas, windows: &[WindowView<'_>]) {
        let w = canvas.width;
        let bar = menu_bar(w);
        // The shade: darkest at the top, gone at the bar's foot.
        for row in 0..MENU_HEIGHT {
            let alpha = (70 * (MENU_HEIGHT - row) / MENU_HEIGHT) as u32;
            canvas.tint(Rect::new(0, row, w, 1), 0, BLACK, alpha);
        }
        let hover = |r: Rect| r.contains(self.pointer.0, self.pointer.1);
        let lit = |canvas: &mut Canvas, r: Rect, open: bool| {
            if open || hover(r) {
                canvas.tint(r, 6, WHITE, 45);
            }
        };
        // The left (ADR-0121): the time and date, then the app in use,
        // bold, as macOS names it.
        let end = canvas.text(14, 6, &self.clock, Font::Strong, WHITE, bar);
        let current = windows
            .iter()
            .find(|view| view.focused && !view.frame.minimized)
            .map(|view| view.frame.app.as_str())
            .or((self.terminal_focused && !self.terminal_hidden).then_some("Terminal"))
            .unwrap_or("Oceans");
        canvas.text(end + 22, 6, current, Font::Strong, WHITE, bar);

        // The right, from the end in, as macOS has it: the bell (ADR-0119)
        // with how many came since in red, Control Center (ADR-0118), the
        // network, the user (ADR-0121), the speaker, the keyboard layout.
        let bell = bell_button(w);
        lit(canvas, bell, self.notices_open);
        glyph(canvas, bell, BAR_ICON, |s, r| icon::bell(s, r, WHITE));
        if self.unread > 0 {
            let count = alloc::format!("{}", self.unread.min(99));
            let cw = canvas.measure(&count, Font::Body);
            // On the bell's top right, kept on the screen at the bar's end.
            let bw = (cw + 8).max(16);
            let badge = Rect::new((bell.x + bell.w - 12).min(w - 2 - bw), 1, bw, 16);
            canvas.round_fill(badge, 8, CLOSE);
            canvas.text(
                badge.x + (badge.w - cw) / 2,
                0,
                &count,
                Font::Body,
                WHITE,
                badge,
            );
        }
        let cc = status_icon(w, ICON_CONTROL);
        lit(canvas, cc, self.volume_open);
        glyph(canvas, cc, BAR_ICON, |s, r| icon::controls(s, r, WHITE));
        if self.network.is_some() || self.wifi.is_some() {
            let r = status_icon(w, ICON_NETWORK);
            lit(canvas, r, false);
            let link = self.link();
            glyph(canvas, r, BAR_ICON, |s, r| {
                link.draw(s, r, WHITE, BAR_FAINT)
            });
        }
        let user = status_icon(w, ICON_USER);
        lit(canvas, user, self.user_menu_open);
        glyph(canvas, user, BAR_ICON, |s, r| icon::user(s, r, WHITE));
        if let Some((level, muted)) = self.volume {
            let button = volume_button(w);
            lit(canvas, button, false);
            speaker(canvas, button, level, muted, WHITE);
        }
        let r = layout_button(w);
        lit(canvas, r, false);
        let name = self.layout.short_name();
        let x = r.x + (r.w - canvas.measure(name, Font::Strong)) / 2;
        canvas.text(x, 6, name, Font::Strong, WHITE, bar);
    }

    /// The user's menu (ADR-0121): the account's name, then Lock Screen,
    /// Switch User and Users & Accounts.
    fn draw_user_menu(&self, canvas: &mut Canvas) {
        let w = canvas.width;
        let items = self.user_items();
        let menu = user_menu(w, items.len());
        canvas.shadow(menu, 12, 70);
        canvas.round_fill(menu, 12, colour::panel());
        let clip = Rect::new(menu.x + 14, menu.y, menu.w - 28, menu.h);
        canvas.text(
            menu.x + 14,
            menu.y + 12,
            &self.user_name,
            Font::Strong,
            colour::text(),
            clip,
        );
        canvas.fill(
            Rect::new(menu.x + 10, menu.y + 38, menu.w - 20, 1),
            colour::button_secondary(),
        );
        for (i, (label, _)) in items.iter().enumerate() {
            let r = user_menu_item(w, items.len(), i);
            if r.contains(self.pointer.0, self.pointer.1) {
                canvas.round_fill(r, 6, colour::accent());
                canvas.text(r.x + 8, r.y + 6, label, Font::Body, WHITE, r);
            } else {
                canvas.text(r.x + 8, r.y + 6, label, Font::Body, colour::text(), r);
            }
        }
    }

    /// The desktop's icons (ADR-0120): Home as a folder, the kept apps as
    /// their icons, each name in white on a soft shade; the chosen one
    /// lit.
    fn draw_desktop_icons(&self, canvas: &mut Canvas) {
        let w = canvas.width;
        for (i, (label, id)) in self.desktop_items().iter().enumerate() {
            let tile = desktop_icon(w, canvas.height, i);
            if self.icon_chosen == Some(i) {
                canvas.tint(tile, 12, WHITE, 60);
            } else if tile.contains(self.pointer.0, self.pointer.1) {
                canvas.tint(tile, 12, WHITE, 30);
            }
            let icon = Rect::new(tile.x + (tile.w - ICON) / 2, tile.y + 6, ICON, ICON);
            if id == FILES_APP && label == "Home" {
                folder_icon(canvas, icon);
            } else {
                app_icon(canvas, icon, label);
            }
            let lw = canvas.measure(label, Font::Body).min(tile.w - 8);
            let shade = Rect::new(tile.x + (tile.w - lw) / 2 - 6, tile.y + 60, lw + 12, 22);
            canvas.tint(shade, 8, BLACK, 90);
            canvas.text(shade.x + 6, shade.y + 2, label, Font::Body, WHITE, shade);
        }
    }

    /// The notifications (ADR-0119), as phones show them: the time large
    /// and the date, then a card for each, newest first: the app, how long
    /// ago, and what it said.
    fn draw_notices(&self, canvas: &mut Canvas, now_ms: u64) {
        let count = self.notices.len();
        let panel = notice_panel(canvas.width, count);
        canvas.shadow(panel, 16, 80);
        canvas.round_fill(panel, 18, colour::panel());
        let clip = Rect::new(panel.x + 18, panel.y, panel.w - 36, panel.h);
        canvas.text(
            panel.x + 18,
            panel.y + 6,
            &self.clock_time,
            Font::Display,
            colour::text(),
            clip,
        );
        canvas.text(
            panel.x + 18,
            panel.y + 70,
            &self.clock_date,
            Font::Body,
            colour::muted(),
            clip,
        );
        canvas.text(
            panel.x + 18,
            panel.y + 110,
            "Notifications",
            Font::Strong,
            colour::text(),
            clip,
        );
        if count == 0 {
            let card = notice_card(canvas.width, 1, 0);
            canvas.round_fill(card, 14, colour::surface());
            canvas.text(
                card.x + 14,
                card.y + 20,
                "No notifications",
                Font::Body,
                colour::muted(),
                card,
            );
            return;
        }
        let clear = notice_clear(canvas.width, count);
        let hovered = clear.contains(self.pointer.0, self.pointer.1);
        canvas.round_fill(
            clear,
            13,
            if hovered {
                Rgb(0xd5_d5_db)
            } else {
                colour::button_secondary()
            },
        );
        let cw = canvas.measure("Clear", Font::Body);
        canvas.text(
            clear.x + (clear.w - cw) / 2,
            clear.y + 4,
            "Clear",
            Font::Body,
            colour::text(),
            clear,
        );
        for (i, notice) in self.notices.iter().take(NOTICE_CARDS).enumerate() {
            let card = notice_card(canvas.width, count, i);
            canvas.round_fill(card, 14, colour::surface());
            let icon = Rect::new(card.x + 10, card.y + 12, 36, 36);
            app_icon(canvas, icon, &notice.app);
            let text = Rect::new(card.x + 56, card.y, card.w - 66, card.h);
            let when = ago(notice.at_ms, now_ms);
            let ww = canvas.measure(&when, Font::Body);
            canvas.text(
                text.x + text.w - ww,
                card.y + 9,
                &when,
                Font::Body,
                colour::muted(),
                text,
            );
            let head = Rect::new(text.x, text.y, text.w - ww - 8, text.h);
            canvas.text(
                text.x,
                card.y + 9,
                &notice.app,
                Font::Body,
                colour::muted(),
                head,
            );
            canvas.text(
                text.x,
                card.y + 31,
                &notice.text,
                Font::Strong,
                colour::text(),
                text,
            );
        }
        if count > NOTICE_CARDS {
            let more = alloc::format!("and {} more", count - NOTICE_CARDS);
            let last = notice_card(canvas.width, count, NOTICE_CARDS - 1);
            canvas.text(
                last.x + 4,
                last.y + last.h + 2,
                &more,
                Font::Body,
                colour::muted(),
                panel,
            );
        }
    }

    /// The Wi-Fi panel (ADR-0130), in Control Center's place: back, the
    /// title and Scan; what Wi-Fi is doing; the networks, strongest first,
    /// or the password of the one chosen.
    fn draw_wifi(&self, canvas: &mut Canvas) {
        let w = canvas.width;
        let cc = control_center(w);
        let pointer = self.pointer;
        let hover = |r: Rect| r.contains(pointer.0, pointer.1);
        let clip = Rect::new(cc.x + 12, cc.y, cc.w - 24, cc.h);

        let back = wifi_back(w);
        if hover(back) {
            canvas.tint(back, 8, BLACK, 20);
        }
        chevron_left(
            canvas,
            back.x + back.w / 2,
            back.y + back.h / 2,
            colour::text(),
        );
        canvas.text(
            back.x + back.w + 6,
            cc.y + 17,
            "Wi-Fi",
            Font::Strong,
            colour::text(),
            clip,
        );

        let wifi = self.wifi.as_ref();
        let state = wifi.map_or(&WifiState::Off, |w| &w.state);
        let (line, error) = match state {
            WifiState::Off => (String::from("Wi-Fi is off: no Wi-Fi device works"), true),
            WifiState::Idle => (String::from("Not connected"), false),
            WifiState::Scanning => (String::from("Looking for networks..."), false),
            WifiState::Joining(name) => (alloc::format!("Joining {name}..."), false),
            WifiState::Joined { name, .. } => (alloc::format!("Connected to {name}"), false),
            WifiState::Failed { name, reason } => {
                (alloc::format!("Could not join {name}: {reason}"), true)
            }
        };
        let ink = if error {
            Rgb(0xd7_3a_49)
        } else {
            colour::muted()
        };
        canvas.text(cc.x + 16, cc.y + 52, &line, Font::Body, ink, clip);

        if let Some(entry) = &self.wifi_entry {
            let prompt = alloc::format!("Password for {}", entry.name);
            canvas.text(
                cc.x + 16,
                cc.y + WIFI_LIST_TOP + 4,
                &prompt,
                Font::Strong,
                colour::text(),
                clip,
            );
            let field = wifi_field(w);
            canvas.round_fill(field, 8, colour::surface());
            canvas.round_fill(
                Rect::new(field.x, field.y + field.h - 2, field.w, 2),
                1,
                colour::accent(),
            );
            // Dots, never the password; the last ones if it is long.
            let count = entry.password.chars().count();
            let shown = count.min(((field.w - 24) / 12) as usize);
            for i in 0..shown {
                canvas.circle(
                    field.x + 14 + i as i32 * 12,
                    field.y + field.h / 2,
                    4,
                    colour::text(),
                );
            }
            let caret = field.x + 10 + shown as i32 * 12;
            canvas.fill(
                Rect::new(caret, field.y + 8, 2, field.h - 16),
                colour::accent(),
            );
            let hint = if count < 8 {
                "8 characters or more"
            } else {
                "Enter joins, Escape cancels"
            };
            // On its own line, under the buttons.
            let below = wifi_button(w, true);
            canvas.text(
                field.x,
                below.y + below.h + 14,
                hint,
                Font::Body,
                colour::muted(),
                Rect::new(field.x, below.y + below.h, field.w, 40),
            );
            for join in [false, true] {
                let b = wifi_button(w, join);
                let ready = !join || count >= 8;
                let fill = if join && ready {
                    colour::accent()
                } else {
                    colour::button_secondary()
                };
                canvas.round_fill(b, 8, fill);
                if hover(b) && ready {
                    canvas.tint(b, 8, BLACK, 20);
                }
                let label = if join { "Join" } else { "Cancel" };
                let ink = if join && ready {
                    WHITE
                } else if ready {
                    colour::text()
                } else {
                    colour::muted()
                };
                let lw = canvas.measure(label, Font::Strong);
                canvas.text(b.x + (b.w - lw) / 2, b.y + 6, label, Font::Strong, ink, b);
            }
            return;
        }

        let scan = wifi_scan(w);
        let scanning = matches!(state, WifiState::Scanning);
        canvas.round_fill(scan, 8, colour::button_secondary());
        if hover(scan) && !scanning {
            canvas.tint(scan, 8, BLACK, 20);
        }
        let label = "Scan";
        let lw = canvas.measure(label, Font::Body);
        let ink = if scanning {
            colour::muted()
        } else {
            colour::text()
        };
        canvas.text(
            scan.x + (scan.w - lw) / 2,
            scan.y + 5,
            label,
            Font::Body,
            ink,
            scan,
        );

        let networks = wifi.map_or(&[][..], |w| &w.networks[..]);
        if networks.is_empty() {
            let none = if scanning { "" } else { "No networks found" };
            canvas.text(
                cc.x + 16,
                cc.y + WIFI_LIST_TOP + 8,
                none,
                Font::Body,
                colour::muted(),
                clip,
            );
        }
        let joined = match state {
            WifiState::Joined { name, .. } => Some(name.as_str()),
            _ => None,
        };
        for (i, network) in networks.iter().take(WIFI_ROWS).enumerate() {
            let row = wifi_row(w, i);
            let name = ssid_name(network.ssid.as_bytes());
            let connected = joined == Some(name.as_str());
            if connected {
                canvas.round_fill(row, 8, colour::surface());
            }
            if hover(row) && network.security != Security::Unsupported {
                canvas.tint(row, 8, BLACK, 16);
            }
            let ink = if network.security == Security::Unsupported {
                colour::muted()
            } else {
                colour::text()
            };
            // The network joined: its bars in the accent.
            let bars = if connected { colour::accent() } else { ink };
            signal_icon(canvas, row.x + 16, row.y + row.h / 2, network.rssi, bars);
            let right = match (connected, network.security) {
                (true, _) => "Connected",
                (false, Security::Wpa2Personal) => "WPA2",
                (false, Security::Open) => "Open",
                (false, Security::Unsupported) => "Not supported",
            };
            let rw = canvas.measure(right, Font::Body);
            let name_clip = Rect::new(row.x + 36, row.y, row.w - 44 - rw - 8, row.h);
            let font = if connected { Font::Strong } else { Font::Body };
            canvas.text(row.x + 36, row.y + 7, &name, font, ink, name_clip);
            let right_ink = if connected {
                colour::accent()
            } else {
                colour::muted()
            };
            canvas.text(
                row.x + row.w - 8 - rw,
                row.y + 7,
                right,
                Font::Body,
                right_ink,
                row,
            );
        }
    }

    /// Control Center (ADR-0118): the round toggles, what plays, the
    /// keyboard and the accent, the sound, and a line of the system's
    /// state, each module on its own surface.
    fn draw_control_center(&self, canvas: &mut Canvas) {
        let w = canvas.width;
        let cc = control_center(w);
        canvas.shadow(cc, 16, 80);
        canvas.round_fill(cc, 18, colour::panel());
        if self.wifi_open {
            self.draw_wifi(canvas);
            return;
        }
        for which in 0..5 {
            canvas.round_fill(cc_module(w, which), 14, colour::surface());
        }
        let pointer = self.pointer;
        let (dark, accent) = oceans_draw::theme::bytes();

        // The round toggles, each with its name under it.
        let connected = self.network.as_ref().is_some_and(|n| n.connected);
        let toggles: [(&str, bool); 4] = [
            ("Network", connected),
            ("Dark", dark == 1),
            ("Lock", false),
            ("Settings", false),
        ];
        for (i, (name, on)) in toggles.iter().enumerate() {
            let r = cc_toggle(w, i);
            let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
            let fill = if *on {
                colour::accent()
            } else if r.contains(pointer.0, pointer.1) {
                Rgb(0xd5_d5_db)
            } else {
                colour::button_secondary()
            };
            canvas.circle(cx, cy, r.w / 2, fill);
            let ink = if *on { WHITE } else { colour::text() };
            match i {
                0 => glyph_at(canvas, (cx, cy), 20, |s, r| icon::wifi(s, r, 3, ink, ink)),
                1 => moon_icon(canvas, cx, cy, ink, fill),
                2 => lock_icon(canvas, cx, cy, ink, fill),
                _ => gear_icon(canvas, cx, cy, ink, fill),
            }
            let lw = canvas.measure(name, Font::Body);
            let label = Rect::new(cx - 40, r.y + r.h + 2, 80, 18);
            canvas.text(
                cx - lw / 2,
                label.y,
                name,
                Font::Body,
                colour::muted(),
                label,
            );
        }

        // What plays (ADR-0103), or that nothing does.
        let m = cc_module(w, 1);
        let clip = Rect::new(m.x + 12, m.y, m.w - 24, m.h);
        canvas.text(
            m.x + 12,
            m.y + 12,
            "Now playing",
            Font::Body,
            colour::muted(),
            clip,
        );
        match &self.now_playing {
            Some((app, state, title)) => {
                canvas.text(
                    m.x + 12,
                    m.y + 34,
                    title,
                    Font::Strong,
                    colour::text(),
                    clip,
                );
                canvas.text(m.x + 12, m.y + 56, app, Font::Body, colour::muted(), clip);
                let playing = *state == oceans_window::proto::playing::PLAYING;
                for which in 0..3 {
                    let b = media_button(w, which);
                    if b.contains(pointer.0, pointer.1) {
                        canvas.tint(b, 8, BLACK, 20);
                    }
                    let (cx, cy) = (b.x + b.w / 2, b.y + b.h / 2);
                    match which {
                        0 => skip_icon(canvas, cx, cy, false),
                        1 if playing => pause_icon(canvas, cx, cy),
                        1 => play_icon(canvas, cx, cy),
                        _ => skip_icon(canvas, cx, cy, true),
                    }
                }
            }
            None => {
                canvas.text(
                    m.x + 12,
                    m.y + 34,
                    "Nothing",
                    Font::Strong,
                    colour::text(),
                    clip,
                );
            }
        }

        // The keyboard layout (ADR-0114) and the accent (ADR-0113).
        let accents = oceans_draw::theme::ACCENTS;
        let (accent_name, accent_colour) = accents[usize::from(accent) % accents.len()];
        let tiles: [(usize, &str, &str); 2] = [
            (2, self.layout.name(), "Keyboard"),
            (3, accent_name, "Accent"),
        ];
        for (which, value, what) in tiles {
            let t = cc_module(w, which);
            if t.contains(pointer.0, pointer.1) {
                canvas.tint(t, 14, BLACK, 14);
            }
            let (cx, cy) = (t.x + 28, t.y + t.h / 2);
            if which == 2 {
                canvas.circle(cx, cy, 16, colour::button_secondary());
                let short = self.layout.short_name();
                let sw = canvas.measure(short, Font::Strong);
                canvas.text(cx - sw / 2, cy - 9, short, Font::Strong, colour::text(), t);
            } else {
                canvas.circle(cx, cy, 16, Rgb(accent_colour.0));
            }
            let clip = Rect::new(t.x + 52, t.y, t.w - 60, t.h);
            canvas.text(t.x + 52, t.y + 9, value, Font::Strong, colour::text(), clip);
            canvas.text(t.x + 52, t.y + 29, what, Font::Body, colour::muted(), clip);
        }

        // The sound (ADR-0100): the speaker mutes, the slider sets.
        if let Some((level, muted)) = self.volume {
            let button = volume_mute(w);
            if button.contains(pointer.0, pointer.1) {
                canvas.tint(button, 8, BLACK, 20);
            }
            speaker(
                canvas,
                Rect::new(button.x + 5, button.y + 6, 28, 24),
                level,
                muted,
                colour::text(),
            );
            let track = volume_track(w);
            canvas.round_fill(track, track.h / 2, colour::button_secondary());
            let filled = (track.w * i32::from(level) / 100).max(track.h);
            let ink = if muted {
                colour::muted()
            } else {
                colour::accent()
            };
            canvas.round_fill(
                Rect::new(track.x, track.y, filled, track.h),
                track.h / 2,
                ink,
            );
        } else {
            let m = cc_module(w, 4);
            canvas.text(
                m.x + 16,
                m.y + 16,
                "No sound device",
                Font::Body,
                colour::muted(),
                m,
            );
        }

        // The system's state: memory and where the network is.
        let mut line = self.status.clone();
        match &self.network {
            Some(n) if n.connected => {
                let by = match self.wifi.as_ref().map(|w| &w.state) {
                    Some(WifiState::Joined { name, .. }) => alloc::format!("Wi-Fi {name}"),
                    _ => String::from("Ethernet"),
                };
                line.push_str(&alloc::format!("  ·  {by} {}", n.address));
            }
            Some(_) => line.push_str("  ·  Not connected"),
            None => {}
        }
        canvas.text(
            cc.x + 18,
            cc.y + cc.h - 30,
            &line,
            Font::Body,
            colour::muted(),
            cc,
        );
    }

    /// Wave (ADR-0117): closed, one round glass button with the Oceans logo
    /// and a dot for each app with a window open; open, the Terminal, the
    /// kept apps and the windows rise over it along a crest, the name of
    /// the one under the pointer above it.
    fn draw_dock(&self, canvas: &mut Canvas, windows: &[WindowView<'_>]) {
        let (w, h) = (canvas.width, canvas.height);
        let shown = taskbar_windows(windows);
        let listed: Vec<DockWindow> = shown
            .iter()
            .map(|view| (view.frame.id, view.frame.app.clone()))
            .collect();
        let entries = self.dock_entries(&listed);
        let kept = self.kept();
        let pointer = self.pointer;

        let button = wave_button(w, h);
        let (cx, cy, radius) = (
            button.x + button.w / 2,
            button.y + button.h / 2,
            button.w / 2,
        );
        let lit = self.wave_open || self.start_open || button.contains(pointer.0, pointer.1);
        // Lit: a ring in the accent; else a hairline.
        if lit {
            canvas.circle(cx, cy, radius + 2, colour::accent());
        } else {
            canvas.circle(cx, cy, radius + 1, Rgb(0x8e_8e_93));
        }
        canvas.circle(cx, cy, radius, colour::glass());
        oceans_logo(canvas, button);
        // What is open: a dot for each app with a window (and the
        // Terminal), up to five, under the button.
        let running = shown
            .iter()
            .map(|view| view.frame.app.as_str())
            .fold(Vec::<&str>::new(), |mut apps, app| {
                if !apps.contains(&app) {
                    apps.push(app);
                }
                apps
            })
            .len()
            + usize::from(!self.terminal_hidden);
        let dots = running.min(5) as i32;
        for i in 0..dots {
            let x = cx - (dots - 1) * 4 + i * 8;
            canvas.circle(x, button.y + button.h + 6, 2, WHITE);
        }
        if !self.wave_open || self.start_open {
            return;
        }

        let count = 1 + entries.len();
        let mut label: Option<(Rect, String)> = None;
        let mut item = |canvas: &mut Canvas, index: usize, name: &str, open: bool| {
            let r = wave_item(w, h, index, count);
            canvas.shadow(r, 12, 60);
            if r.contains(pointer.0, pointer.1) && self.dialog.is_none() {
                label = Some((r, String::from(name)));
            }
            if open {
                // Open: a short bar under the icon, as on the button.
                canvas.round_fill(Rect::new(r.x + r.w / 2 - 6, r.y + r.h + 5, 12, 3), 1, WHITE);
            }
            r
        };
        let term = item(canvas, 0, "Terminal", !self.terminal_hidden);
        terminal_icon(canvas, term);
        // Not for this account (ADR-0115): shown, but dimmed.
        if !self.admin {
            canvas.tint(term, term.w / 4, colour::glass(), 150);
        }
        for (i, entry) in entries.iter().enumerate() {
            match *entry {
                // A kept app (ADR-0109): marked while one of its windows is
                // open.
                dock::Entry::Pinned(index) => {
                    let name = kept.get(index).map_or("?", |&(_, name)| name);
                    let open = shown.iter().any(|view| view.frame.app == name);
                    let r = item(canvas, i + 1, name, open);
                    app_icon(canvas, r, name);
                }
                dock::Entry::Window(id) => {
                    let Some(view) = shown.iter().find(|view| view.frame.id == id) else {
                        continue;
                    };
                    let r = item(canvas, i + 1, &view.frame.app, true);
                    app_icon(canvas, r, &view.frame.app);
                    if view.frame.minimized {
                        // Put away: the icon is a little faded.
                        canvas.tint(r, 12, colour::glass(), 90);
                    }
                }
            }
        }

        // The name of what the pointer is on, over its icon.
        if let Some((r, name)) = label {
            let width = canvas.measure(&name, Font::Body) + 20;
            let tip = Rect::new(r.x + (r.w - width) / 2, r.y - 34, width, 26);
            canvas.tint(tip, 8, Rgb(0x2c_2c_2e), 230);
            canvas.text(tip.x + 10, tip.y + 5, &name, Font::Body, WHITE, tip);
        }
    }

    /// Launchpad (ADR-0117): the apps in a grid across the screen, over a
    /// frosted wash of what is under it; the search at the top; Lock,
    /// Restart and Shut Down in the corner.
    fn draw_apps_panel(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        let panel = start_menu(w, h);
        canvas.fill(panel, colour::panel());
        // The search field (ADR-0108): what is typed goes in while the
        // panel is open.
        let field = search_field(w, h);
        canvas.round_fill(field, 10, colour::accent());
        let inner = inset(field, 2);
        canvas.round_fill(inner, 8, colour::surface());
        let clip = Rect::new(inner.x + 10, inner.y, inner.w - 20, inner.h);
        let end = if self.search.is_empty() {
            canvas.text(
                inner.x + 12,
                inner.y + 7,
                "Search apps and files",
                Font::Body,
                colour::muted(),
                clip,
            );
            inner.x + 12
        } else {
            canvas.text(
                inner.x + 12,
                inner.y + 7,
                &self.search,
                Font::Body,
                colour::text(),
                clip,
            )
        };
        canvas.fill(
            Rect::new(end.min(clip.x + clip.w - 2), inner.y + 7, 2, 18),
            colour::accent(),
        );
        if !self.search.is_empty() {
            return self.draw_found(canvas);
        }
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
        for (i, app) in self.apps.iter().take(max_tiles(w, h) - 1).enumerate() {
            let t = tile_frame(canvas, i + 1);
            let icon = icon_in(t);
            app_icon(canvas, icon, &app.name);
            if app.running {
                canvas.circle(icon.x + icon.w - 2, icon.y + 2, 5, colour::success());
            }
            tile_label(canvas, t, &app.name);
        }
        let footer_y = power_button(w, h, 0).y;
        let note = if self.apps.is_empty() {
            String::from("No apps installed yet: the Store, or `app install`")
        } else if self.apps.len() > max_tiles(w, h) - 1 {
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
        canvas.text(
            panel.x + 32,
            footer_y + 8,
            &note,
            Font::Body,
            colour::muted(),
            panel,
        );
        let mut buttons: Vec<(Rect, &str)> = Vec::new();
        // Lock (ADR-0112), with a password set.
        if self.password_set {
            buttons.push((lock_button(w, h, self.can_power), "Lock"));
        }
        if self.can_power {
            buttons.push((power_button(w, h, 0), "Restart"));
            buttons.push((power_button(w, h, 1), "Shut Down"));
        }
        for (button, label) in buttons {
            let hovered = button.contains(pointer.0, pointer.1) && self.dialog.is_none();
            let fill = if hovered {
                Rgb(0xd5_d5_db)
            } else {
                colour::button_secondary()
            };
            canvas.round_fill(button, 8, fill);
            let tx = button.x + (button.w - canvas.measure(label, Font::Body)) / 2;
            canvas.text(tx, button.y + 6, label, Font::Body, colour::text(), button);
        }
    }

    /// The search's results (ADR-0108): one row each, apps then files, the
    /// one Enter opens highlighted.
    fn draw_found(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        let panel = start_menu(w, h);
        let pointer = self.pointer;
        for (i, found) in self.found.iter().enumerate() {
            let row = found_row(w, h, i);
            if i == self.chosen {
                canvas.round_fill(row, 8, colour::chosen());
            } else if row.contains(pointer.0, pointer.1) && self.dialog.is_none() {
                canvas.tint(row, 8, BLACK, 14);
            }
            let icon = Rect::new(row.x + 8, row.y + 4, 24, 24);
            let (name, kind) = match found {
                Found::App(index) => {
                    let name = self.apps.get(*index).map_or("?", |a| a.name.as_str());
                    app_icon(canvas, icon, name);
                    (String::from(name), String::from("App"))
                }
                Found::File(path) => {
                    canvas.round_fill(icon, 5, WHITE);
                    canvas.tint(
                        Rect::new(icon.x, icon.y, icon.w, 6),
                        5,
                        colour::accent(),
                        120,
                    );
                    let name = oceans_search::file_name(path);
                    let folder = &path[..path.len() - name.len()];
                    let kind = if folder.is_empty() {
                        String::from("Home")
                    } else {
                        alloc::format!("Home/{}", folder.trim_end_matches('/'))
                    };
                    (String::from(name), kind)
                }
            };
            let kind_w = canvas.measure(&kind, Font::Body);
            let right = row.x + row.w - 12;
            canvas.text(
                right - kind_w,
                row.y + 7,
                &kind,
                Font::Body,
                colour::muted(),
                row,
            );
            let clip = Rect::new(row.x + 40, row.y, right - kind_w - 16 - (row.x + 40), row.h);
            canvas.text(
                row.x + 40,
                row.y + 7,
                &name,
                Font::Body,
                colour::text(),
                clip,
            );
        }
        let note = if self.found.is_empty() {
            alloc::format!("Nothing matches \"{}\"", self.search)
        } else {
            String::from("Enter opens; the arrows choose; Esc closes")
        };
        let footer_y = power_button(w, h, 0).y;
        canvas.text(
            panel.x + 32,
            footer_y + 8,
            &note,
            Font::Body,
            colour::muted(),
            panel,
        );
    }

    /// The lock screen (ADR-0110): the time, the user, and a field for the
    /// password, shown as dots.
    fn draw_lock_screen(&self, canvas: &mut Canvas) {
        let (w, h) = (canvas.width, canvas.height);
        canvas.dim(BLACK, 110);
        let centre = |canvas: &mut Canvas, y: i32, text: &str, font: Font, colour: Rgb| {
            let width = canvas.measure(text, font);
            canvas.text(
                (w - width) / 2,
                y,
                text,
                font,
                colour,
                Rect::new(0, 0, w, h),
            );
        };
        // The time large and the date (ADR-0119), as phones show them;
        // the short clock while the time is not set.
        if self.clock_time.is_empty() {
            centre(canvas, h / 2 - 190, &self.clock, Font::Title, WHITE);
        } else {
            centre(canvas, h / 2 - 262, &self.clock_time, Font::Display, WHITE);
            centre(canvas, h / 2 - 196, &self.clock_date, Font::Strong, WHITE);
        }
        if self.lock == Lock::Starting {
            return centre(canvas, h / 2, "Starting Oceans...", Font::Body, WHITE);
        }
        // One account: its picture and name. Several (ADR-0115): each
        // one's picture in a row, the chosen one ringed, its name below.
        // Round pictures, each account's colour by its place, and each name
        // under its own: two names with one initial stay apart.
        let count = self.users.len().max(1);
        let chosen = self.chosen_user.min(count - 1);
        for i in 0..count {
            let avatar = lock_avatar(w, h, i, count);
            let name = self
                .users
                .get(i)
                .map_or(self.user_name.as_str(), |(_, n)| n.as_str());
            let (cx, cy, radius) = (
                avatar.x + avatar.w / 2,
                avatar.y + avatar.h / 2,
                avatar.w / 2,
            );
            let picked = count == 1 || i == chosen;
            if count > 1 && picked {
                canvas.circle(cx, cy, radius + 4, WHITE);
            }
            user_picture(canvas, avatar, name, i, picked);
            let font = if picked { Font::Strong } else { Font::Body };
            let width = canvas.measure(name, font).min(avatar.w + 30);
            let label = Rect::new(cx - width / 2, avatar.y + avatar.h + 14, width, 24);
            let colour = if picked { WHITE } else { Rgb(0xc8_cc_d8) };
            canvas.text(label.x, label.y, name, font, colour, label);
        }
        let field = lock_field(w, h);
        canvas.round_fill(field, 12, colour::accent());
        let inner = inset(field, 2);
        canvas.round_fill(inner, 10, WHITE);
        if self.lock_input.is_empty() {
            canvas.text(
                inner.x + 14,
                inner.y + 10,
                "Password",
                Font::Body,
                colour::muted(),
                inner,
            );
        } else {
            // As many dots as fit left of the layout's name.
            let dots = self.lock_input.chars().count().min(18) as i32;
            for i in 0..dots {
                canvas.circle(inner.x + 18 + i * 14, inner.y + inner.h / 2, 4, INK);
            }
        }
        // The layout the password is typed in (ADR-0114), as the menu bar
        // shows it: the same keys type another password in the other one.
        let layout = self.layout.short_name();
        let width = canvas.measure(layout, Font::Strong);
        canvas.text(
            inner.x + inner.w - 14 - width,
            inner.y + 10,
            layout,
            Font::Strong,
            Rgb(0x6e_6e_73),
            inner,
        );
        let note = if !self.lock_note.is_empty() {
            self.lock_note.as_str()
        } else if count > 1 {
            "Choose with the arrows or a click; type the password, then Enter"
        } else {
            "Type your password, then Enter"
        };
        centre(canvas, field.y + field.h + 16, note, Font::Body, WHITE);
    }

    fn draw_dialog(&self, canvas: &mut Canvas, dialog: &Dialog) {
        let (w, h) = (canvas.width, canvas.height);
        let r = self::dialog(w, h);
        canvas.dim(BLACK, 100);
        canvas.shadow(r, 18, 120);
        canvas.round_fill(r, 14, colour::dialog_surface());
        let (x, mut y) = (r.x + 24, r.y + 20);
        canvas.text(x, y, dialog.title, Font::Title, colour::text(), r);
        y += 36;
        canvas.text(x, y, &dialog.app, Font::Body, colour::muted(), r);
        y += 26;
        canvas.text(x, y, dialog.ask, Font::Body, colour::text(), r);
        y += 22;
        canvas.text(
            x + 16,
            y,
            &dialog.description,
            Font::Strong,
            colour::text(),
            r,
        );
        if !dialog.note.is_empty() {
            y += 30;
            canvas.text(x, y, &dialog.note, Font::Body, colour::muted(), r);
        }
        for (button, label, primary) in [
            (deny(w, h), dialog.deny, false),
            (allow(w, h), dialog.allow, true),
        ] {
            let hovered = button.contains(self.pointer.0, self.pointer.1);
            let fill = match (primary, hovered) {
                (true, false) => colour::accent(),
                (true, true) => Rgb(0x52_93_f8),
                (false, false) => colour::button_secondary(),
                (false, true) => Rgb(0xd5_d5_db),
            };
            canvas.round_fill(button, 8, fill);
            let tx = button.x + (button.w - canvas.measure(label, Font::Strong)) / 2;
            let colour = if primary { WHITE } else { colour::text() };
            canvas.text(tx, button.y + 9, label, Font::Strong, colour, button);
        }
    }
}

fn inset(r: Rect, by: i32) -> Rect {
    Rect::new(r.x + by, r.y + by, r.w - 2 * by, r.h - 2 * by)
}

/// The icon of a Launchpad tile: 48 pixels, centred at its top.
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
        colour::text(),
        tile,
    );
}

/// An app's icon: a rounded square in its colour (chosen by its name, which
/// Wave and Launchpad both know), with its initial.
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

/// An account's picture at the lock screen (ADR-0115): a circle in the
/// colour of its place among the accounts, with its initial; dimmed when
/// another is chosen.
fn user_picture(canvas: &mut Canvas, r: Rect, name: &str, index: usize, chosen: bool) {
    let (cx, cy, radius) = (r.x + r.w / 2, r.y + r.h / 2, r.w / 2);
    let colour = ICON_COLOURS[index % ICON_COLOURS.len()];
    canvas.circle(cx, cy, radius, colour);
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .into_iter()
        .collect();
    let width = canvas.measure(&initial, Font::Title);
    canvas.text(
        cx - width / 2,
        cy - Font::Title.height() / 2 - 1,
        &initial,
        Font::Title,
        WHITE,
        r,
    );
    if !chosen {
        canvas.tint(r, r.w / 2, BLACK, 90);
    }
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

/// Where the level shows after a volume key (ADR-0101): above Wave's
/// button, in the middle.
pub fn volume_shown(width: i32, height: i32) -> Rect {
    Rect::new((width - 260) / 2, height - DOCK_RESERVE - 64, 260, 48)
}

/// The level over the desktop (ADR-0101): a speaker and a bar.
fn draw_volume_shown(canvas: &mut Canvas, (level, muted): (u8, bool)) {
    let r = volume_shown(canvas.width, canvas.height);
    canvas.shadow(r, 12, 70);
    canvas.round_fill(r, 14, colour::dialog_surface());
    speaker(
        canvas,
        Rect::new(r.x + 8, r.y + 12, 28, 24),
        level,
        muted,
        colour::text(),
    );
    let track = Rect::new(r.x + 48, r.y + 21, r.w - 64, 6);
    canvas.round_fill(track, 3, colour::button_secondary());
    if !muted {
        let filled = track.w * i32::from(level) / 100;
        canvas.round_fill(
            Rect::new(track.x, track.y, filled, track.h),
            3,
            colour::accent(),
        );
    }
}

/// The side of the top bar's icons (ADR-0136), as other systems draw them
/// in a bar of its height.
const BAR_ICON: i32 = 18;
/// What is off in a top bar icon (a weak signal's bands), over the shade.
const BAR_FAINT: Rgb = Rgb(0x8e_8e_93);
/// What is off in an icon on a light panel.
const PANEL_FAINT: Rgb = Rgb(0xb8_b8_be);

/// An icon of `oceans_draw::icon`, `side` pixels square, centred in `r`.
fn glyph(canvas: &mut Canvas, r: Rect, side: i32, draw: impl FnOnce(&mut Surface<'_>, Rect)) {
    let at = Rect::new(r.x + (r.w - side) / 2, r.y + (r.h - side) / 2, side, side);
    canvas.with_surface(|s| draw(s, at));
}

/// The same, centred on (`cx`, `cy`).
fn glyph_at(
    canvas: &mut Canvas,
    (cx, cy): (i32, i32),
    side: i32,
    draw: impl FnOnce(&mut Surface<'_>, Rect),
) {
    glyph(
        canvas,
        Rect::new(cx - side / 2, cy - side / 2, side, side),
        side,
        draw,
    );
}

/// A speaker centred in `r` (ADR-0100): a body and a cone, then up to three
/// waves for the level, or a cross when muted.
fn speaker(canvas: &mut Canvas, r: Rect, level: u8, muted: bool, ink: Rgb) {
    glyph(canvas, r, BAR_ICON, |s, r| {
        icon::speaker(s, r, level, muted, ink)
    });
}

/// How the machine is connected, as the top bar shows it (ADR-0136).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Joined to a Wi-Fi network, its signal's bars lit (1 to 3).
    Wifi(u8),
    /// Wi-Fi is on and joined to nothing, and no cable is connected.
    WifiOff,
    /// Through a cable; struck through when not connected.
    Ethernet(bool),
}

impl Link {
    fn draw(self, s: &mut Surface<'_>, r: Rect, ink: Rgb, faint: Rgb) {
        match self {
            Self::Wifi(bars) => icon::wifi(s, r, bars, ink, faint),
            Self::WifiOff => icon::wifi_off(s, r, ink, faint),
            Self::Ethernet(connected) => icon::ethernet(s, r, connected, ink),
        }
    }
}

impl Desktop {
    /// The connection to show: Wi-Fi when joined, else a cable that
    /// works, else Wi-Fi off when there is Wi-Fi, else a cable that does
    /// not.
    pub fn link(&self) -> Link {
        let connected = self.network.as_ref().is_some_and(|n| n.connected);
        match self.wifi.as_ref().map(|w| &w.state) {
            Some(WifiState::Joined { rssi, .. }) => Link::Wifi(icon::wifi_bars(*rssi)),
            _ if connected => Link::Ethernet(true),
            Some(WifiState::Off) | None => Link::Ethernet(false),
            Some(_) => Link::WifiOff,
        }
    }
}

/// A network's signal (ADR-0130): Wi-Fi's fan, as many bands lit as it is
/// strong.
fn signal_icon(canvas: &mut Canvas, cx: i32, cy: i32, rssi: i8, ink: Rgb) {
    glyph_at(canvas, (cx, cy), BAR_ICON, |s, r| {
        icon::wifi(s, r, icon::wifi_bars(rssi), ink, PANEL_FAINT)
    });
}

/// Back: a chevron pointing left, about 12 pixels.
fn chevron_left(canvas: &mut Canvas, cx: i32, cy: i32, ink: Rgb) {
    // The point at the left, the arms going right and away.
    for i in 0..7 {
        canvas.fill(Rect::new(cx - 3 + i, cy - i, 2, 2), ink);
        canvas.fill(Rect::new(cx - 3 + i, cy + i, 2, 2), ink);
    }
}

/// Dark (ADR-0118): a crescent, cut from a circle by one in `bg`.
fn moon_icon(canvas: &mut Canvas, cx: i32, cy: i32, ink: Rgb, bg: Rgb) {
    canvas.circle(cx, cy, 9, ink);
    canvas.circle(cx + 5, cy - 4, 8, bg);
}

/// Lock (ADR-0118): a padlock.
fn lock_icon(canvas: &mut Canvas, cx: i32, cy: i32, ink: Rgb, bg: Rgb) {
    canvas.circle(cx, cy - 4, 6, ink);
    canvas.circle(cx, cy - 4, 4, bg);
    canvas.round_fill(Rect::new(cx - 8, cy - 2, 17, 12), 3, ink);
    canvas.circle(cx, cy + 3, 2, bg);
}

/// Settings (ADR-0118): a gear.
fn gear_icon(canvas: &mut Canvas, cx: i32, cy: i32, ink: Rgb, bg: Rgb) {
    canvas.fill(Rect::new(cx - 10, cy - 2, 21, 5), ink);
    canvas.fill(Rect::new(cx - 2, cy - 10, 5, 21), ink);
    for (dx, dy) in [(-6, -6), (6, -6), (-6, 6), (6, 6)] {
        canvas.circle(cx + dx, cy + dy, 3, ink);
    }
    canvas.circle(cx, cy, 8, ink);
    canvas.circle(cx, cy, 3, bg);
}

/// Play: a triangle pointing right, about 14 pixels.
fn play_icon(canvas: &mut Canvas, cx: i32, cy: i32) {
    for row in -7i32..=7 {
        let len = 13 - row.abs() * 13 / 7;
        canvas.fill(Rect::new(cx - 5, cy + row, len, 1), colour::text());
    }
}

fn pause_icon(canvas: &mut Canvas, cx: i32, cy: i32) {
    canvas.fill(Rect::new(cx - 6, cy - 7, 4, 15), colour::text());
    canvas.fill(Rect::new(cx + 2, cy - 7, 4, 15), colour::text());
}

/// Next (`forward`) or Previous: a triangle and a bar.
fn skip_icon(canvas: &mut Canvas, cx: i32, cy: i32, forward: bool) {
    for row in -6i32..=6 {
        let len = 10 - row.abs() * 10 / 6;
        let x = if forward { cx - 6 } else { cx + 4 - len };
        canvas.fill(Rect::new(x, cy + row, len, 1), colour::text());
    }
    let bar = if forward { cx + 4 } else { cx - 7 };
    canvas.fill(Rect::new(bar, cy - 6, 3, 13), colour::text());
}

/// Home on the desktop (ADR-0120): a blue folder.
fn folder_icon(canvas: &mut Canvas, r: Rect) {
    let tab = Rect::new(r.x + 4, r.y + 8, r.w / 2 - 4, 10);
    canvas.round_fill(tab, 3, Rgb(0x2f_8f_e8));
    let body = Rect::new(r.x + 2, r.y + 13, r.w - 4, r.h - 18);
    canvas.round_fill(body, 6, Rgb(0x4a_a8_f8));
    canvas.tint(Rect::new(body.x, body.y, body.w, 6), 6, WHITE, 40);
}

/// The Oceans logo (ADR-0117): the wave in a circle, made from the logo
/// the project chose, 40 pixels, centred in `r` (Wave's button).
fn oceans_logo(canvas: &mut Canvas, r: Rect) {
    canvas.logo(r.x + (r.w - 40) / 2, r.y + (r.h - 40) / 2, 40);
}

/// A window's frame (the Terminal's too): a soft shadow, a rounded border
/// and the title bar's colour.
fn chrome(canvas: &mut Canvas, outer: Rect, focused: bool) {
    canvas.shadow(outer, 14, if focused { 120 } else { 60 });
    canvas.round_fill(outer, 10, colour::window_border());
    canvas.round_fill(
        inset(outer, 1),
        9,
        if focused {
            colour::focus_title()
        } else {
            colour::idle_title()
        },
    );
}

/// The title, centred in the bar (clear of the buttons, which start at
/// `right`, ADR-0117): the app's verified name, then its own title.
fn title_text(canvas: &mut Canvas, bar: Rect, app: &str, title: &str, right: i32) {
    let gap = if title.is_empty() { 0 } else { 8 };
    let width = canvas.measure(app, Font::Strong) + gap + canvas.measure(title, Font::Body);
    let left = bar.x + 12;
    let clip = Rect::new(left, bar.y, right - left, bar.h);
    let x = (bar.x + (bar.w - width) / 2).max(left);
    let end = canvas.text(x, bar.y + 5, app, Font::Strong, colour::text(), clip);
    if !title.is_empty() {
        canvas.text(
            end + gap,
            bar.y + 5,
            title,
            Font::Body,
            colour::muted(),
            clip,
        );
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
        canvas.circle(cx, cy, 6, colour.unwrap_or(colour::button_off()));
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
    // Full screen (ADR-0107): no frame, only the app's pixels.
    if !frame.full_screen() {
        draw_chrome(canvas, view, pointer);
    }
    draw_content(canvas, view);
}

/// The frame the system draws: border, title bar, title and buttons.
fn draw_chrome(canvas: &mut Canvas, view: &WindowView<'_>, pointer: (i32, i32)) {
    let frame = view.frame;
    chrome(canvas, frame.outer(), view.focused);
    title_text(
        canvas,
        frame.title_bar(),
        &frame.app,
        &frame.title,
        frame.zoom_button().x - 8,
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
}

/// The app's pixels, in the frame's content.
fn draw_content(canvas: &mut Canvas, view: &WindowView<'_>) {
    let frame = view.frame;
    let content = frame.content();
    match view.pixels {
        // The app's pixels are the screen's, at the scale (ADR-0136); of
        // another size (a resize or a new scale the app has not caught up
        // with), as much as fits, the rest in the windows' colour.
        Some((pixels, width, height)) if frame.presented => {
            canvas.blit_window(content, pixels, width, height, colour::idle_title());
        }
        _ => canvas.fill(content, colour::surface()),
    }
}
