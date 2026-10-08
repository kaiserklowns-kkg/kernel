//! Settings: the system's settings, an app that comes with the system
//! (ADR-0080, ADR-0081), built on the app toolkit.
//!
//! - **General:** the release, memory, uptime, the date and time.
//! - **Network:** the addresses, the router, the DNS server, the hardware
//!   address (ADR-0096).
//! - **Sound:** the output and the input (ADR-0096).
//! - **Apps:** the installed apps; for the one chosen, what it is, each
//!   permission and its decision (allow, deny, ask again), and removing
//!   it. Decisions are made through Core and audited as made in Settings.
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
use oceans_core_proto::{Core, Decision, decision, field, op, parts, source};
use oceans_net_proto::{Colons, Dotted};
use oceans_package::Permission;
use oceans_rt::{Directory, Handle, Start};
use oceans_ui::{Rect, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 760;
const HEIGHT: u16 = 520;
const SECTIONS: [&str; 4] = ["General", "Network", "Sound", "Apps"];

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

fn general(ui: &mut Ui<'_, '_>, settings: &Settings) {
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
    match oceans_rt::unix_time_ms() {
        Some(ms) => {
            let (year, month, day) = oceans_package::date::civil((ms / 86_400_000) as u32);
            let minutes = ms / 60_000;
            let _ = write!(
                line,
                "{year}-{month:02}-{day:02} {:02}:{:02} UTC",
                minutes / 60 % 24,
                minutes % 60
            );
        }
        None => line.push_str("not set"),
    }
    ui.row("Date and time", &line);
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

fn apps(ui: &mut Ui<'_, '_>, settings: &mut Settings) {
    ui.heading("Apps");
    if settings.core.is_none() {
        ui.muted("Settings may not manage apps on this system.");
        return;
    }
    // The list on the left, the chosen app on the right.
    let whole = ui.area;
    let top = ui.y;
    let list_width = 210;
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
    ui.y = top;
    let Some(detail) = settings.detail.take() else {
        ui.muted("Choose an app.");
        return;
    };
    if let Some(app) = settings.chosen.and_then(|i| settings.apps.get(i)) {
        let title = alloc::format!("{} {}", app.name, app.version);
        let r = Rect::new(ui.area.x, ui.y, ui.area.w, 24);
        ui.text_at(r.x, r.y, &title, Style::Strong, colour::TEXT, r);
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
        let row = Rect::new(ui.area.x, ui.y, ui.area.w, 30);
        ui.text_at(row.x, row.y + 6, name, Style::Body, colour::TEXT, row);
        ui.text_at(
            row.x + 130,
            row.y + 6,
            shown,
            Style::Body,
            colour::MUTED,
            row,
        );
        if state != Decision::Automatic {
            let buttons = [
                ("Allow", decision::ALLOW),
                ("Deny", decision::DENY),
                ("Ask", decision::FORGET),
            ];
            for (i, (label, choice)) in buttons.into_iter().enumerate() {
                let r = Rect::new(row.x + row.w - 3 * 66 + i as i32 * 66, row.y, 60, 28);
                if ui.button_in(r, label, false) {
                    decided = Some((permission, choice));
                }
            }
        }
        ui.y += 34;
    }
    ui.space(10);
    let remove = ui.button("Remove this app");
    if !settings.message.is_empty() {
        ui.muted(&settings.message);
    }
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
    ui.background(colour::WINDOW);
    if let Some(section) = ui.sidebar(180, &SECTIONS, settings.section) {
        settings.section = section;
        settings.stale = true;
    }
    match settings.section {
        0 => general(ui, settings),
        1 => network(ui, settings),
        2 => sound(ui, settings),
        _ => apps(ui, settings),
    }
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
    };
    oceans_ui::run(&directory, "", WIDTH, HEIGHT, &mut settings, frame)
}
