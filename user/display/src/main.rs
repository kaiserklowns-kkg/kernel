//! The Oceans display service (ADR-0057): the on-device desktop.
//!
//! It takes the screen over from the kernel console (`DISPLAY_CLAIM`),
//! draws the desktop into a back buffer and presents only the pixels that
//! changed (the layout: ADR-0076, `desktop`):
//! - the taskbar: the Start button, the Terminal and the open windows, the
//!   system's state and the clock;
//! - the Start menu: the installed apps, from Oceans Core; a click starts
//!   one;
//! - the Terminal: the console's text (`DISPLAY_TEXT`), where the shell
//!   keeps working, in a window that can be minimized;
//! - **app windows** (ADR-0059): apps given `window` open windows through
//!   the window endpoint this service makes and registers with Core
//!   (`WINDOWS`); each app's end is badged by Core, and Core says whose a
//!   badge is (`WINDOW_OWNER`) before a window opens. The system draws the
//!   frame, the app the pixels inside;
//! - **the keyboard focus** (ADR-0059): the keyboard comes here
//!   (`DISPLAY_KEYBOARD`) and goes to the focused window, or back to the
//!   console (`console-input`) when the Terminal has the focus. A click
//!   or Ctrl+Tab moves the focus;
//! - **resizing** (ADR-0097): a window whose app said it can be resized
//!   follows its edges and corners, and the zoom button or a double click
//!   on its title bar maximizes and restores it;
//! - **the clipboard** (ADR-0095): text an app copies while it has the
//!   focus, pasted only where the user pastes (Ctrl+V in a window,
//!   Ctrl+Shift+V there and in the Terminal, a line at a time);
//! - **opening files** (ADR-0099): an app asks, after the user's key or
//!   click, to open a file of Home; it starts in the app that opens its
//!   kind (the system's own first) or the one the user chose;
//! - **the system volume** (ADR-0100): a speaker in the menu bar opens a
//!   panel with a slider and Mute; Core sets it and keeps it;
//! - **the volume keys** (ADR-0101): the desktop's whoever has the focus;
//!   a step of 5 or mute, the level shown over the desktop for a moment;
//! - **the media keys** (ADR-0102): to the player that asked for them
//!   (`MEDIA_KEYS`) or whose window the user looked at last;
//! - notifications;
//! - **permission dialogs**: when an app needs a decision, the desktop
//!   asks, in the system's words, and sends the answer to Core
//!   (`source::DIALOG`). Permission UI is system-rendered, never a web view
//!   (ADR-0056).
//!
//! The pointer comes from the input service (ADR-0042). If this service
//! ends, the kernel console takes the screen back.
//!
//! - **Restart and Shut Down** (ADR-0085): confirmed in a system dialog,
//!   then asked of init (`grant = power`), which stops the system.
//!
//! Grants: `log`, `display`, `console-input` (to hand keys to the
//! console), `use = core` (the user's agent, like the shell), `use =
//! input`, `sysinfo`, `power`.

#![no_std]
#![no_main]

extern crate alloc;

mod canvas;
mod desktop;
mod windows;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_core_proto::{
    Core, CoreError, Decision, Status, decision, field, op, parts, run_flags, source,
};
use oceans_input_proto::{Kind, Subscription};
use oceans_package::Permission;
use oceans_rt::{Buffer, Directory, Handle, Start, prot, rights};
use oceans_window::{Focus, KeyRoute, Manager, VolumeKey};

use canvas::{Canvas, Scale};
use desktop::{App, Desktop, Dialog, Hit, Question, Terminal, Toast, WindowView};
use windows::{Owner, Pixels};

oceans_rt::entry!(main);

const EXIT_BAD_START: i64 = 2;
const EXIT_NO_SCREEN: i64 = 3;

/// Notification bits.
const INPUT: u64 = 1 << 0;
const TICK: u64 = 1 << 1;
const KEYS: u64 = 1 << 2;
/// How often the clock, the console mirror and the app list are checked.
const TICK_MS: u64 = 100;
const APPS_EVERY_MS: u64 = 2000;
/// The wallpaper's photograph (ADR-0117), a module of the boot image.
const WALLPAPER: &str = "wallpaper-ocean.png";
/// How long the desktop runs before this boot counts as finished
/// (ADR-0122).
const BOOT_OK_MS: u64 = 60_000;
const TOAST_MS: u64 = 4000;
/// A second press within this long, and 4 pixels, is a double click.
const DOUBLE_CLICK_MS: u64 = 500;
/// How long the level shows after a volume key (ADR-0101).
const VOLUME_SHOWN_MS: u64 = 1500;

/// A media key's name (ADR-0102), for the log.
fn media_key_name(byte: u8) -> Option<&'static str> {
    use oceans_window::proto::{KEY_NEXT, KEY_PLAY_PAUSE, KEY_PREVIOUS, KEY_STOP};
    Some(match byte {
        KEY_PLAY_PAUSE => "Play/Pause",
        KEY_STOP => "Stop",
        KEY_PREVIOUS => "Previous",
        KEY_NEXT => "Next",
        _ => return None,
    })
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<200>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

struct Service {
    log: Handle,
    display: Handle,
    core: Option<Core>,
    sysinfo: Option<Handle>,
    desktop: Desktop,
    /// Permissions still to ask about for the app being started.
    pending: Vec<(u8, String)>,
    /// Bytes of a `DISPLAY_TEXT` answer: the header and every cell.
    text_capacity: usize,
    /// Where keys go when the Terminal has the focus.
    console: Option<Handle>,
    /// App windows (ADR-0059).
    windows: Manager,
    /// The apps with windows, by badge.
    owners: BTreeMap<u64, Owner>,
    /// Each window's pixel memory, by window.
    pixels: BTreeMap<u32, Pixels>,
    /// Asking init to switch off or restart (ADR-0085).
    power: Option<Handle>,
    /// A reader end of the network stack (ADR-0118), from Core.
    net_reader: Option<Handle>,
    /// Wi-Fi's files (ADR-0130): `/system/wifi`, granted as storage.
    wifi_dir: Option<oceans_fs_proto::Node>,
    wifi_refreshed_ms: u64,
    /// When the desktop started, and whether it told Core this boot
    /// finished (ADR-0122).
    started_ms: u64,
    boot_reported: bool,
    /// The last press of the main button: when and where (a double click
    /// maximizes, ADR-0097).
    last_press: Option<(u64, i32, i32)>,
    /// What the app being asked about is started with once the user has
    /// answered (a file to open, ADR-0099).
    pending_args: String,
    /// The sound panel's slider is held (ADR-0100).
    volume_drag: bool,
    /// When the last key or pointer event came (ms since boot), and how
    /// long without one before the screen locks (ADR-0112; minutes, 0:
    /// never), as Core keeps it.
    last_input_ms: u64,
    lock_after: u8,
    /// The interface's scale (ADR-0136): the one drawn at, the user's
    /// choice as Core keeps it (0: automatic), and the screen's size in
    /// millimetres and pixels, which the automatic one follows.
    scale: Scale,
    scale_chosen: u16,
    /// The time zone (ADR-0143): minutes ahead of UTC, from Core.
    time_zone: i16,
    screen_mm: Option<(u16, u16)>,
    screen_pixels: (i32, i32),
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(display)) = (
        directory.find("log", "log"),
        directory.find("display", "display"),
    ) else {
        return EXIT_BAD_START;
    };
    let info = match oceans_rt::display_info(display) {
        Ok(info) if info.bpp == 32 && info.width >= 900 && info.height >= 420 => info,
        Ok(info) => {
            say(
                log,
                format_args!(
                    "display: {}x{}x{} is not supported; staying with the console",
                    info.width, info.height, info.bpp
                ),
            );
            return EXIT_NO_SCREEN;
        }
        Err(error) => {
            say(log, format_args!("display: no screen ({error:?})"));
            return EXIT_NO_SCREEN;
        }
    };
    let (memory, _) = match oceans_rt::display_claim(display) {
        Ok(claimed) => claimed,
        Err(error) => {
            say(
                log,
                format_args!("display: cannot take the screen over: {error:?}"),
            );
            return EXIT_NO_SCREEN;
        }
    };
    let Ok(screen) = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE) else {
        say(log, format_args!("display: cannot map the framebuffer"));
        return EXIT_NO_SCREEN;
    };
    let _ = oceans_rt::close(memory);
    let mut canvas = Canvas::new(
        screen,
        info.width,
        info.height,
        info.pitch,
        (info.red_shift, info.green_shift, info.blue_shift),
    );
    // Drawn once (ADR-0078); each frame starts from a copy. The photograph
    // is a module of the boot image, mapped while it is decoded (kept out
    // of the program: the kernel loads programs of at most 4 MiB).
    let photo = directory.find("module", WALLPAPER).and_then(|module| {
        let size = oceans_rt::memory_size(module).ok()? as usize;
        let base = oceans_rt::memory_map(module, 0, prot::READ).ok()?;
        Some((base, size))
    });
    // SAFETY: `base` maps `size` bytes of the module, read-only, until it
    // is unmapped below, after the last use of the slice.
    let bytes = photo.map(|(base, size)| unsafe { core::slice::from_raw_parts(base, size) });
    canvas.set_wallpaper(desktop::wallpaper(
        canvas.device_width,
        canvas.device_height,
        bytes,
    ));
    if let Some((base, _)) = photo {
        let _ = oceans_rt::memory_unmap(base);
    }
    if photo.is_none() {
        say(
            log,
            format_args!("display: no {WALLPAPER}: the drawn wallpaper"),
        );
    }

    let Ok(notification) = oceans_rt::notification_create() else {
        return EXIT_BAD_START;
    };
    let subscription = directory
        .find("use", "input")
        .and_then(|input| Subscription::new(input, notification, INPUT).ok());
    if subscription.is_none() {
        say(
            log,
            format_args!("display: no pointer input; the desktop is view-only"),
        );
    }
    // The window endpoint (ADR-0059): its notification is the one the
    // loop waits on, so calls, input and the clock arrive in one place.
    // Our own client end stays open: without any, receiving would fail.
    let Ok((server, _own_end)) = oceans_rt::endpoint_create() else {
        return EXIT_BAD_START;
    };
    if oceans_rt::endpoint_bind(server, notification).is_err() {
        return EXIT_BAD_START;
    }
    let console = directory.find("console-input", "console-input");
    let area = desktop::area(canvas.width, canvas.height);
    let screen_pixels = (canvas.width * canvas.height) as usize;
    let mut service = Service {
        log,
        display,
        core: directory.find("use", "core").map(Core),
        sysinfo: directory.find("sysinfo", "sysinfo"),
        desktop: Desktop {
            pointer: (canvas.width / 2, canvas.height / 2),
            pointer_device: (canvas.device_width / 2, canvas.device_height / 2),
            terminal_focused: true,
            can_power: directory.find("power", "power").is_some(),
            // Until Core says otherwise (without Core, no one else).
            admin: true,
            ..Desktop::default()
        },
        pending: Vec::new(),
        last_press: None,
        pending_args: String::new(),
        volume_drag: false,
        last_input_ms: oceans_rt::clock_ms(),
        lock_after: 0,
        text_capacity: 16 + info.cols as usize * info.rows as usize,
        console,
        windows: Manager::new(area, 2 * screen_pixels),
        owners: BTreeMap::new(),
        pixels: BTreeMap::new(),
        power: directory.find("power", "power"),
        net_reader: None,
        wifi_dir: directory.find("use", "storage").map(oceans_fs_proto::Node),
        wifi_refreshed_ms: 0,
        started_ms: oceans_rt::clock_ms(),
        boot_reported: false,
        scale: Scale::ONE,
        scale_chosen: 0,
        time_zone: 0,
        screen_mm: (info.mm_width > 0 && info.mm_height > 0)
            .then_some((info.mm_width, info.mm_height)),
        screen_pixels: (canvas.device_width, canvas.device_height),
    };
    // Full screen covers all of it (ADR-0107).
    service
        .windows
        .set_screen(oceans_window::Rect::new(0, 0, canvas.width, canvas.height));
    service.register_windows(server);
    // The keyboard comes here only if it can be handed on to the console.
    match console.map(|_| oceans_rt::display_keyboard(display, notification, KEYS)) {
        Some(Ok(())) => {}
        Some(Err(error)) => say(
            log,
            format_args!("display: cannot take the keyboard: {error:?}"),
        ),
        None => say(
            log,
            format_args!("display: no console-input; the keyboard stays with the console"),
        ),
    }
    service.refresh_apps();
    // The lock screen (ADR-0110): open at once without Core (no password
    // can be set), else when Core says whether there is one.
    if service.core.is_none() {
        service.desktop.lock = desktop::Lock::Open;
    }
    service.refresh_user();
    service.refresh_appearance();
    // The scale (ADR-0136) before the first frame: drawn at it from the
    // start.
    service.refresh_scale();
    service.refresh_time_zone();
    service.apply_scale(&mut canvas);
    service.refresh_layout();
    service.refresh_volume();
    service.refresh_network();
    service.show_mode();
    service.refresh_clock();
    service.refresh_terminal();
    service.draw(&mut canvas);
    say(
        log,
        format_args!(
            "display: {}x{} framebuffer taken over; desktop ready with {} apps",
            info.width,
            info.height,
            service.desktop.apps.len()
        ),
    );

    let mut last_apps = oceans_rt::clock_ms();
    let _ = oceans_rt::timer_set(notification, TICK, TICK_MS);
    // The largest message the kernel carries inline.
    let mut data = [0u8; 256];
    let mut handles = [Handle(0); 4];
    loop {
        let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
            Ok(got) => got,
            Err(error) => {
                say(log, format_args!("display: receive failed: {error:?}"));
                return EXIT_BAD_START;
            }
        };
        let mut dirty = false;
        if got.closed {
            // An app's window end is gone (it exited): its windows go.
            dirty = service.forget_owner(got.badge);
        } else if got.signals == 0 {
            dirty = service.window_request(&got, &data, &handles[..got.handles_len]);
        }
        let bits = got.signals;
        if bits & INPUT != 0
            && let Some(subscription) = &subscription
        {
            while let Ok(batch) = subscription.read() {
                if batch.is_empty() {
                    break;
                }
                for event in batch.events() {
                    dirty |= service.pointer(event.kind, event.time_ms, &canvas);
                }
            }
        }
        if bits & KEYS != 0 {
            dirty |= service.keys();
            // The PS/2 pointer (a touchpad, ADR-0132) comes with the keys.
            dirty |= service.ps2_pointer(&canvas);
        }
        if bits & TICK != 0 {
            let _ = oceans_rt::timer_set(notification, TICK, TICK_MS);
            let now = oceans_rt::clock_ms();
            dirty |= service.refresh_clock();
            if service.desktop.lock == desktop::Lock::Starting {
                dirty |= service.refresh_user();
            }
            dirty |= service.refresh_terminal();
            // The Wi-Fi panel open (ADR-0130): what the driver does, often.
            if service.desktop.wifi_open && now.saturating_sub(service.wifi_refreshed_ms) >= 500 {
                dirty |= service.refresh_wifi();
            }
            // A while without input (ADR-0112): the screen locks.
            if service.desktop.lock == desktop::Lock::Open
                && oceans_account::lock_due(
                    service.desktop.password_set,
                    service.lock_after,
                    service.last_input_ms,
                    now,
                )
            {
                service.lock_idle();
                dirty = true;
            }
            // A minute up (ADR-0122): this boot finished.
            if !service.boot_reported && now.saturating_sub(service.started_ms) >= BOOT_OK_MS {
                service.boot_reported = true;
                if let Some(core) = service.core {
                    let mut reply = [0u8; 8];
                    let _ = core.call(op::BOOT_OK, &[], &[], &mut reply);
                }
            }
            if now.saturating_sub(last_apps) >= APPS_EVERY_MS {
                last_apps = now;
                dirty |= service.refresh_apps();
                dirty |= service.refresh_user();
                dirty |= service.refresh_appearance();
                if service.refresh_time_zone() {
                    dirty |= service.refresh_clock();
                }
                if service.refresh_scale() {
                    service.apply_scale(&mut canvas);
                    dirty = true;
                }
                dirty |= service.refresh_volume();
                dirty |= service.refresh_network();
                dirty |= service.refresh_wifi();
                dirty |= service.refresh_pending();
            }
            if service
                .desktop
                .toast
                .as_ref()
                .is_some_and(|t| t.until_ms <= now)
            {
                service.desktop.toast = None;
                dirty = true;
            }
            if service.desktop.volume_shown_until != 0 && service.desktop.volume_shown_until <= now
            {
                service.desktop.volume_shown_until = 0;
                dirty = true;
            }
        }
        service.signal_owners();
        if dirty {
            service.draw(&mut canvas);
        }
    }
}

impl Service {
    fn toast(&mut self, text: String, error: bool) {
        self.desktop.toast = Some(Toast {
            text,
            error,
            until_ms: oceans_rt::clock_ms() + TOAST_MS,
        });
    }

    /// Draws the desktop and the app windows, and shows what changed.
    fn draw(&mut self, canvas: &mut Canvas) {
        // What plays (ADR-0103), for the sound panel.
        self.desktop.now_playing = self
            .windows
            .now_playing()
            .map(|n| (String::from(n.app), n.state, String::from(n.title)));
        let focus = self.windows.focus();
        self.desktop.terminal_focused = focus == Focus::Terminal;
        // Arranging windows (ADR-0107): where a dragged window would go,
        // and whether one covers the screen.
        self.desktop.snap = self.windows.snap_preview();
        self.desktop.full_screen = self.windows.full_screen().is_some();
        let views: Vec<WindowView<'_>> = self
            .windows
            .frames()
            .iter()
            .map(|frame| WindowView {
                frame,
                app_id: self.owners.get(&frame.owner).map_or("", |o| o.id.as_str()),
                pixels: self
                    .pixels
                    .get(&frame.id)
                    .map(|p| (p.pixels(), p.width, p.height)),
                focused: focus == Focus::Window(frame.id),
            })
            .collect();
        self.desktop.draw(canvas, oceans_rt::clock_ms(), &views);
        canvas.present();
    }

    /// The PS/2 pointing device's events (ABI 22, ADR-0132), queued by the
    /// kernel with the keys: handled as a USB pointer's are.
    fn ps2_pointer(&mut self, canvas: &Canvas) -> bool {
        const EVENT: usize = oceans_input_proto::EVENT_SIZE;
        let mut dirty = false;
        let mut buffer = [0u8; 16 * EVENT];
        loop {
            let count = match oceans_rt::display_pointer(self.display, &mut buffer) {
                Ok(0) | Err(_) => return dirty,
                Ok(count) => count,
            };
            for record in buffer.as_chunks::<EVENT>().0.iter().take(count) {
                if let Some(event) = oceans_input_proto::Event::decode(record) {
                    dirty |= self.pointer(event.kind, event.time_ms, canvas);
                }
            }
        }
    }

    /// A pointer event; `true` if the screen must change.
    fn pointer(&mut self, kind: Kind, time_ms: u64, canvas: &Canvas) -> bool {
        self.last_input_ms = oceans_rt::clock_ms();
        let (w, h) = (canvas.width, canvas.height);
        // The pointer moves over the screen's own pixels (ADR-0136), so a
        // larger scale does not make it coarser or faster; what it points
        // at is the logical pixel there.
        let (dw, dh) = (canvas.device_width, canvas.device_height);
        let (x, y) = self.desktop.pointer_device;
        let moved_to = match kind {
            Kind::Motion { dx, dy } => Some(((x + dx).clamp(0, dw - 1), (y + dy).clamp(0, dh - 1))),
            Kind::Absolute { x, y, x_max, y_max } if x_max > 0 && y_max > 0 => {
                let at = |v: u32, max: u32, size: i32| {
                    (u64::from(v) * (size as u64 - 1) / u64::from(max)) as i32
                };
                Some((at(x, x_max, dw), at(y, y_max, dh)))
            }
            _ => None,
        };
        if let Some((x, y)) = moved_to {
            let scale = canvas.scale();
            self.desktop.pointer_device = (x, y);
            self.desktop.pointer = (
                scale.logical(x).clamp(0, w - 1),
                scale.logical(y).clamp(0, h - 1),
            );
        }
        match kind {
            Kind::Motion { .. } => {}
            Kind::Absolute { .. } if moved_to.is_some() => {}
            // Locked (ADR-0110): clicks reach nothing but the accounts'
            // pictures, which choose one (ADR-0115).
            Kind::Button {
                button: 1,
                pressed: true,
            } if self.desktop.lock == desktop::Lock::Locked && self.desktop.users.len() > 1 => {
                let (x, y) = self.desktop.pointer;
                let count = self.desktop.users.len();
                let Some(index) =
                    (0..count).find(|&i| desktop::lock_avatar(w, h, i, count).contains(x, y))
                else {
                    return false;
                };
                if index != self.desktop.chosen_user {
                    self.choose_user(index);
                }
            }
            Kind::Button { .. } if self.desktop.lock != desktop::Lock::Open => return false,
            Kind::Button { button, pressed } => {
                let (x, y) = self.desktop.pointer;
                // The slider let go (ADR-0100): the level it shows is set.
                if button == 1 && !pressed && self.volume_drag {
                    self.volume_drag = false;
                    if let Some((level, _)) = self.desktop.volume {
                        self.set_volume(level, false);
                    }
                    return true;
                }
                // A permission dialog is modal: windows get nothing. The
                // taskbar, an open Start menu and the sound panel lie over
                // the windows, but a release always reaches them (it ends a
                // drag).
                // Full screen (ADR-0107), the menu bar and the dock give way.
                let bars = self.windows.full_screen().is_none();
                let wave = self.desktop.wave_open && {
                    let count = 1 + self.desktop.dock_entries(&self.dock_windows()).len();
                    desktop::wave_zone(w, h, count).contains(x, y)
                };
                let over = self.desktop.start_open
                    || (bars && y < desktop::MENU_HEIGHT)
                    || (bars && y >= h - desktop::DOCK_RESERVE)
                    || (bars && wave)
                    || (self.desktop.volume_open && desktop::control_center(w).contains(x, y))
                    || (self.desktop.notices_open
                        && desktop::notice_panel(w, self.desktop.notices.len()).contains(x, y))
                    || (self.desktop.user_menu_open
                        && desktop::user_menu(w, self.desktop.user_items().len()).contains(x, y));
                // A second press of the main button soon after the first,
                // where it was: a double click (ADR-0097). Timed when the
                // input service read them, not when they are handled here
                // (a slow frame between them must not part them).
                let now = time_ms;
                let double = button == 1
                    && pressed
                    && self.last_press.is_some_and(|(then, px, py)| {
                        now.saturating_sub(then) <= DOUBLE_CLICK_MS
                            && (px - x).abs() <= 4
                            && (py - y).abs() <= 4
                    });
                if button == 1 && pressed {
                    self.last_press = (!double).then_some((now, x, y));
                }
                // A press away from Control Center closes it (ADR-0118).
                if pressed
                    && self.desktop.volume_open
                    && !desktop::control_center(w).contains(x, y)
                    && ![
                        desktop::ICON_CONTROL,
                        desktop::ICON_NETWORK,
                        desktop::ICON_SPEAKER,
                    ]
                    .iter()
                    .any(|&i| desktop::status_icon(w, i).contains(x, y))
                {
                    self.desktop.volume_open = false;
                }
                // And from the user's menu (ADR-0121).
                let items = self.desktop.user_items().len();
                if pressed
                    && self.desktop.user_menu_open
                    && !desktop::user_menu(w, items).contains(x, y)
                    && !desktop::status_icon(w, desktop::ICON_USER).contains(x, y)
                {
                    self.desktop.user_menu_open = false;
                }
                // And from the notifications' panel (ADR-0119).
                if pressed
                    && self.desktop.notices_open
                    && !desktop::notice_panel(w, self.desktop.notices.len()).contains(x, y)
                    && !desktop::bell_button(w).contains(x, y)
                {
                    self.desktop.notices_open = false;
                }
                if self.desktop.dialog.is_none()
                    && (!pressed || !over)
                    && self.windows.button(button, pressed, x, y)
                {
                    if double {
                        self.windows.double_click(x, y);
                    }
                    return true;
                }
                // The second button keeps apps in the dock (ADR-0109).
                if button == 2 && pressed && self.desktop.dialog.is_none() {
                    return self.right_click(x, y, w, h);
                }
                return button == 1 && pressed && self.click(x, y, w, h, double);
            }
            // The wheel (ADR-0138): to the window under the pointer, or
            // the Terminal's rows back; not while something lies over the
            // windows.
            Kind::Wheel {
                vertical,
                horizontal,
            } => {
                let (x, y) = self.desktop.pointer;
                let covered = self.desktop.lock != desktop::Lock::Open
                    || self.desktop.dialog.is_some()
                    || self.desktop.start_open
                    || self.desktop.volume_open
                    || self.desktop.notices_open
                    || self.desktop.user_menu_open;
                if covered || self.windows.wheel(x, y, -vertical, horizontal) {
                    return false;
                }
                return !self.desktop.terminal_hidden
                    && desktop::terminal(w, h).contains(x, y)
                    && self.desktop.scroll_terminal(vertical);
            }
            _ => return false,
        }
        // Dragging the slider: the level follows, set when let go.
        if self.volume_drag
            && let Some((_, muted)) = self.desktop.volume
        {
            let level = desktop::volume_at(w, self.desktop.pointer.0);
            self.desktop.volume = Some((level, muted));
            return true;
        }
        // Wave opens at its button and closes once the pointer leaves it
        // (ADR-0117).
        let windows = self.dock_windows();
        self.desktop.track_wave(w, h, &windows);
        if self.desktop.dialog.is_none() && self.desktop.lock == desktop::Lock::Open {
            let (x, y) = self.desktop.pointer;
            self.windows.pointer_moved(x, y);
        }
        true
    }

    /// The system volume from Core (ADR-0100); `true` if it changed.
    fn refresh_volume(&mut self) -> bool {
        if self.volume_drag {
            return false;
        }
        let volume = self.core.and_then(|core| {
            let mut reply = [0u8; 8];
            match core.call(op::VOLUME, &[], &[], &mut reply) {
                Ok(got) if got.len == 2 => Some((reply[0], reply[1] != 0)),
                _ => None,
            }
        });
        let changed = volume != self.desktop.volume;
        self.desktop.volume = volume;
        changed
    }

    /// A volume key (ADR-0101): the level moves by a step, or mute turns
    /// on or off, and the new level shows over the desktop for a moment.
    fn volume_key(&mut self, key: VolumeKey) {
        if self.desktop.volume.is_none() {
            self.refresh_volume();
        }
        let Some(now) = self.desktop.volume else {
            return;
        };
        let (level, muted) = key.apply(now);
        self.set_volume(level, muted);
        self.desktop.volume_shown_until = oceans_rt::clock_ms() + VOLUME_SHOWN_MS;
    }

    /// Sets the system volume through Core, which keeps it (ADR-0100).
    fn set_volume(&mut self, level: u8, muted: bool) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        match core.call(op::SET_VOLUME, &[level, u8::from(muted)], &[], &mut reply) {
            Ok(got) if got.len == 2 => {
                self.desktop.volume = Some((reply[0], reply[1] != 0));
            }
            Ok(_) => {}
            Err((error, _)) => {
                self.toast(alloc::format!("The volume: {}", error.message()), true);
                self.refresh_volume();
            }
        }
    }

    /// Keys from the keyboard (`DISPLAY_KEYS`): to the focused window, or
    /// to the console for the Terminal. While a permission dialog asks,
    /// they go nowhere: typing must not land anywhere unseen. The volume
    /// keys (ADR-0101) are the desktop's, always.
    fn keys(&mut self) -> bool {
        let mut keys = [0u8; 256];
        let mut dirty = false;
        loop {
            let count = match oceans_rt::display_keys(self.display, &mut keys) {
                Ok(0) | Err(_) => return dirty,
                Ok(count) => count,
            };
            // Input: the time to lock after starts again (ADR-0112).
            self.last_input_ms = oceans_rt::clock_ms();
            let mut terminal = Vec::new();
            for &byte in &keys[..count] {
                // Super+Space (ADR-0114): the next keyboard layout, locked
                // or not.
                if byte == oceans_window::proto::KEY_LAYOUT {
                    self.next_layout();
                    dirty = true;
                    continue;
                }
                // Locked (ADR-0110): the keys are the lock screen's, but for
                // the volume keys.
                if self.desktop.lock != desktop::Lock::Open {
                    if let Some(key) = VolumeKey::of(byte) {
                        self.volume_key(key);
                    } else {
                        self.lock_key(byte);
                    }
                    dirty = true;
                    continue;
                }
                if byte == oceans_window::proto::KEY_LOCK {
                    self.lock_screen();
                    dirty = true;
                    continue;
                }
                // Super alone (ADR-0108): the launcher's search, the
                // desktop's whoever has the focus.
                if byte == oceans_window::proto::KEY_SEARCH {
                    if self.desktop.dialog.is_none() {
                        self.toggle_panel();
                        dirty = true;
                    }
                    continue;
                }
                if self.desktop.dialog.is_some() && VolumeKey::of(byte).is_none() {
                    continue;
                }
                // A Wi-Fi password being typed (ADR-0130): the keys are its.
                if self.desktop.volume_open
                    && self.desktop.wifi_open
                    && self.desktop.wifi_entry.is_some()
                    && VolumeKey::of(byte).is_none()
                    && media_key_name(byte).is_none()
                {
                    dirty |= self.wifi_key(byte);
                    continue;
                }
                // Launchpad open: typing searches, and no window or
                // the Terminal gets the keys (the volume and media keys
                // still work).
                if self.desktop.start_open
                    && VolumeKey::of(byte).is_none()
                    && media_key_name(byte).is_none()
                {
                    dirty |= self.search_key(byte);
                    continue;
                }
                // A letter of the layout in use (ADR-0114), to the focused
                // window as a character; the Terminal gets the key.
                if let Some(c) = self.desktop.layout.character(byte)
                    && self.windows.text(c).is_some()
                {
                    continue;
                }
                match self.windows.key(byte) {
                    KeyRoute::Terminal(byte) if self.desktop.admin => {
                        // Typing shows the cursor's rows again (ADR-0138).
                        self.desktop.terminal_back = 0;
                        terminal.push(byte);
                    }
                    KeyRoute::Terminal(_) => {}
                    KeyRoute::Window(owner) => {
                        // A media key (ADR-0102), to the player that asked.
                        if let Some(name) = media_key_name(byte) {
                            let app = self.owners.get(&owner).map_or("?", |o| o.app.as_str());
                            say(self.log, format_args!("display: media key {name} to {app}"));
                        }
                    }
                    KeyRoute::Consumed => dirty = true,
                    KeyRoute::Volume(key) => {
                        self.volume_key(key);
                        dirty = true;
                    }
                    // Ctrl+Shift+V (ADR-0095): one line, typed as if by
                    // the user, never Enter.
                    KeyRoute::TerminalPaste => {
                        if let Some((line, cut)) = self.windows.terminal_paste() {
                            terminal.extend_from_slice(line.as_bytes());
                            say(
                                self.log,
                                format_args!("display: clipboard: pasted into the Terminal"),
                            );
                            if cut {
                                self.toast(
                                    String::from("Only the clipboard's first line was pasted."),
                                    false,
                                );
                                dirty = true;
                            }
                        }
                    }
                }
            }
            if let Some(console) = self.console
                && !terminal.is_empty()
            {
                let _ = oceans_rt::console_input(console, &terminal);
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, width: i32, height: i32, double: bool) -> bool {
        let windows = self.dock_windows();
        let hit = self.desktop.hit(x, y, width, height, &windows);
        match hit {
            Hit::Layout => {
                self.next_layout();
                // Control Center's tile (ADR-0118), which stays open.
                if self.desktop.volume_open {
                    let name = self.desktop.layout.name();
                    say(
                        self.log,
                        format_args!("desktop: keyboard {name}, from Control Center"),
                    );
                }
                true
            }
            // The user's menu (ADR-0121).
            Hit::User => {
                self.desktop.user_menu_open = !self.desktop.user_menu_open;
                self.desktop.volume_open = false;
                self.desktop.notices_open = false;
                self.desktop.start_open = false;
                true
            }
            Hit::UserItem(action) => {
                self.desktop.user_menu_open = false;
                match action {
                    desktop::UserAction::Lock => self.lock_screen(),
                    // The lock screen chooses among the accounts.
                    desktop::UserAction::Switch => {
                        say(self.log, format_args!("desktop: switching user"));
                        self.lock_screen();
                    }
                    desktop::UserAction::Settings => self.launch("app.oceans.settings", ""),
                }
                true
            }
            // The bell (ADR-0119): the notifications, looked at.
            Hit::Clock => {
                self.desktop.notices_open = !self.desktop.notices_open;
                self.desktop.volume_open = false;
                self.desktop.user_menu_open = false;
                self.desktop.start_open = false;
                if self.desktop.notices_open {
                    self.desktop.unread = 0;
                    let count = self.desktop.notices.len();
                    say(
                        self.log,
                        format_args!(
                            "desktop: notifications shown: {count} kept, the newest from {}",
                            self.desktop
                                .notices
                                .first()
                                .map_or("no app", |n| n.app.as_str())
                        ),
                    );
                }
                true
            }
            Hit::ClearNotices => {
                let count = self.desktop.notices.len();
                self.desktop.notices.clear();
                say(
                    self.log,
                    format_args!("desktop: notifications cleared ({count})"),
                );
                true
            }
            Hit::NoticePanel => false,
            Hit::Volume => {
                self.desktop.volume_open = !self.desktop.volume_open;
                self.close_wifi();
                self.desktop.notices_open = false;
                self.desktop.user_menu_open = false;
                self.desktop.start_open = false;
                self.refresh_volume();
                true
            }
            Hit::VolumeLevel(level) => {
                // Shown at once, set when the button comes up.
                self.desktop.volume = Some((level, false));
                self.volume_drag = true;
                true
            }
            Hit::VolumeMute => {
                if let Some((level, muted)) = self.desktop.volume {
                    self.set_volume(level, !muted);
                }
                true
            }
            // What plays (ADR-0103): its buttons press the media keys.
            Hit::Media(byte) => {
                if let oceans_window::KeyRoute::Window(owner) = self.windows.media_key(byte) {
                    let app = self.owners.get(&owner).map_or("?", |o| o.app.as_str());
                    let name = media_key_name(byte).unwrap_or("?");
                    say(
                        self.log,
                        format_args!("desktop: {name} for {app}, from Control Center"),
                    );
                }
                true
            }
            Hit::VolumePanel => false,
            // Control Center's toggles (ADR-0118).
            Hit::Toggle(which) => {
                // Dark is seen at once in the open panel; the others leave
                // it.
                self.desktop.volume_open = which == 1;
                match which {
                    // The network with Wi-Fi: its panel (ADR-0130), a scan
                    // asked for.
                    0 if self.desktop.wifi.is_some() => {
                        self.desktop.volume_open = true;
                        self.desktop.wifi_open = true;
                        self.desktop.wifi_entry = None;
                        self.wifi_scan();
                        self.refresh_wifi();
                    }
                    // The network and Settings: Settings, which shows them.
                    0 | 3 => self.launch("app.oceans.settings", ""),
                    1 => {
                        let (dark, accent) = oceans_draw::theme::bytes();
                        self.set_appearance(1 - dark.min(1), accent);
                    }
                    _ => self.lock_screen(),
                }
                true
            }
            // The Wi-Fi panel (ADR-0130).
            Hit::WifiBack => {
                self.close_wifi();
                true
            }
            Hit::WifiScan => {
                self.wifi_scan();
                true
            }
            Hit::WifiNetwork(index) => {
                self.wifi_choose(index);
                true
            }
            Hit::WifiField => false,
            Hit::WifiJoin => {
                self.wifi_key(b'\r');
                true
            }
            Hit::WifiCancel => {
                self.desktop.wifi_entry = None;
                true
            }
            Hit::Accent => {
                let (dark, accent) = oceans_draw::theme::bytes();
                let next = (accent + 1) % oceans_core_proto::ACCENT_COUNT;
                self.set_appearance(dark, next);
                true
            }
            Hit::Start => {
                self.toggle_panel();
                true
            }
            Hit::Found(index) => {
                self.open_found(index);
                true
            }
            Hit::Outside => {
                self.desktop.start_open = false;
                true
            }
            Hit::StartApp(index) => {
                self.desktop.start_open = false;
                let id = self.desktop.apps[index].id.clone();
                self.launch(&id, "");
                true
            }
            // Lock (ADR-0112), as Super+L.
            Hit::Lock => {
                self.lock_screen();
                true
            }
            Hit::Restart | Hit::ShutDown => {
                self.desktop.start_open = false;
                let off = hit == Hit::ShutDown;
                self.desktop.dialog = Some(Dialog {
                    question: Question::Power(if off {
                        oceans_rt::power::OFF
                    } else {
                        oceans_rt::power::RESTART
                    }),
                    title: if off {
                        "Shut down Oceans?"
                    } else {
                        "Restart Oceans?"
                    },
                    app: String::from("Every app and service stops first."),
                    ask: "Files are saved to disk before the machine",
                    description: String::from(if off { "switches off." } else { "restarts." }),
                    note: String::new(),
                    deny: "Cancel",
                    allow: if off { "Shut Down" } else { "Restart" },
                });
                true
            }
            Hit::StartTerminal => {
                self.desktop.start_open = false;
                self.show_terminal();
                true
            }
            Hit::TaskbarTerminal => {
                // Like other taskbars: it shows the Terminal, or hides it if
                // it is in front with the keyboard.
                if !self.desktop.terminal_hidden && self.windows.focus() == Focus::Terminal {
                    self.desktop.terminal_hidden = true;
                } else {
                    self.show_terminal();
                }
                true
            }
            // A kept app (ADR-0109): its first window, as a window's icon
            // does; none open, it starts.
            Hit::DockPinned(index) => {
                let Some((id, name)) = self
                    .desktop
                    .kept()
                    .get(index)
                    .map(|&(id, name)| (String::from(id), String::from(name)))
                else {
                    return false;
                };
                let listed: Vec<(u32, &str)> =
                    windows.iter().map(|(w, app)| (*w, app.as_str())).collect();
                match oceans_window::dock::first_window(&name, &listed) {
                    Some(window) => self.taskbar_window(window),
                    None => self.launch(&id, ""),
                }
                true
            }
            Hit::TaskbarWindow(id) => {
                self.taskbar_window(id);
                true
            }
            Hit::TerminalMinimize => {
                self.desktop.terminal_hidden = true;
                true
            }
            Hit::Terminal => {
                self.windows.set_focus(Focus::Terminal);
                true
            }
            Hit::Allow => {
                self.answer(true);
                true
            }
            Hit::Deny => {
                self.answer(false);
                true
            }
            // The desktop's icons (ADR-0120): a click chooses one, a
            // double click opens it.
            Hit::DesktopIcon(index) => {
                if double && self.desktop.icon_chosen == Some(index) {
                    self.desktop.icon_chosen = None;
                    if let Some((label, id)) = self.desktop.desktop_items().get(index).cloned() {
                        say(
                            self.log,
                            format_args!("desktop: opening {label}, from the desktop"),
                        );
                        self.launch(&id, "");
                    }
                } else {
                    self.desktop.icon_chosen = Some(index);
                }
                true
            }
            // A click on the wallpaper lets the chosen icon go.
            Hit::Nothing => self.desktop.icon_chosen.take().is_some(),
        }
    }

    /// The Terminal, shown and given the keyboard.
    fn show_terminal(&mut self) {
        // The system's console (ADR-0115): the administrator's only.
        if !self.desktop.admin {
            return self.toast(
                String::from("The Terminal is for the administrator's account."),
                false,
            );
        }
        self.desktop.terminal_hidden = false;
        self.windows.set_focus(Focus::Terminal);
    }

    /// Starts an app in the background; asks first if it needs decisions.
    /// Starts app `id`, with `args` (a file to open, ADR-0099).
    fn launch(&mut self, id: &str, args: &str) {
        let Some(core) = self.core else {
            return self.toast("This desktop cannot start apps".to_string(), true);
        };
        let mut data = Vec::with_capacity(2 + id.len() + args.len());
        data.push(run_flags::DETACH);
        data.push(id.len() as u8);
        data.extend_from_slice(id.as_bytes());
        data.extend_from_slice(args.as_bytes());
        let mut reply = [0u8; 64];
        match core.call(op::RUN, &data, &[], &mut reply) {
            Ok(_) => {
                say(self.log, format_args!("desktop: started {id}"));
                let name = self.name_of(id);
                self.toast(alloc::format!("Started {name}"), false);
                self.refresh_apps();
            }
            Err((CoreError::Status(Status::NeedsConsent), _)) => {
                // Asked first; started with the same arguments after.
                self.pending_args = String::from(args);
                self.ask(core, id)
            }
            Err((error, _)) => {
                say(self.log, format_args!("desktop: {id}: {}", error.message()));
                self.toast(
                    alloc::format!("{}: {}", self.name_of(id), error.message()),
                    true,
                );
            }
        }
    }

    /// The user's account from Core (ADR-0110). While starting, it decides
    /// the lock: locked with a password, else open. `true` if the screen
    /// changes.
    fn refresh_user(&mut self) -> bool {
        let Some(core) = self.core else {
            return false;
        };
        let mut reply = [0u8; 64];
        let Ok(got) = core.call(op::USER, &[], &[], &mut reply) else {
            return false;
        };
        if got.len < 1 {
            return false;
        }
        let name = String::from(core::str::from_utf8(&reply[1..got.len]).unwrap_or("?"));
        let password = reply[0] != 0;
        let mut minutes = [0u8; 1];
        if let Ok(got) = core.call(op::LOCK_AFTER, &[], &[], &mut minutes)
            && got.len == 1
        {
            self.lock_after = minutes[0];
        }
        // The accounts (ADR-0115): the lock screen chooses among them.
        let mut list = [0u8; 1024];
        if let Ok(got) = core.call(op::USERS, &[], &[], &mut list) {
            let text = core::str::from_utf8(&list[..got.len]).unwrap_or("");
            let mut users = Vec::new();
            for (index, line) in text.lines().enumerate() {
                let mut fields = line.split('\t');
                let (id, name) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""));
                let flags = fields.next().unwrap_or("");
                if flags.contains('c') {
                    self.desktop.current_user = index;
                    self.desktop.admin = flags.contains('a');
                }
                users.push((String::from(id), String::from(name)));
            }
            self.desktop.users = users;
        }
        let changed = name != self.desktop.user_name || password != self.desktop.password_set;
        self.desktop.user_name = name;
        self.desktop.password_set = password;
        if self.desktop.lock != desktop::Lock::Locked {
            self.desktop.chosen_user = self.desktop.current_user;
        }
        if self.desktop.lock == desktop::Lock::Starting {
            self.desktop.lock = if password {
                say(self.log, format_args!("desktop: locked at start"));
                desktop::Lock::Locked
            } else {
                desktop::Lock::Open
            };
            return true;
        }
        changed
    }

    /// The appearance and accent from Core (ADR-0113): the desktop draws in
    /// them, and every window hears of a change. `true` if it changed.
    fn refresh_appearance(&mut self) -> bool {
        let Some(core) = self.core else {
            return false;
        };
        let mut reply = [0u8; 2];
        let Ok(got) = core.call(op::APPEARANCE, &[], &[], &mut reply) else {
            return false;
        };
        if got.len != 2 || oceans_draw::theme::bytes() == (reply[0], reply[1]) {
            return false;
        }
        let appearance = oceans_draw::theme::Appearance::from_byte(reply[0]);
        oceans_draw::theme::set(appearance, reply[1]);
        self.windows.set_theme(reply[0], reply[1]);
        say(
            self.log,
            format_args!(
                "desktop: appearance {}, accent {}",
                if appearance == oceans_draw::theme::Appearance::Dark {
                    "dark"
                } else {
                    "light"
                },
                reply[1]
            ),
        );
        true
    }

    /// The scale to draw at (ADR-0136): the one the user chose, if this
    /// screen can take it, else the one its density suggests. `true` if
    /// it is not the one drawn at now.
    fn refresh_scale(&mut self) -> bool {
        if let Some(core) = self.core {
            let mut reply = [0u8; 2];
            if let Ok(got) = core.call(op::SCALE, &[], &[], &mut reply)
                && got.len == 2
            {
                self.scale_chosen = u16::from_le_bytes(reply);
            }
        }
        self.wanted_scale() != self.scale
    }

    fn wanted_scale(&self) -> Scale {
        let (w, h) = self.screen_pixels;
        Scale::new(self.scale_chosen)
            .filter(|s| s.fits(w, h))
            .unwrap_or_else(|| Scale::auto(w, h, self.screen_mm))
    }

    /// Draws at the scale wanted from now on: the desktop's logical size
    /// follows, the windows are put back inside it and every app makes
    /// its pixels again (`Manager::rearrange`).
    fn apply_scale(&mut self, canvas: &mut Canvas) {
        let scale = self.wanted_scale();
        self.scale = scale;
        canvas.set_scale(scale);
        let (w, h) = (canvas.width, canvas.height);
        self.windows.rearrange(
            desktop::area(w, h),
            oceans_window::Rect::new(0, 0, w, h),
            2 * (w * h) as usize,
        );
        let (x, y) = self.desktop.pointer_device;
        self.desktop.pointer = (scale.logical(x), scale.logical(y));
        let (dw, dh) = self.screen_pixels;
        let size = match self.screen_mm {
            Some((mw, mh)) => alloc::format!("{mw}x{mh} mm"),
            None => String::from("size unknown"),
        };
        say(
            self.log,
            format_args!(
                "display: scale {}% ({}), {dw}x{dh} pixels ({size}) as {w}x{h}",
                scale.percent(),
                if self.scale_chosen == 0 {
                    "automatic"
                } else {
                    "chosen"
                },
            ),
        );
    }

    /// From Control Center (ADR-0118): Core keeps the appearance for the
    /// account in use, and the desktop and every window follow at once.
    fn set_appearance(&mut self, dark: u8, accent: u8) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        if core
            .call(op::SET_APPEARANCE, &[dark, accent], &[], &mut reply)
            .is_ok()
        {
            self.refresh_appearance();
        }
    }

    /// Safe mode (ADR-0122), said once at start: why, and what is off.
    fn show_mode(&mut self) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 1];
        let safe = match core.call(op::MODE, &[], &[], &mut reply) {
            Ok(got) if got.len == 1 => reply[0],
            _ => 0,
        };
        let text = match safe {
            0 => return,
            1 => "Safe mode: the services kept to start at boot are off.",
            _ => {
                "Safe mode: the last boots did not finish, so the services kept to start at boot are off."
            }
        };
        say(self.log, format_args!("desktop: {text}"));
        self.toast(String::from(text), true);
    }

    /// The network from the reader end Core gave (ADR-0118), asked for
    /// once if not yet; `true` if what shows changed.
    fn refresh_network(&mut self) -> bool {
        if self.net_reader.is_none()
            && let Some(core) = self.core
        {
            let mut reply = [0u8; 8];
            self.net_reader = core
                .call(op::NET_READER, &[], &[], &mut reply)
                .ok()
                .and_then(|got| got.handle);
        }
        let network = self
            .net_reader
            .map(|reader| match oceans_net_proto::info(reader) {
                Ok(info) if info.configured => desktop::Network {
                    connected: true,
                    address: alloc::format!("{}", oceans_net_proto::Dotted(info.address)),
                },
                _ => desktop::Network::default(),
            });
        let changed = network != self.desktop.network;
        if changed && let Some(network) = &network {
            if network.connected {
                say(
                    self.log,
                    format_args!("desktop: network connected, {}", network.address),
                );
            } else {
                say(self.log, format_args!("desktop: network not connected"));
            }
        }
        self.desktop.network = network;
        changed
    }

    // ---- Wi-Fi (ADR-0130): the driver's files in /system/wifi ---------------

    fn wifi_read(&self, name: &str) -> Option<Vec<u8>> {
        let dir = self.wifi_dir.as_ref()?;
        let (node, kind) = dir.open(name, 0).ok()?;
        let mut data = alloc::vec![0u8; 4096];
        let len = if kind == oceans_fs_proto::Kind::File {
            node.read(0, &mut data).ok()
        } else {
            None
        };
        node.close();
        data.truncate(len?);
        Some(data)
    }

    fn wifi_write(&self, name: &str, bytes: &[u8]) -> bool {
        let Some(dir) = self.wifi_dir.as_ref() else {
            return false;
        };
        let create = oceans_fs_proto::flags::CREATE_FILE | oceans_fs_proto::flags::WRITE;
        let Ok((node, kind)) = dir.open(name, create) else {
            return false;
        };
        let written = kind == oceans_fs_proto::Kind::File
            && node.truncate(0).is_ok()
            && node.write_all(0, bytes).is_ok();
        node.close();
        written
    }

    fn wifi_remove(&self, name: &str) {
        if let Some(dir) = self.wifi_dir.as_ref() {
            let _ = dir.remove(name);
        }
    }

    /// Wi-Fi as the driver last wrote it; `None` without a driver (no
    /// `status`). `true` if what shows changed. Control Center closed, the
    /// panel closes too (and a half-typed password goes).
    fn refresh_wifi(&mut self) -> bool {
        use oceans_wifi::state;
        self.wifi_refreshed_ms = oceans_rt::clock_ms();
        let mut changed = false;
        if !self.desktop.volume_open
            && (self.desktop.wifi_open || self.desktop.wifi_entry.is_some())
        {
            self.close_wifi();
            changed = true;
        }
        let wifi = self.wifi_read(state::STATUS).map(|status| {
            let name = |ssid: state::Ssid| desktop::ssid_name(ssid.as_bytes());
            let state = match state::parse_status(&status) {
                Some(state::Status::Off) | None => desktop::WifiState::Off,
                Some(state::Status::Idle) => desktop::WifiState::Idle,
                Some(state::Status::Scanning) => desktop::WifiState::Scanning,
                Some(state::Status::Joining(ssid)) => desktop::WifiState::Joining(name(ssid)),
                Some(state::Status::Joined { ssid, rssi, .. }) => desktop::WifiState::Joined {
                    name: name(ssid),
                    rssi,
                },
                Some(state::Status::Failed { ssid, reason }) => desktop::WifiState::Failed {
                    name: name(ssid),
                    reason: String::from(reason),
                },
            };
            let networks = self
                .wifi_read(state::NETWORKS)
                .map(|file| {
                    state::lines(&file)
                        .filter_map(state::parse_network)
                        .collect()
                })
                .unwrap_or_default();
            desktop::Wifi { state, networks }
        });
        if wifi != self.desktop.wifi {
            if let Some(w) = &wifi
                && self.desktop.wifi.as_ref().map(|old| &old.state) != Some(&w.state)
            {
                self.say_wifi(&w.state);
            }
            self.desktop.wifi = wifi;
            changed = true;
        }
        changed
    }

    fn say_wifi(&self, state: &desktop::WifiState) {
        use desktop::WifiState::*;
        match state {
            Off => say(self.log, format_args!("desktop: Wi-Fi off")),
            Idle => say(self.log, format_args!("desktop: Wi-Fi not connected")),
            Scanning => say(self.log, format_args!("desktop: Wi-Fi scanning")),
            Joining(name) => say(self.log, format_args!("desktop: Wi-Fi joining {name}")),
            Joined { name, .. } => say(self.log, format_args!("desktop: Wi-Fi joined {name}")),
            Failed { name, reason } => say(
                self.log,
                format_args!("desktop: Wi-Fi could not join {name}: {reason}"),
            ),
        }
    }

    fn close_wifi(&mut self) {
        self.desktop.wifi_open = false;
        self.desktop.wifi_entry = None;
    }

    /// Asks the driver for a scan (it removes the file once it has seen it).
    fn wifi_scan(&mut self) {
        if self.wifi_write(oceans_wifi::state::SCAN, b"1\n") {
            say(self.log, format_args!("desktop: Wi-Fi scan asked for"));
        }
    }

    /// A network chosen in the list: the one joined, left; an open one,
    /// joined; a WPA2 one, its password asked for.
    fn wifi_choose(&mut self, index: usize) {
        let Some(wifi) = &self.desktop.wifi else {
            return;
        };
        let Some(network) = wifi.networks.get(index).copied() else {
            return;
        };
        let name = desktop::ssid_name(network.ssid.as_bytes());
        let joined =
            matches!(&wifi.state, desktop::WifiState::Joined { name: n, .. } if *n == name);
        if joined {
            self.wifi_remove(oceans_wifi::state::SSID);
            say(self.log, format_args!("desktop: Wi-Fi: leaving {name}"));
            return;
        }
        match network.security {
            oceans_wifi::Security::Open => self.wifi_join(network.ssid, None),
            oceans_wifi::Security::Wpa2Personal => {
                self.desktop.wifi_entry = Some(desktop::WifiEntry {
                    ssid: network.ssid,
                    name,
                    password: String::new(),
                });
            }
            oceans_wifi::Security::Unsupported => self.toast(
                String::from(
                    "This network's security is not supported: WPA2-Personal or open only",
                ),
                true,
            ),
        }
    }

    /// Sets the network to join: the password first, then the name, which
    /// the driver acts on. Each ends with a newline, as the shell's `cat`
    /// shows best (the driver drops it).
    fn wifi_join(&mut self, ssid: oceans_wifi::state::Ssid, password: Option<&str>) {
        let line = |bytes: &[u8]| [bytes, b"\n"].concat();
        let saved = match password {
            Some(password) => {
                self.wifi_write(oceans_wifi::state::PASSWORD, &line(password.as_bytes()))
            }
            None => {
                self.wifi_remove(oceans_wifi::state::PASSWORD);
                true
            }
        };
        if saved && self.wifi_write(oceans_wifi::state::SSID, &line(ssid.as_bytes())) {
            let name = desktop::ssid_name(ssid.as_bytes());
            say(self.log, format_args!("desktop: Wi-Fi: joining {name}"));
            self.desktop.wifi_entry = None;
        } else {
            self.toast(String::from("Could not set the Wi-Fi network"), true);
        }
    }

    /// A key into the password (ADR-0130): printable ASCII, as WPA2 has it;
    /// Backspace, Enter (Join), Escape (Cancel). `true` if the screen
    /// changes.
    fn wifi_key(&mut self, byte: u8) -> bool {
        let Some(entry) = self.desktop.wifi_entry.as_mut() else {
            return false;
        };
        match byte {
            0x1b => self.desktop.wifi_entry = None,
            b'\r' => {
                let count = entry.password.chars().count();
                if count >= 8 {
                    let (ssid, password) = (entry.ssid, core::mem::take(&mut entry.password));
                    self.wifi_join(ssid, Some(&password));
                }
            }
            0x08 | 0x7f => {
                entry.password.pop();
            }
            0x20..=0x7e if entry.password.len() < 63 => {
                let c = self
                    .desktop
                    .layout
                    .character(byte)
                    .unwrap_or(char::from(byte));
                if !c.is_ascii() || c.is_ascii_control() {
                    return false;
                }
                if let Some(entry) = self.desktop.wifi_entry.as_mut() {
                    entry.password.push(c);
                }
            }
            _ => return false,
        }
        true
    }

    /// No input for the time the user chose (ADR-0112): the screen locks.
    fn lock_idle(&mut self) {
        self.lock_screen();
        if self.desktop.lock == desktop::Lock::Locked {
            say(
                self.log,
                format_args!(
                    "desktop: locked after {} minute{} without input",
                    self.lock_after,
                    if self.lock_after == 1 { "" } else { "s" }
                ),
            );
        }
    }

    /// Super+L (ADR-0110): with a password, the screen locks; the windows
    /// and the Terminal stay as they are, unseen until it opens.
    fn lock_screen(&mut self) {
        self.refresh_user();
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 64];
        let password = matches!(core.call(op::USER, &[], &[], &mut reply), Ok(got) if got.len >= 1 && reply[0] != 0);
        if !password {
            return self.toast(
                String::from("Set a password first: user password ... in the Terminal"),
                false,
            );
        }
        self.desktop.lock = desktop::Lock::Locked;
        self.desktop.start_open = false;
        self.desktop.volume_open = false;
        self.desktop.notices_open = false;
        self.desktop.user_menu_open = false;
        self.desktop.lock_input.clear();
        self.desktop.lock_note.clear();
        self.desktop.chosen_user = self.desktop.current_user;
        say(self.log, format_args!("desktop: locked"));
    }

    /// A key on the lock screen (ADR-0110): typed into the password (never
    /// shown), Backspace, Escape to clear, Enter to try it with Core.
    fn lock_key(&mut self, byte: u8) {
        if self.desktop.lock != desktop::Lock::Locked {
            return;
        }
        let desktop = &mut self.desktop;
        match byte {
            b'\r' => self.try_unlock(),
            0x1b => desktop.lock_input.clear(),
            0x08 | 0x7f => {
                desktop.lock_input.pop();
            }
            0x20..=0x7e if desktop.lock_input.len() < 64 => {
                let c = desktop.layout.character(byte).unwrap_or(char::from(byte));
                desktop.lock_input.push(c)
            }
            // Another account (ADR-0115): what was typed is forgotten.
            oceans_window::proto::KEY_LEFT if desktop.chosen_user > 0 => {
                let to = desktop.chosen_user - 1;
                self.choose_user(to);
            }
            oceans_window::proto::KEY_RIGHT if desktop.chosen_user + 1 < desktop.users.len() => {
                let to = desktop.chosen_user + 1;
                self.choose_user(to);
            }
            _ => {}
        }
    }

    /// Another account signed in (ADR-0115): Core stopped the apps; what
    /// was copied, the search and the Terminal are not the new user's.
    fn signed_in(&mut self) {
        self.windows.clear_clipboard();
        // The last user's notifications are not the new one's (ADR-0119).
        self.desktop.notices.clear();
        self.desktop.unread = 0;
        self.refresh_user();
        self.refresh_pinned();
        self.refresh_appearance();
        self.refresh_layout();
        self.refresh_apps();
        if !self.desktop.admin {
            self.desktop.terminal_hidden = true;
        }
        say(
            self.log,
            format_args!("desktop: {} signed in", self.desktop.user_name),
        );
    }

    /// The lock screen's account (ADR-0115).
    fn choose_user(&mut self, index: usize) {
        let desktop = &mut self.desktop;
        desktop.chosen_user = index;
        desktop.lock_input.clear();
        desktop.lock_note.clear();
        let name = desktop
            .users
            .get(index)
            .map_or("?", |(_, name)| name.as_str());
        say(
            self.log,
            format_args!("desktop: {name} chosen at the lock screen"),
        );
    }

    fn try_unlock(&mut self) {
        let Some(core) = self.core else {
            return;
        };
        let password = core::mem::take(&mut self.desktop.lock_input);
        let mut reply = [0u8; 16];
        // Several accounts (ADR-0115): the chosen one signs in.
        let chosen = self.desktop.chosen_user;
        let other = chosen != self.desktop.current_user;
        let result = match self.desktop.users.get(chosen) {
            Some((id, _)) if self.desktop.users.len() > 1 => {
                let mut data = Vec::with_capacity(1 + id.len() + password.len());
                data.push(id.len() as u8);
                data.extend_from_slice(id.as_bytes());
                data.extend_from_slice(password.as_bytes());
                core.call(op::SIGN_IN, &data, &[], &mut reply)
            }
            _ => core.call(op::UNLOCK, password.as_bytes(), &[], &mut reply),
        };
        match result {
            Ok(_) => {
                self.desktop.lock = desktop::Lock::Open;
                self.desktop.lock_note.clear();
                if other {
                    self.signed_in();
                } else {
                    say(self.log, format_args!("desktop: unlocked"));
                }
            }
            Err((CoreError::Status(Status::TooSoon), got)) => {
                let wait = core::str::from_utf8(&reply[..got.min(reply.len())]).unwrap_or("?");
                self.desktop.lock_note = alloc::format!("Too many wrong passwords: wait {wait} s");
                say(
                    self.log,
                    format_args!("desktop: unlock refused: wait {wait} s"),
                );
            }
            Err((error, _)) => {
                self.desktop.lock_note = String::from(match error {
                    CoreError::Status(Status::WrongPassword) => "Wrong password: try again",
                    _ => "The password cannot be checked now",
                });
                say(self.log, format_args!("desktop: unlock refused"));
            }
        }
    }

    /// The windows as the dock needs them: id and app name.
    fn dock_windows(&self) -> Vec<desktop::DockWindow> {
        self.windows
            .frames()
            .iter()
            .map(|f| (f.id, f.app.clone()))
            .collect()
    }

    /// A window's icon in the dock: like other docks, it brings the window
    /// forward, or minimizes it if it is in front with the keyboard.
    fn taskbar_window(&mut self, id: u32) {
        let shown = self
            .windows
            .frames()
            .iter()
            .any(|f| f.id == id && !f.minimized);
        if shown && self.windows.focus() == Focus::Window(id) {
            self.windows.minimize(id);
        } else {
            self.windows.set_focus(Focus::Window(id));
        }
    }

    /// A right click (ADR-0109): on an app's tile or search result, or on
    /// a window's icon, its app is kept in the dock; on a kept app's icon,
    /// taken out. `true` if the screen changes.
    fn right_click(&mut self, x: i32, y: i32, width: i32, height: i32) -> bool {
        let windows = self.dock_windows();
        let id = match self.desktop.hit(x, y, width, height, &windows) {
            Hit::StartApp(index) => self.desktop.apps.get(index).map(|a| a.id.clone()),
            Hit::Found(index) => match self.desktop.found.get(index) {
                Some(desktop::Found::App(app)) => self.desktop.apps.get(*app).map(|a| a.id.clone()),
                _ => None,
            },
            Hit::DockPinned(index) => self
                .desktop
                .kept()
                .get(index)
                .map(|&(id, _)| String::from(id)),
            Hit::TaskbarWindow(window) => windows
                .iter()
                .find(|(w, _)| *w == window)
                .and_then(|(_, name)| self.desktop.apps.iter().find(|a| &a.name == name))
                .map(|a| a.id.clone()),
            _ => None,
        };
        let Some(id) = id else {
            return false;
        };
        let keep = !self.desktop.pinned.contains(&id);
        if keep && self.desktop.pinned.len() >= oceans_window::dock::MAX_PINNED {
            self.toast(String::from("Wave is full: take an app out first."), true);
            return true;
        }
        let Some(core) = self.core else {
            return false;
        };
        let mut data = Vec::with_capacity(1 + id.len());
        data.push(u8::from(keep));
        data.extend_from_slice(id.as_bytes());
        let mut reply = [0u8; 256];
        let name = self.name_of(&id);
        match core.call(op::PIN, &data, &[], &mut reply) {
            Ok(got) => {
                let text = core::str::from_utf8(&reply[..got.len]).unwrap_or("");
                self.desktop.pinned = oceans_window::dock::decode(text);
                let what = if keep { "kept in" } else { "taken out of" };
                say(self.log, format_args!("desktop: {name} {what} Wave"));
                self.toast(alloc::format!("{name} {what} Wave"), false);
            }
            Err((error, _)) => {
                self.toast(alloc::format!("{name}: {}", error.message()), true);
            }
        }
        true
    }

    /// The apps kept in the dock, from Core (ADR-0109).
    fn refresh_pinned(&mut self) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 256];
        if let Ok(got) = core.call(op::PINNED, &[], &[], &mut reply) {
            let text = core::str::from_utf8(&reply[..got.len]).unwrap_or("");
            self.desktop.pinned = oceans_window::dock::decode(text);
        }
    }

    /// Opens Launchpad with an empty search, or closes it.
    fn toggle_panel(&mut self) {
        self.desktop.start_open = !self.desktop.start_open;
        self.desktop.wave_open = false;
        self.desktop.notices_open = false;
        self.desktop.user_menu_open = false;
        self.desktop.volume_open = false;
        self.desktop.search.clear();
        self.desktop.found.clear();
        self.desktop.chosen = 0;
        if self.desktop.start_open {
            self.refresh_apps();
        }
    }

    /// Super+Space or the menu bar (ADR-0114): the next keyboard layout.
    fn next_layout(&mut self) {
        self.desktop.layout = self.desktop.layout.next();
        let name = self.desktop.layout.name();
        say(self.log, format_args!("desktop: keyboard layout {name}"));
        // Kept by Core for the account in use (ADR-0116); at the lock
        // screen the account may be about to change, so only for typing
        // the password.
        if let Some(core) = self
            .core
            .filter(|_| self.desktop.lock == desktop::Lock::Open)
        {
            let mut reply = [0u8; 8];
            let _ = core.call(
                op::SET_LAYOUT,
                &[self.desktop.layout.byte()],
                &[],
                &mut reply,
            );
        }
    }

    /// The account in use's keyboard layout, from Core (ADR-0116).
    fn refresh_layout(&mut self) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 1];
        if let Ok(got) = core.call(op::LAYOUT, &[], &[], &mut reply)
            && got.len == 1
        {
            self.desktop.layout = oceans_window::layout::Layout::from_byte(reply[0]);
        }
    }

    /// A key while Launchpad is open (ADR-0108): typed into the
    /// search; the arrows choose a result, Enter opens it, Escape closes
    /// the panel. `true` if the screen changes.
    fn search_key(&mut self, byte: u8) -> bool {
        use oceans_window::proto::{KEY_DOWN, KEY_UP};
        let desktop = &mut self.desktop;
        match byte {
            0x1b => desktop.start_open = false,
            b'\r' => self.open_found(self.desktop.chosen),
            KEY_UP => desktop.chosen = desktop.chosen.saturating_sub(1),
            KEY_DOWN => {
                if desktop.chosen + 1 < desktop.found.len() {
                    desktop.chosen += 1;
                }
            }
            0x08 | 0x7f => {
                desktop.search.pop();
                self.refresh_search();
            }
            0x20..=0x7e if desktop.search.len() < oceans_search::MAX_QUERY => {
                let c = desktop.layout.character(byte).unwrap_or(char::from(byte));
                desktop.search.push(c);
                self.refresh_search();
            }
            _ => return false,
        }
        true
    }

    /// The results for what is typed: the apps whose names match, then the
    /// files of Home Core finds (from two letters on).
    fn refresh_search(&mut self) {
        use desktop::{Found, MAX_FOUND};
        let query = self.desktop.search.clone();
        let names = self.desktop.apps.iter().map(|a| a.name.as_str());
        let mut found: Vec<Found> = oceans_search::best(&query, names, 4)
            .into_iter()
            .map(Found::App)
            .collect();
        if query.trim().len() >= 2
            && let Some(core) = self.core
        {
            let mut reply = [0u8; 256];
            if let Ok(got) = core.call(op::FIND, query.as_bytes(), &[], &mut reply) {
                let text = core::str::from_utf8(&reply[..got.len]).unwrap_or("");
                found.extend(
                    text.split('\n')
                        .filter(|path| !path.is_empty())
                        .map(|path| Found::File(String::from(path))),
                );
            }
        }
        found.truncate(MAX_FOUND);
        let (apps, files) = (
            found.iter().filter(|f| matches!(f, Found::App(_))).count(),
            found.iter().filter(|f| matches!(f, Found::File(_))).count(),
        );
        say(
            self.log,
            format_args!("desktop: search: {apps} apps, {files} files"),
        );
        self.desktop.found = found;
        self.desktop.chosen = 0;
    }

    /// Opens search result `index` (ADR-0108): an app starts; a file opens
    /// in the app that opens its kind (ADR-0099). The panel closes.
    fn open_found(&mut self, index: usize) {
        use desktop::Found;
        let Some(found) = self.desktop.found.get(index).cloned() else {
            let query = self.desktop.search.as_str();
            if !query.is_empty() {
                say(self.log, format_args!("desktop: nothing found for {query}"));
            }
            return;
        };
        self.desktop.start_open = false;
        match found {
            Found::App(app) => {
                let Some(id) = self.desktop.apps.get(app).map(|a| a.id.clone()) else {
                    return;
                };
                say(self.log, format_args!("desktop: opening {id}, from search"));
                self.launch(&id, "");
            }
            Found::File(path) => {
                let Some((id, _)) = self.openers(&path).into_iter().next() else {
                    return self.toast(String::from("No app opens this kind of file."), true);
                };
                say(
                    self.log,
                    format_args!("desktop: opening {path} with {id}, from search"),
                );
                self.launch(&id, &path);
            }
        }
    }

    fn name_of(&self, id: &str) -> String {
        self.desktop
            .apps
            .iter()
            .find(|a| a.id == id)
            .map_or_else(|| id.to_string(), |a| a.name.clone())
    }

    /// Queues the undecided permissions of `id` and shows the first.
    fn ask(&mut self, core: Core, id: &str) {
        self.pending.clear();
        let mut reply = [0u8; 256];
        for index in 0u8..16 {
            let Ok(got) = core.about(op::PERMISSION, &[index], id, &mut reply) else {
                break;
            };
            if got.len >= 2 && Decision::from_byte(reply[1]) == Some(Decision::Undecided) {
                let reason = core::str::from_utf8(&reply[2..got.len])
                    .unwrap_or("")
                    .to_string();
                self.pending.push((reply[0], reason));
            }
        }
        self.pending.reverse();
        self.next_question(core, id);
    }

    fn next_question(&mut self, core: Core, id: &str) {
        let Some((permission, reason)) = self.pending.pop() else {
            self.desktop.dialog = None;
            // Every question answered: start it with what was allowed.
            let args = core::mem::take(&mut self.pending_args);
            return self.launch(id, &args);
        };
        let Some(&catalog) = Permission::ALL.get(usize::from(permission)) else {
            return self.next_question(core, id);
        };
        let field = |which| {
            let mut reply = [0u8; 128];
            core.about(op::INFO, &[which], id, &mut reply)
                .ok()
                .map(|got| String::from(core::str::from_utf8(&reply[..got.len]).unwrap_or("?")))
                .unwrap_or_default()
        };
        let app = alloc::format!(
            "{} ({id} {}, from {})",
            field(field::NAME),
            field(field::VERSION),
            field(field::PUBLISHER)
        );
        say(
            self.log,
            format_args!("desktop: permission dialog for {id}: {}", catalog.name()),
        );
        self.desktop.dialog = Some(Dialog {
            question: Question::Permission {
                id: id.to_string(),
                permission,
            },
            title: "Permission request",
            app,
            ask: "asks to:",
            description: catalog.description().to_string(),
            note: if reason.is_empty() {
                String::new()
            } else {
                alloc::format!("Reason given by the app: \"{reason}\"")
            },
            deny: "Deny",
            allow: "Allow",
        });
    }

    fn answer(&mut self, allow: bool) {
        let Some(dialog) = self.desktop.dialog.take() else {
            return;
        };
        if let Question::Power(action) = dialog.question {
            return self.answer_power(action, allow);
        }
        let Some(core) = self.core else {
            return;
        };
        let (id, permission) = match dialog.question {
            Question::Power(_) => return,
            Question::Permission { id, permission } => (id, permission),
            Question::Install { number, name } => {
                return self.answer_install(core, number, &name, allow);
            }
        };
        let name = Permission::ALL
            .get(usize::from(permission))
            .map_or("?", |p| p.name());
        let request = [
            permission,
            if allow {
                decision::ALLOW
            } else {
                decision::DENY
            },
            source::DIALOG,
        ];
        let mut reply = [0u8; 8];
        match core.about(op::DECIDE, &request, &id, &mut reply) {
            Ok(_) => say(
                self.log,
                format_args!(
                    "desktop: {name} {} for {} in the dialog",
                    if allow { "allowed" } else { "denied" },
                    id
                ),
            ),
            Err((error, _)) => {
                self.pending.clear();
                return self.toast(
                    alloc::format!("Could not record the decision: {}", error.message()),
                    true,
                );
            }
        }
        self.next_question(core, &id);
    }

    /// Restart or Shut Down, confirmed (ADR-0085): init stops the system,
    /// this desktop too.
    fn answer_power(&mut self, action: u64, confirmed: bool) {
        let Some(power) = self.power.filter(|_| confirmed) else {
            return;
        };
        let what = if action == oceans_rt::power::OFF {
            "switch off"
        } else {
            "restart"
        };
        match oceans_rt::request_power(power, action) {
            Ok(()) => say(self.log, format_args!("desktop: asked to {what}")),
            Err(oceans_rt::Error::NotFound) => self.toast(
                "Oceans cannot switch this machine off: turn it off yourself".to_string(),
                true,
            ),
            Err(error) => self.toast(alloc::format!("Could not {what}: {error:?}"), true),
        }
    }

    /// A Store install waiting for the user (ADR-0061): asked in a dialog.
    /// `true` if one is now shown.
    fn refresh_pending(&mut self) -> bool {
        let Some(core) = self.core else {
            return false;
        };
        if self.desktop.dialog.is_some() || !self.pending.is_empty() {
            return false;
        }
        let mut reply = [0u8; 256];
        let Ok(got) = core.call(op::PENDING, &[], &[], &mut reply) else {
            return false;
        };
        if got.len < 4 {
            return false;
        }
        let number = u32::from_le_bytes([reply[0], reply[1], reply[2], reply[3]]);
        let mut fields = parts(&reply[4..got.len]);
        let mut next = || fields.next().unwrap_or("?").to_string();
        let (id, version, name, publisher) = (next(), next(), next(), next());
        let (permissions, previous, description) = (next(), next(), next());
        say(
            self.log,
            format_args!("desktop: install dialog for {id} {version}"),
        );
        let permissions = if permissions.is_empty() {
            String::from("nothing")
        } else {
            permissions.replace(',', ", ")
        };
        self.desktop.dialog = Some(Dialog {
            question: Question::Install {
                number,
                name: name.clone(),
            },
            title: if previous.is_empty() {
                "Install an app"
            } else {
                "Update an app"
            },
            app: if previous.is_empty() {
                alloc::format!("{name} {version} ({id}, from {publisher})")
            } else {
                alloc::format!("{name} {previous} -> {version} ({id}, from {publisher})")
            },
            ask: "from the Store, which may ask for:",
            description: permissions,
            note: description,
            deny: "Cancel",
            allow: "Install",
        });
        true
    }

    fn answer_install(&mut self, core: Core, number: u32, name: &str, install: bool) {
        let mut request = [0u8; 5];
        request[..4].copy_from_slice(&number.to_le_bytes());
        request[4] = u8::from(install);
        let mut reply = [0u8; 128];
        match core.call(op::ACCEPT, &request, &[], &mut reply) {
            Ok(_) if install => {
                say(
                    self.log,
                    format_args!("desktop: installed {name} from the Store"),
                );
                self.toast(alloc::format!("Installed {name}"), false);
                self.refresh_apps();
            }
            Ok(_) => say(
                self.log,
                format_args!("desktop: install of {name} cancelled"),
            ),
            Err((error, _)) => self.toast(alloc::format!("{name}: {}", error.message()), true),
        }
    }

    /// The app list; `true` if it changed.
    fn refresh_apps(&mut self) -> bool {
        let Some(core) = self.core else {
            return false;
        };
        let mut apps = Vec::new();
        let mut reply = [0u8; 256];
        for index in 0u32..64 {
            match core.call(op::LIST, &index.to_le_bytes(), &[], &mut reply) {
                Ok(got) if got.len >= 1 => {
                    let mut fields = parts(&reply[1..got.len]);
                    let id = fields.next().unwrap_or("?").to_string();
                    let version = fields.next().unwrap_or("?").to_string();
                    let name = fields.next().unwrap_or("?").to_string();
                    apps.push(App {
                        id,
                        name,
                        version,
                        running: reply[0] != 0,
                    });
                }
                _ => break,
            }
        }
        let changed = apps.len() != self.desktop.apps.len()
            || apps
                .iter()
                .zip(&self.desktop.apps)
                .any(|(a, b)| a.id != b.id || a.version != b.version || a.running != b.running);
        let counted = apps.len() != self.desktop.apps.len();
        if counted {
            say(
                self.log,
                format_args!("desktop: {} apps in the launcher", apps.len()),
            );
        }
        if changed {
            self.desktop.apps = apps;
        }
        // At start, and when apps come or go: the dock's (ADR-0109); Core
        // keeps only installed ones.
        if counted {
            self.refresh_pinned();
            let kept = self.desktop.kept().len();
            if kept > 0 {
                say(
                    self.log,
                    format_args!(
                        "desktop: {kept} app{} kept in Wave",
                        if kept == 1 { "" } else { "s" }
                    ),
                );
            }
        }
        changed
    }

    /// The time zone from Core (ADR-0143); `true` if it changed.
    fn refresh_time_zone(&mut self) -> bool {
        let Some(core) = self.core else {
            return false;
        };
        let mut reply = [0u8; 2];
        match core.call(op::TIME_ZONE, &[], &[], &mut reply) {
            Ok(got) if got.len == 2 && i16::from_le_bytes(reply) != self.time_zone => {
                self.time_zone = i16::from_le_bytes(reply);
                true
            }
            _ => false,
        }
    }

    /// The time here: the wall clock moved by the time zone.
    fn local_time_ms(&self) -> Option<u64> {
        let offset = i64::from(self.time_zone) * 60_000;
        oceans_rt::unix_time_ms().map(|ms| ms.saturating_add_signed(offset))
    }

    /// The clock and the status; `true` if they changed.
    fn refresh_clock(&mut self) -> bool {
        let clock = self
            .local_time_ms()
            .map_or_else(|| String::from("time not set"), desktop::clock_text);
        let mut status = String::new();
        if let Some(sysinfo) = self.sysinfo {
            let mut record = [0u8; 32];
            if let Ok(32) = oceans_rt::system_info(sysinfo, 1, &mut record) {
                let word = |i: usize| u64::from_le_bytes(record[i..i + 8].try_into().unwrap());
                let (total, free) = (word(8), word(16));
                if let Some(percent) = (total.saturating_sub(free) * 100).checked_div(total) {
                    let _ = write!(status, "RAM {percent}%");
                }
            }
        }
        let changed = clock != self.desktop.clock || status != self.desktop.status;
        self.desktop.clock = clock;
        self.desktop.status = status;
        // In words, for the notifications' panel (ADR-0119).
        if let Some(unix_ms) = self.local_time_ms() {
            let (time, date) = desktop::clock_long(unix_ms);
            self.desktop.clock_time = time;
            self.desktop.clock_date = date;
        }
        changed
    }

    /// The console's text; `true` if it changed.
    fn refresh_terminal(&mut self) -> bool {
        let mut buffer = alloc::vec![0u8; self.text_capacity];
        let Ok(len) = oceans_rt::display_text(self.display, &mut buffer) else {
            return false;
        };
        if len < 16 {
            return false;
        }
        let half = |i: usize| usize::from(u16::from_le_bytes([buffer[i], buffer[i + 1]]));
        let generation = u64::from_le_bytes(buffer[8..16].try_into().unwrap());
        if generation == self.desktop.terminal.generation && !self.desktop.terminal.cells.is_empty()
        {
            return false;
        }
        let (cols, rows) = (half(0), half(2));
        let cells = buffer[16..len].to_vec();
        if cells.len() < cols * rows {
            return false;
        }
        self.desktop.terminal = Terminal {
            cols,
            rows,
            col: half(4),
            row: half(6),
            generation,
            cells,
        };
        true
    }
}
