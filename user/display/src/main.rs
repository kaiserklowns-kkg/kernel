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

use canvas::Canvas;
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
    /// The last press of the main button: when and where (a double click
    /// maximizes, ADR-0097).
    last_press: Option<(u64, i32, i32)>,
    /// What the app being asked about is started with once the user has
    /// answered (a file to open, ADR-0099).
    pending_args: String,
    /// The sound panel's slider is held (ADR-0100).
    volume_drag: bool,
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
    // Drawn once (ADR-0078); each frame starts from a copy.
    canvas.set_wallpaper(desktop::wallpaper(canvas.width, canvas.height));

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
            terminal_focused: true,
            can_power: directory.find("power", "power").is_some(),
            ..Desktop::default()
        },
        pending: Vec::new(),
        last_press: None,
        pending_args: String::new(),
        volume_drag: false,
        text_capacity: 16 + info.cols as usize * info.rows as usize,
        console,
        windows: Manager::new(area, 2 * screen_pixels),
        owners: BTreeMap::new(),
        pixels: BTreeMap::new(),
        power: directory.find("power", "power"),
    };
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
    service.refresh_volume();
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
        }
        if bits & TICK != 0 {
            let _ = oceans_rt::timer_set(notification, TICK, TICK_MS);
            let now = oceans_rt::clock_ms();
            dirty |= service.refresh_clock();
            dirty |= service.refresh_terminal();
            if now.saturating_sub(last_apps) >= APPS_EVERY_MS {
                last_apps = now;
                dirty |= service.refresh_apps();
                dirty |= service.refresh_volume();
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
            if service.desktop.volume_shown_until != 0 && service.desktop.volume_shown_until <= now {
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
        let views: Vec<WindowView<'_>> = self
            .windows
            .frames()
            .iter()
            .map(|frame| WindowView {
                frame,
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

    /// A pointer event; `true` if the screen must change.
    fn pointer(&mut self, kind: Kind, time_ms: u64, canvas: &Canvas) -> bool {
        let (w, h) = (canvas.width, canvas.height);
        let (x, y) = self.desktop.pointer;
        match kind {
            Kind::Motion { dx, dy } => {
                self.desktop.pointer = ((x + dx).clamp(0, w - 1), (y + dy).clamp(0, h - 1));
            }
            Kind::Absolute { x, y, x_max, y_max } if x_max > 0 && y_max > 0 => {
                let scale = |v: u32, max: u32, size: i32| {
                    (u64::from(v) * (size as u64 - 1) / u64::from(max)) as i32
                };
                self.desktop.pointer = (scale(x, x_max, w), scale(y, y_max, h));
            }
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
                let over = self.desktop.start_open
                    || y < desktop::MENU_HEIGHT
                    || y >= h - desktop::DOCK_RESERVE
                    || (self.desktop.volume_open
                        && desktop::sound_panel(w, self.desktop.now_playing.is_some()).contains(x, y));
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
                // A press away from the sound panel closes it.
                if pressed
                    && self.desktop.volume_open
                    && !desktop::sound_panel(w, self.desktop.now_playing.is_some()).contains(x, y)
                    && !desktop::volume_button(w).contains(x, y)
                {
                    self.desktop.volume_open = false;
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
                return button == 1 && pressed && self.click(x, y, w, h);
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
        if self.desktop.dialog.is_none() {
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
        if volume.is_none() {
            self.desktop.volume_open = false;
        }
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
            let mut terminal = Vec::new();
            for &byte in &keys[..count] {
                if self.desktop.dialog.is_some() && VolumeKey::of(byte).is_none() {
                    continue;
                }
                match self.windows.key(byte) {
                    KeyRoute::Terminal(byte) => terminal.push(byte),
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

    fn click(&mut self, x: i32, y: i32, width: i32, height: i32) -> bool {
        let mut windows: Vec<u32> = self.windows.frames().iter().map(|f| f.id).collect();
        windows.sort_unstable();
        let hit = self.desktop.hit(x, y, width, height, &windows);
        match hit {
            // The speaker and its panel (ADR-0100).
            Hit::Volume => {
                self.desktop.volume_open = !self.desktop.volume_open;
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
                        format_args!("desktop: {name} for {app}, from the sound panel"),
                    );
                }
                true
            }
            Hit::VolumePanel => false,
            Hit::Start => {
                self.desktop.start_open = !self.desktop.start_open;
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
            Hit::TaskbarWindow(id) => {
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
            Hit::Nothing => false,
        }
    }

    /// The Terminal, shown and given the keyboard.
    fn show_terminal(&mut self) {
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
        if apps.len() != self.desktop.apps.len() {
            say(
                self.log,
                format_args!("desktop: {} apps in the launcher", apps.len()),
            );
        }
        if changed {
            self.desktop.apps = apps;
        }
        changed
    }

    /// The clock and the status; `true` if they changed.
    fn refresh_clock(&mut self) -> bool {
        let clock = oceans_rt::unix_time_ms()
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
