//! Settings: the system's settings, an app that comes with the system
//! (ADR-0080, ADR-0081), built on the app toolkit.
//!
//! - **General:** the release, memory, uptime, the date and time where
//!   the user is, and the time zone (ADR-0143).
//! - **Network:** the addresses, the router, the DNS server, the hardware
//!   address (ADR-0096).
//! - **Sound:** the output and the input (ADR-0096).
//! - **Displays** (ADR-0136): the screen's resolution, and the interface's
//!   scale: automatic (as the screen's density suggests) or one that fits.
//! - **Apps:** the installed apps; for the one chosen, what it is, each
//!   permission and its decision (allow, deny, ask again), and removing
//!   it. Decisions are made through Core and audited as made in Settings.
//! - **Account** (ADR-0111): the user's name, and the password that locks
//!   the screen, set, changed or removed only with the current one.
//!
//! Its rights: `system-info`, and `manage-apps` (ADR-0081): a Core end
//! that may query, decide, manage and audit, given only to the system's
//! own apps; and `system-settings` (ADR-0096): reader ends of the network
//! and of the sound, which show their configuration and open nothing.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_abi::sysinfo::{self, KernelInfo, MemoryInfo};
use oceans_core_proto::{Core, CoreError, Decision, Status, decision, field, op, parts, source};
use oceans_net_proto::{Colons, Dotted};
use oceans_package::Permission;
use oceans_rt::{Directory, Handle, Start};
use oceans_ui::layout::Flow;
use oceans_ui::{Rect, Scroll, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 760;
const HEIGHT: u16 = 520;
/// The smallest it may be resized to (ADR-0137).
const MIN_SIZE: (u16, u16) = (600, 420);
/// The widest its pages are laid out: rows of facts stay readable in a
/// wide window.
const READABLE: i32 = 720;
const SECTIONS: [&str; 7] = [
    "General", "Network", "Sound", "Displays", "Apps", "Users", "Account",
];
/// The Account section's fields (ADR-0111).
const NAME_FIELD: u32 = 10;
const CURRENT_FIELD: u32 = 11;
const NEW_FIELD: u32 = 12;
const CONFIRM_FIELD: u32 = 13;
/// The Users section's fields (ADR-0115).
const LOGIN_FIELD: u32 = 20;
const USER_NAME_FIELD: u32 = 21;
const USER_PASSWORD_FIELD: u32 = 22;

/// The accounts (ADR-0115), as Core lists them, and a new one's fields.
#[derive(Default)]
struct Users {
    /// Id, name and flags (`a` administrator, `p` password, `c` in use).
    list: Vec<(String, String, String)>,
    login: String,
    name: String,
    password: String,
    message: String,
}

/// The account's page (ADR-0111): its name, and what is typed into its
/// fields, kept only until it is used.
#[derive(Default)]
struct Account {
    name: String,
    password_set: bool,
    /// Minutes without input before the screen locks (ADR-0112); 0: never.
    lock_after: u8,
    new_name: String,
    current: String,
    new: String,
    confirm: String,
    message: String,
}

struct App {
    id: String,
    name: String,
    version: String,
    running: bool,
}

/// What the chosen app asks for, and what was decided.
struct Detail {
    id: String,
    lines: Vec<(String, String)>,
    permissions: Vec<(u8, Decision)>,
}

struct Settings {
    core: Option<Core>,
    sysinfo: Option<Handle>,
    /// Reader ends (`system-settings`, ADR-0096).
    net: Option<Handle>,
    audio: Option<Handle>,
    section: usize,
    apps: Vec<App>,
    chosen: Option<usize>,
    detail: Option<Detail>,
    /// The apps (and the detail) are read again at the next frame.
    stale: bool,
    message: String,
    account: Account,
    users: Users,
    /// The appearance and accent (ADR-0113), as Core keeps them.
    appearance: (u8, u8),
    /// The scale chosen (ADR-0136), as Core keeps it: 0 automatic.
    scale: u16,
    /// How far down the page shown is scrolled (ADR-0138).
    page: Scroll,
    /// The time zone (ADR-0143), as Core keeps it: minutes ahead of UTC.
    time_zone: i16,
}

impl Settings {
    fn refresh(&mut self) {
        self.stale = false;
        self.apps.clear();
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 256];
        for index in 0u32..64 {
            match core.call(op::LIST, &index.to_le_bytes(), &[], &mut reply) {
                Ok(got) if got.len >= 1 => {
                    let mut fields = parts(&reply[1..got.len]);
                    let mut next = || fields.next().unwrap_or("?").to_string();
                    let (id, version, name) = (next(), next(), next());
                    self.apps.push(App {
                        id,
                        name,
                        version,
                        running: reply[0] != 0,
                    });
                }
                _ => break,
            }
        }
        // The account (ADR-0111).
        if let Ok(got) = core.call(op::USER, &[], &[], &mut reply)
            && got.len >= 1
        {
            self.account.password_set = reply[0] != 0;
            self.account.name =
                String::from(core::str::from_utf8(&reply[1..got.len]).unwrap_or("?"));
        }
        // The accounts (ADR-0115).
        let mut list = [0u8; 1024];
        if let Ok(got) = core.call(op::USERS, &[], &[], &mut list) {
            let text = core::str::from_utf8(&list[..got.len]).unwrap_or("");
            self.users.list = text
                .lines()
                .map(|line| {
                    let mut fields = line.split('\t').map(String::from);
                    let mut next = || fields.next().unwrap_or_default();
                    (next(), next(), next())
                })
                .collect();
        }
        if let Ok(got) = core.call(op::LOCK_AFTER, &[], &[], &mut reply)
            && got.len == 1
        {
            self.account.lock_after = reply[0];
        }
        if let Ok(got) = core.call(op::APPEARANCE, &[], &[], &mut reply)
            && got.len == 2
        {
            self.appearance = (reply[0], reply[1]);
        }
        if let Ok(got) = core.call(op::SCALE, &[], &[], &mut reply)
            && got.len == 2
        {
            self.scale = u16::from_le_bytes([reply[0], reply[1]]);
        }
        if let Ok(got) = core.call(op::TIME_ZONE, &[], &[], &mut reply)
            && got.len == 2
        {
            self.time_zone = i16::from_le_bytes([reply[0], reply[1]]);
        }
        self.chosen = self.chosen.filter(|&i| i < self.apps.len());
        self.detail = self
            .chosen
            .and_then(|i| self.apps.get(i))
            .map(|app| app.id.clone())
            .map(|id| detail(core, &id));
    }

    /// `DECIDE` for the chosen app, as made in Settings.
    fn decide(&mut self, permission: u8, choice: u8) {
        let (Some(core), Some(detail)) = (self.core, &self.detail) else {
            return;
        };
        let request = [permission, choice, source::SETTINGS];
        let mut reply = [0u8; 8];
        self.message = match core.about(op::DECIDE, &request, &detail.id, &mut reply) {
            Ok(_) => String::from("Saved."),
            Err((error, _)) => alloc::format!("Not saved: {}", error.message()),
        };
        self.stale = true;
    }

    /// `SET_NAME`: the account's new name.
    fn rename(&mut self) {
        let Some(core) = self.core else {
            return;
        };
        let name = self.account.new_name.trim().to_string();
        let mut reply = [0u8; 8];
        self.account.message = match core.call(op::SET_NAME, name.as_bytes(), &[], &mut reply) {
            Ok(_) => {
                self.account.new_name.clear();
                alloc::format!("Named {name}.")
            }
            Err(_) => String::from("A name has 1 to 32 characters."),
        };
        self.stale = true;
    }

    /// `ADD_USER` (ADR-0115). The password typed is forgotten whatever
    /// happens.
    fn add_user(&mut self) {
        let Some(core) = self.core else {
            return;
        };
        let login = self.users.login.trim().to_string();
        let name = self.users.name.trim().to_string();
        let password = core::mem::take(&mut self.users.password);
        let mut data = Vec::with_capacity(2 + login.len() + name.len() + password.len());
        data.push(login.len().min(255) as u8);
        data.extend_from_slice(login.as_bytes());
        data.push(name.len().min(255) as u8);
        data.extend_from_slice(name.as_bytes());
        data.extend_from_slice(password.as_bytes());
        let mut reply = [0u8; 8];
        self.users.message = match core.call(op::ADD_USER, &data, &[], &mut reply) {
            Ok(_) => {
                self.users.login.clear();
                self.users.name.clear();
                alloc::format!("Added {name}: the lock screen offers them from now on.")
            }
            Err((CoreError::Status(Status::BadRequest), _)) => String::from(
                "A login has 1 to 16 of a-z and 0-9, a letter first, not taken; \
                 a name 1 to 32 characters; a password 4 to 64.",
            ),
            Err((error, _)) => alloc::format!("Not added: {}", error.message()),
        };
        self.stale = true;
    }

    /// `REMOVE_USER` (ADR-0115): the account goes, its files stay.
    fn remove_user(&mut self, id: &str) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        self.users.message = match core.call(op::REMOVE_USER, id.as_bytes(), &[], &mut reply) {
            Ok(_) => alloc::format!("Removed {id}; its files stay."),
            Err((error, _)) => alloc::format!("Not removed: {}", error.message()),
        };
        self.stale = true;
    }

    /// `SET_APPEARANCE` (ADR-0113): Core keeps it, the desktop and every
    /// window follow; this window at once.
    fn set_appearance(&mut self, dark: u8, accent: u8) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        match core.call(op::SET_APPEARANCE, &[dark, accent], &[], &mut reply) {
            Ok(_) => {
                self.appearance = (dark, accent);
                oceans_ui::theme::set(oceans_ui::theme::Appearance::from_byte(dark), accent);
            }
            Err((error, _)) => {
                self.message = alloc::format!("Not saved: {}", error.message());
            }
        }
    }

    /// `SET_SCALE` (ADR-0136): Core keeps it; the desktop takes it up
    /// within a moment, and every window, this one too, is drawn again at
    /// it.
    fn set_scale(&mut self, percent: u16) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        match core.call(op::SET_SCALE, &percent.to_le_bytes(), &[], &mut reply) {
            Ok(_) => {
                self.scale = percent;
                self.message.clear();
            }
            Err((error, _)) => {
                self.message = alloc::format!("Not saved: {}", error.message());
            }
        }
    }

    /// `SET_TIME_ZONE` (ADR-0143): Core keeps it; the desktop's clock
    /// follows within a moment.
    fn set_time_zone(&mut self, minutes: i16) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        match core.call(op::SET_TIME_ZONE, &minutes.to_le_bytes(), &[], &mut reply) {
            Ok(_) => {
                self.time_zone = minutes;
                self.message.clear();
            }
            Err((error, _)) => {
                self.message = alloc::format!("Not saved: {}", error.message());
            }
        }
    }

    /// `SET_LOCK_AFTER` (ADR-0112).
    fn set_lock_after(&mut self, minutes: u8) {
        let Some(core) = self.core else {
            return;
        };
        let mut reply = [0u8; 8];
        self.account.message = match core.call(op::SET_LOCK_AFTER, &[minutes], &[], &mut reply) {
            Ok(_) if minutes == 0 => String::from("The screen never locks by itself."),
            Ok(_) => alloc::format!(
                "The screen locks after {minutes} minute{} without input.",
                if minutes == 1 { "" } else { "s" }
            ),
            Err((error, _)) => alloc::format!("Not saved: {}", error.message()),
        };
        self.stale = true;
    }

    /// `CHANGE_PASSWORD` (ADR-0111): with the current password; `remove`,
    /// to none. What was typed is forgotten whatever happens.
    fn change_password(&mut self, remove: bool) {
        let Some(core) = self.core else {
            return;
        };
        let current = core::mem::take(&mut self.account.current);
        let new = core::mem::take(&mut self.account.new);
        let confirm = core::mem::take(&mut self.account.confirm);
        let new = if remove { String::new() } else { new };
        if !remove && new != confirm {
            self.account.message = String::from("The two new passwords are not the same.");
            return;
        }
        if !remove && !(4..=64).contains(&new.len()) {
            self.account.message = String::from("A password has 4 to 64 characters.");
            return;
        }
        let mut data = Vec::with_capacity(1 + current.len() + new.len());
        data.push(current.len() as u8);
        data.extend_from_slice(current.as_bytes());
        data.extend_from_slice(new.as_bytes());
        let mut reply = [0u8; 16];
        self.account.message = match core.call(op::CHANGE_PASSWORD, &data, &[], &mut reply) {
            Ok(_) if remove => String::from("Password removed: the screen is not locked."),
            Ok(_) => String::from("Password changed: it unlocks the screen from now on."),
            Err((CoreError::Status(Status::WrongPassword), _)) => {
                String::from("The current password is not right.")
            }
            Err((CoreError::Status(Status::TooSoon), got)) => alloc::format!(
                "Too many wrong passwords: wait {} s.",
                core::str::from_utf8(&reply[..got.min(reply.len())]).unwrap_or("?")
            ),
            Err((error, _)) => alloc::format!("Not changed: {}", error.message()),
        };
        self.stale = true;
    }

    fn remove(&mut self) {
        let (Some(core), Some(detail)) = (self.core, &self.detail) else {
            return;
        };
        let id = detail.id.clone();
        // Not keeping its data (`[0]`).
        let mut reply = [0u8; 64];
        self.message = match core.about(op::REMOVE, &[0], &id, &mut reply) {
            Ok(_) => alloc::format!("Removed {id}."),
            Err((error, _)) => alloc::format!("Not removed: {}", error.message()),
        };
        self.chosen = None;
        self.detail = None;
        self.stale = true;
    }
}

/// Everything shown about app `id`.
fn detail(core: Core, id: &str) -> Detail {
    let field = |which| {
        let mut reply = [0u8; 160];
        core.about(op::INFO, &[which], id, &mut reply)
            .ok()
            .map(|got| String::from(core::str::from_utf8(&reply[..got.len]).unwrap_or("?")))
            .unwrap_or_default()
    };
    let lines = [
        ("Version", field::VERSION),
        ("Publisher", field::PUBLISHER),
        ("Kind", field::KIND),
        ("State", field::STATE),
        ("Key", field::KEY),
    ]
    .into_iter()
    .map(|(name, which)| (name.to_string(), field(which)))
    .chain([("Id".to_string(), id.to_string())])
    .collect();
    let mut permissions = Vec::new();
    let mut reply = [0u8; 160];
    for index in 0u8..16 {
        match core.about(op::PERMISSION, &[index], id, &mut reply) {
            Ok(got) if got.len >= 2 => {
                if let Some(state) = Decision::from_byte(reply[1]) {
                    permissions.push((reply[0], state));
                }
            }
            _ => break,
        }
    }
    Detail {
        id: id.to_string(),
        lines,
        permissions,
    }
}

fn general(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    ui.heading("General");
    ui.space(6);
    let mut line = String::new();
    if let Some(sysinfo) = settings.sysinfo {
        let mut record = [0u8; KernelInfo::SIZE];
        if let Some(kernel) = oceans_rt::system_info(sysinfo, sysinfo::KERNEL, &mut record)
            .ok()
            .and_then(|len| KernelInfo::decode(&record[..len]))
        {
            let _ = write!(
                line,
                "Oceans {} ({})",
                sysinfo::text(&kernel.version),
                sysinfo::text(&kernel.arch)
            );
            ui.row("System", &line);
            line.clear();
            let _ = write!(line, "{}", kernel.abi_version);
            ui.row("System call ABI", &line);
            line.clear();
        }
        let mut record = [0u8; MemoryInfo::SIZE];
        if let Some(memory) = oceans_rt::system_info(sysinfo, sysinfo::MEMORY, &mut record)
            .ok()
            .and_then(|len| MemoryInfo::decode(&record[..len]))
        {
            let mib = |frames: u64| (frames * memory.page_size) >> 20;
            let _ = write!(
                line,
                "{} MiB free of {} MiB",
                mib(memory.free_frames),
                mib(memory.total_frames)
            );
            ui.row("Memory", &line);
            line.clear();
        }
    } else {
        ui.muted("System information is not available to Settings.");
    }
    let seconds = oceans_rt::clock_ms() / 1000;
    let _ = write!(line, "{} h {} min", seconds / 3600, seconds / 60 % 60);
    ui.row("Up for", &line);
    line.clear();
    // The date and time where the user is (ADR-0143), and the time zone.
    let zone = settings.time_zone;
    match oceans_rt::unix_time_ms() {
        Some(ms) => {
            let ms = ms.saturating_add_signed(i64::from(zone) * 60_000);
            let (year, month, day) = oceans_package::date::civil((ms / 86_400_000) as u32);
            let minutes = ms / 60_000;
            let _ = write!(
                line,
                "{year}-{month:02}-{day:02} {:02}:{:02}",
                minutes / 60 % 24,
                minutes % 60
            );
        }
        None => line.push_str("not set"),
    }
    ui.row("Date and time", &line);
    line.clear();
    let index = oceans_core_proto::TIME_ZONES
        .iter()
        .position(|&(offset, _)| offset == zone);
    let _ = write!(line, "{}", ZoneName(zone));
    if let Some(i) = index {
        let _ = write!(line, "  {}", oceans_core_proto::TIME_ZONES[i].1);
    }
    ui.row("Time zone", &line);
    if settings.core.is_some() {
        let (x, y) = (ui.area.x, ui.y);
        let mut chosen = None;
        let last = oceans_core_proto::TIME_ZONES.len() - 1;
        let at = index.unwrap_or(7);
        if ui.button_in(Rect::new(x, y, 120, 30), "West", false) && at > 0 {
            chosen = Some(at - 1);
        }
        if ui.button_in(Rect::new(x + 128, y, 120, 30), "East", false) && at < last {
            chosen = Some(at + 1);
        }
        ui.y = y + 38;
        if let Some(i) = chosen {
            settings.set_time_zone(oceans_core_proto::TIME_ZONES[i].0);
        }
    }
    // The appearance (ADR-0113): light or dark, and the accent.
    if settings.core.is_none() {
        return;
    }
    ui.space(10);
    ui.heading("Appearance");
    let (dark, accent) = settings.appearance;
    let y = ui.y;
    let x = ui.area.x;
    let mut chosen = None;
    for (i, label) in ["Light", "Dark"].into_iter().enumerate() {
        let r = Rect::new(x + i as i32 * 96, y, 88, 30);
        if ui.button_in(r, label, usize::from(dark) == i) {
            chosen = Some((i as u8, accent));
        }
    }
    ui.y = y + 38;
    ui.label("Accent:");
    // On as many lines as the width needs (ADR-0137), room under each
    // for its colour.
    let mut flow = Flow::new(Rect::new(x, ui.y, ui.area.w, 0), 30, 6).line_gap(16);
    for (i, &(name, rgb)) in oceans_ui::theme::ACCENTS.iter().enumerate() {
        // A button by the accent's name (the keyboard reaches it,
        // ADR-0106), the one in use filled; the colour itself under it.
        let r = flow.next(76);
        if ui.button_in(r, name, usize::from(accent) == i) {
            chosen = Some((dark, i as u8));
        }
        ui.surface
            .round_fill(Rect::new(r.x + 8, r.y + r.h + 4, r.w - 16, 4), 2, rgb);
    }
    ui.y = flow.bottom() + 16;
    if let Some((dark, accent)) = chosen {
        settings.set_appearance(dark, accent);
    }
}

/// The screen (ADR-0136): its resolution, and the interface's scale, as
/// the desktop draws it. Automatic follows the screen's density; the
/// others are offered only where the desktop still fits.
fn displays(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    ui.heading("Displays");
    ui.space(6);
    let Some(screen) = ui.screen() else {
        ui.muted("The screen's settings are not available to Settings.");
        return;
    };
    let (w, h) = (i32::from(screen.width), i32::from(screen.height));
    let mut line = String::new();
    let _ = write!(line, "{w} × {h}");
    ui.row("Resolution", &line);
    line.clear();
    let scale = screen.scale;
    let _ = write!(
        line,
        "{}% ({}), looks like {} × {}",
        scale.percent(),
        if settings.scale == 0 {
            "automatic"
        } else {
            "chosen"
        },
        scale.logical_fit(w),
        scale.logical_fit(h)
    );
    ui.row("Scale", &line);
    line.clear();
    if settings.core.is_none() {
        return;
    }
    ui.space(10);
    ui.heading("Interface size");
    ui.muted("Larger makes text and controls bigger; smaller fits more on the screen.");
    let x = ui.area.x;
    let mut chosen = None;
    let _ = write!(line, "Automatic ({}%)", screen.automatic.percent());
    let y = ui.y;
    if ui.button_in(Rect::new(x, y, 168, 30), &line, settings.scale == 0) {
        chosen = Some(0);
    }
    // The choices, as many to a row as fit.
    let (step, width) = (80, 72);
    let per_row = (ui.area.w / step).max(1);
    let mut row_y = y + 40;
    for (i, choice) in screen.choices().enumerate() {
        let i = i as i32;
        if i > 0 && i % per_row == 0 {
            row_y += 38;
        }
        line.clear();
        let _ = write!(line, "{}%", choice.percent());
        let r = Rect::new(x + (i % per_row) * step, row_y, width, 30);
        if ui.button_in(r, &line, settings.scale == choice.percent()) {
            chosen = Some(choice.percent());
        }
    }
    ui.y = row_y + 42;
    if !settings.message.is_empty() {
        let message = settings.message.clone();
        ui.muted(&message);
    }
    if let Some(percent) = chosen
        && percent != settings.scale
    {
        settings.set_scale(percent);
    }
}

/// The network's configuration (ADR-0096), through a reader end of the
/// stack: it shows, and opens nothing.
fn network(ui: &mut Ui<'_, '_>, settings: &Settings) {
    ui.heading("Network");
    ui.space(6);
    let Some(net) = settings.net else {
        ui.muted("The network's settings are not available to Settings.");
        return;
    };
    let info = match oceans_net_proto::info(net) {
        Ok(info) => info,
        Err(error) => {
            ui.muted(error.message());
            return;
        }
    };
    let mut line = String::new();
    if info.configured {
        ui.row("Status", "connected");
        let _ = write!(line, "{}/{}", Dotted(info.address), info.prefix);
        ui.row("IPv4 address", &line);
        line.clear();
        if info.gateway != [0; 4] {
            let _ = write!(line, "{}", Dotted(info.gateway));
            ui.row("Router", &line);
            line.clear();
        }
        if info.dns != [0; 4] {
            let _ = write!(line, "{}", Dotted(info.dns));
            ui.row("DNS server", &line);
            line.clear();
        }
    } else {
        ui.row("Status", "no address yet (asking DHCP)");
    }
    // A stack without IPv6 does not know the request.
    if let Ok(info6) = oceans_net_proto::info6(net)
        && info6.enabled
    {
        for address in info6.addresses() {
            let _ = write!(line, "{}/{}", Colons(address.address), address.prefix);
            let name = if address.link_local {
                "IPv6 (link-local)"
            } else {
                "IPv6 address"
            };
            ui.row(name, &line);
            line.clear();
        }
        if let Some(router) = info6.router {
            let _ = write!(line, "{}", Colons(router));
            ui.row("IPv6 router", &line);
            line.clear();
        }
    }
    let [a, b, c, d, e, f] = info.mac;
    let _ = write!(line, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}");
    ui.row("Hardware address", &line);
}

/// The sound's output and input (ADR-0096), through a reader end of the
/// audio service: it names them, and plays and records nothing.
fn sound(ui: &mut Ui<'_, '_>, settings: &Settings) {
    ui.heading("Sound");
    ui.space(6);
    let Some(audio) = settings.audio else {
        ui.muted("The sound's settings are not available to Settings.");
        return;
    };
    let mut text = [0u8; 160];
    for (name, read) in [
        (
            "Output",
            oceans_audio_proto::info as fn(Handle, &mut [u8]) -> _,
        ),
        ("Input", oceans_audio_proto::input_info),
    ] {
        let value = match read(audio, &mut text) {
            Ok(len) => core::str::from_utf8(&text[..len]).unwrap_or("?"),
            Err(_) => "none",
        };
        // "Intel HDA, codec …, line out (…), 48 kHz stereo": the device,
        // then what it is, one row each.
        let mut parts = value.splitn(2, ", ");
        ui.row(name, parts.next().unwrap_or(value));
        if let Some(rest) = parts.next() {
            ui.muted(rest);
        }
    }
    ui.space(6);
    ui.muted("Sound plays at 48 kHz, 16-bit stereo; apps that may play it are in Apps.");
}

/// The account (ADR-0111): its name, and the password that locks the
/// screen, changed only with the current one.
fn account(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    ui.heading("Account");
    if settings.core.is_none() {
        ui.muted("The account is not available to Settings.");
        return;
    }
    let account = &mut settings.account;
    ui.row("Name", &account.name);
    let renamed = ui.text_field(NAME_FIELD, &mut account.new_name, "A new name");
    let rename = ui.button("Rename") || renamed;
    ui.separator();
    ui.label(if account.password_set {
        "A password locks the screen at start and with Super+L."
    } else {
        "No password: the screen is never locked."
    });
    if account.password_set {
        ui.password_field(CURRENT_FIELD, &mut account.current, "Current password");
    }
    ui.password_field(NEW_FIELD, &mut account.new, "New password");
    let entered = ui.password_field(CONFIRM_FIELD, &mut account.confirm, "New password again");
    let label = if account.password_set {
        "Change Password"
    } else {
        "Set Password"
    };
    // Change and Remove side by side, to fit the window.
    let y = ui.y;
    let x = ui.area.x;
    let change_w = ui.measure(label, Style::Strong) + 28;
    let change = ui.button_in(Rect::new(x, y, change_w, 30), label, true) || entered;
    let remove_w = ui.measure("Remove Password", Style::Strong) + 28;
    let remove = account.password_set
        && ui.button_in(
            Rect::new(x + change_w + 8, y, remove_w, 30),
            "Remove Password",
            false,
        );
    ui.y = y + 38;
    // Locking after a while without input (ADR-0112), with a password.
    let mut lock_after = None;
    if account.password_set {
        ui.label("Lock the screen after this many minutes without input:");
        // On as many lines as the width needs (ADR-0137).
        let mut flow = Flow::new(Rect::new(ui.area.x, ui.y, ui.area.w, 0), 30, 6);
        for &minutes in &oceans_core_proto::LOCK_CHOICES {
            let text = if minutes == 0 {
                String::from("Never")
            } else {
                alloc::format!("{minutes}")
            };
            let w = if minutes == 0 { 68 } else { 44 };
            if ui.button_in(flow.next(w), &text, minutes == account.lock_after) {
                lock_after = Some(minutes);
            }
        }
        ui.y = flow.bottom() + 8;
    }
    if !account.message.is_empty() {
        let message = account.message.clone();
        ui.muted(&message);
    }
    if rename && !settings.account.new_name.trim().is_empty() {
        settings.rename();
    }
    if change {
        settings.change_password(false);
    } else if remove {
        settings.change_password(true);
    }
    if let Some(minutes) = lock_after {
        settings.set_lock_after(minutes);
    }
}

/// The accounts (ADR-0115): who may use this computer. The administrator
/// adds and removes the others; each has its own Home.
fn users(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    ui.heading("Users");
    if settings.core.is_none() {
        ui.muted("The accounts are not available to Settings.");
        return;
    }
    let admin = settings
        .users
        .list
        .iter()
        .any(|(_, _, flags)| flags.contains('a') && flags.contains('c'));
    let mut remove = None;
    for (id, name, flags) in &settings.users.list {
        let mut kind = String::from(if flags.contains('a') {
            "Administrator"
        } else {
            "Standard"
        });
        if flags.contains('c') {
            kind.push_str(", in use");
        }
        ui.row(name, &kind);
        if admin && !flags.contains('a') && !flags.contains('c') {
            let label = alloc::format!("Remove {name}");
            if ui.button(&label) {
                remove = Some(id.clone());
            }
        }
    }
    ui.separator();
    let mut add = false;
    if admin {
        ui.label("Add a user, with a password; their files are their own.");
        let users = &mut settings.users;
        ui.text_field(LOGIN_FIELD, &mut users.login, "Login (a-z, 0-9)");
        ui.text_field(USER_NAME_FIELD, &mut users.name, "Name");
        let entered = ui.password_field(USER_PASSWORD_FIELD, &mut users.password, "Password");
        add = ui.button("Add User") || entered;
    } else {
        ui.muted("Only the administrator adds and removes users.");
    }
    if !settings.users.message.is_empty() {
        let message = settings.users.message.clone();
        ui.muted(&message);
    }
    if add {
        settings.add_user();
    }
    if let Some(id) = remove {
        settings.remove_user(&id);
    }
}

fn apps(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    ui.heading("Apps");
    if settings.core.is_none() {
        ui.muted("Settings may not manage apps on this system.");
        return;
    }
    // The list on the left, the chosen app on the right.
    let whole = ui.area;
    let top = ui.y;
    // Half of what the margins leave, 210 at the size the window opens at.
    let list_width = ((whole.w - 112) / 2).clamp(160, 320);
    ui.area = Rect::new(whole.x, top, list_width, whole.h - (top - whole.y));
    let names: Vec<String> = settings
        .apps
        .iter()
        .map(|app| {
            if app.running {
                alloc::format!("{}  (running)", app.name)
            } else {
                app.name.clone()
            }
        })
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    if let Some(index) = ui.list(&names, settings.chosen) {
        settings.chosen = Some(index);
        settings.message.clear();
        settings.stale = true;
    }
    if settings.apps.is_empty() {
        ui.muted("No apps installed.");
    }

    ui.area = Rect::new(
        whole.x + list_width + 24,
        top,
        whole.w - list_width - 24,
        whole.h - (top - whole.y),
    );
    // The page is as long as the longer column (ADR-0138).
    let list_end = ui.y;
    ui.y = top;
    let Some(detail) = settings.detail.take() else {
        ui.muted("Choose an app.");
        ui.y = ui.y.max(list_end);
        return;
    };
    if let Some(app) = settings.chosen.and_then(|i| settings.apps.get(i)) {
        let title = alloc::format!("{} {}", app.name, app.version);
        let r = Rect::new(ui.area.x, ui.y, ui.area.w, 24);
        ui.text_at(r.x, r.y, &title, Style::Strong, colour::text(), r);
        ui.space(28);
    }
    for (name, value) in &detail.lines {
        ui.row(name, value);
    }
    ui.space(8);
    ui.label("Permissions");
    if detail.permissions.is_empty() {
        ui.muted("It asks for none.");
    }
    let mut decided = None;
    for &(permission, state) in &detail.permissions {
        let name = Permission::ALL
            .get(usize::from(permission))
            .map_or("?", |p| p.name());
        let shown = match state {
            Decision::Automatic => "allowed (automatic)",
            Decision::Allowed => "allowed",
            Decision::Denied => "denied",
            Decision::Undecided => "asks at the next start",
        };
        // The name and what was decided, then the three choices: on the
        // same line where the width allows, else under them (ADR-0137),
        // so they never cover the text.
        let row = Rect::new(ui.area.x, ui.y, ui.area.w, 30);
        let choices = 3 * 66 - 6;
        let wide = row.w >= 130 + 170 + 12 + choices;
        let text_w = if wide { row.w - choices - 12 } else { row.w };
        let name_clip = Rect::new(row.x, row.y, text_w.min(124), row.h);
        ui.text_at(
            row.x,
            row.y + 6,
            name,
            Style::Body,
            colour::text(),
            name_clip,
        );
        let shown_clip = Rect::new(row.x + 130, row.y, (text_w - 130).max(0), row.h);
        ui.text_at(
            row.x + 130,
            row.y + 6,
            shown,
            Style::Body,
            colour::muted(),
            shown_clip,
        );
        if state != Decision::Automatic {
            let buttons = [
                ("Allow", decision::ALLOW),
                ("Deny", decision::DENY),
                ("Ask", decision::FORGET),
            ];
            let line = if wide { row.y } else { row.y + 32 };
            let left = if wide { row.x + row.w - choices } else { row.x };
            for (i, (label, choice)) in buttons.into_iter().enumerate() {
                let r = Rect::new(left + i as i32 * 66, line, 60, 28);
                if ui.button_in(r, label, false) {
                    decided = Some((permission, choice));
                }
            }
            if !wide {
                ui.y += 32;
            }
        }
        ui.y += 34;
    }
    ui.space(10);
    let remove = ui.button("Remove this app");
    if !settings.message.is_empty() {
        ui.muted(&settings.message);
    }
    ui.y = ui.y.max(list_end);
    settings.detail = Some(detail);
    if let Some((permission, choice)) = decided {
        settings.decide(permission, choice);
    }
    if remove {
        settings.remove();
    }
}

fn frame(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    if settings.stale {
        settings.refresh();
    }
    ui.background(colour::window());
    if let Some(section) = ui.sidebar(180, &SECTIONS, settings.section) {
        settings.section = section;
        settings.stale = true;
        settings.page.reset();
    }
    ui.readable(READABLE);
    // The page scrolls when it is longer than the window (ADR-0138).
    let view = ui.area;
    let mut page = settings.page;
    let scrolled = ui.begin_scroll(view, &page);
    match settings.section {
        0 => general(ui, settings),
        1 => network(ui, settings),
        2 => sound(ui, settings),
        3 => displays(ui, settings),
        4 => apps(ui, settings),
        5 => users(ui, settings),
        _ => account(ui, settings),
    }
    ui.end_scroll(scrolled, &mut page);
    settings.page = page;
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut settings = Settings {
        core: directory.find("use", "core").map(Core),
        sysinfo: directory.find("sysinfo", "sysinfo"),
        net: directory.find("use", "net-info"),
        audio: directory.find("use", "audio-info"),
        section: 0,
        apps: Vec::new(),
        chosen: None,
        detail: None,
        stale: true,
        message: String::new(),
        account: Account::default(),
        users: Users::default(),
        appearance: (0, 0),
        scale: 0,
        page: Scroll::default(),
        time_zone: 0,
    };
    oceans_ui::run_resizable(
        &directory,
        "",
        WIDTH,
        HEIGHT,
        MIN_SIZE,
        None,
        &mut settings,
        frame,
    )
}

/// A time zone as `UTC+07:00` (ADR-0143).
struct ZoneName(i16);

impl core::fmt::Display for ZoneName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let minutes = self.0.unsigned_abs();
        write!(
            f,
            "UTC{}{:02}:{:02}",
            if self.0 < 0 { '-' } else { '+' },
            minutes / 60,
            minutes % 60
        )
    }
}
