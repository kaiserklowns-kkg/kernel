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
//! - notifications;
//! - **permission dialogs**: when an app needs a decision, the desktop
//!   asks, in the system's words, and sends the answer to Core
//!   (`source::DIALOG`). Permission UI is system-rendered, never a web view
//!   (ADR-0056).
//!
//! The pointer comes from the input service (ADR-0042). If this service
//! ends, the kernel console takes the screen back.
//!
//! Grants: `log`, `display`, `console-input` (to hand keys to the
//! console), `use = core` (the user's agent, like the shell), `use =
//! input`, `sysinfo`.

#![no_std]
#![no_main]

extern crate alloc;

mod canvas;
mod desktop;
mod text;
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
use oceans_window::{Focus, KeyRoute, Manager};

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
            ..Desktop::default()
        },
        pending: Vec::new(),
        text_capacity: 16 + info.cols as usize * info.rows as usize,
        console,
        windows: Manager::new(area, 2 * screen_pixels),
        owners: BTreeMap::new(),
        pixels: BTreeMap::new(),
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
                    dirty |= service.pointer(event.kind, &canvas);
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
        let focus = self.windows.focus();
        self.desktop.terminal_focused = focus == Focus::Terminal;
        let views: Vec<WindowView<'_>> = self
            .windows
            .frames()
            .iter()
            .map(|frame| WindowView {
                frame,
                pixels: self.pixels.get(&frame.id).map(Pixels::pixels),
                focused: focus == Focus::Window(frame.id),
            })
            .collect();
        self.desktop.draw(canvas, oceans_rt::clock_ms(), &views);
        canvas.present();
    }

    /// A pointer event; `true` if the screen must change.
    fn pointer(&mut self, kind: Kind, canvas: &Canvas) -> bool {
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
                // A permission dialog is modal: windows get nothing. The
                // taskbar and an open Start menu lie over the windows, but
                // a release always reaches them (it ends a drag).
                let over = self.desktop.start_open || desktop::taskbar(w, h).contains(x, y);
                if self.desktop.dialog.is_none()
                    && (!pressed || !over)
                    && self.windows.button(button, pressed, x, y)
                {
                    return true;
                }
                return button == 1 && pressed && self.click(x, y, w, h);
            }
            _ => return false,
        }
        if self.desktop.dialog.is_none() {
            let (x, y) = self.desktop.pointer;
            self.windows.pointer_moved(x, y);
        }
        true
    }

    /// Keys from the keyboard (`DISPLAY_KEYS`): to the focused window, or
    /// to the console for the Terminal. While a permission dialog asks,
    /// they go nowhere: typing must not land anywhere unseen.
    fn keys(&mut self) -> bool {
        let mut keys = [0u8; 256];
        let mut dirty = false;
        loop {
            let count = match oceans_rt::display_keys(self.display, &mut keys) {
                Ok(0) | Err(_) => return dirty,
                Ok(count) => count,
            };
            if self.desktop.dialog.is_some() {
                continue;
            }
            let mut terminal = Vec::new();
            for &byte in &keys[..count] {
                match self.windows.key(byte) {
                    KeyRoute::Terminal(byte) => terminal.push(byte),
                    KeyRoute::Window(_) => {}
                    KeyRoute::Consumed => dirty = true,
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
        match self.desktop.hit(x, y, width, height, &windows) {
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
                self.launch(&id);
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
    fn launch(&mut self, id: &str) {
        let Some(core) = self.core else {
            return self.toast("This desktop cannot start apps".to_string(), true);
        };
        let mut data = Vec::with_capacity(2 + id.len());
        data.push(run_flags::DETACH);
        data.push(id.len() as u8);
        data.extend_from_slice(id.as_bytes());
        let mut reply = [0u8; 64];
        match core.call(op::RUN, &data, &[], &mut reply) {
            Ok(_) => {
                say(self.log, format_args!("desktop: started {id}"));
                let name = self.name_of(id);
                self.toast(alloc::format!("Started {name}"), false);
                self.refresh_apps();
            }
            Err((CoreError::Status(Status::NeedsConsent), _)) => self.ask(core, id),
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
            return self.launch(id);
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
        let (Some(core), Some(dialog)) = (self.core, self.desktop.dialog.take()) else {
            return;
        };
        let (id, permission) = match dialog.question {
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

    /// The clock, the date and the status; `true` if they changed.
    fn refresh_clock(&mut self) -> bool {
        let (clock, date) = match oceans_rt::unix_time_ms() {
            Some(ms) => {
                let minutes = ms / 60_000;
                let (year, month, day) = oceans_package::date::civil((ms / 86_400_000) as u32);
                (
                    alloc::format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60),
                    alloc::format!("{year}-{month:02}-{day:02} UTC"),
                )
            }
            None => (String::from("--:--"), String::from("time not set")),
        };
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
        let changed = clock != self.desktop.clock
            || date != self.desktop.date
            || status != self.desktop.status;
        self.desktop.clock = clock;
        self.desktop.date = date;
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
