//! init: the first user process and the Oceans service manager (ADR-0016).
//!
//! 1. Maps the boot archive the kernel passes (handle 1, ADR-0025) and
//!    checks it; the manifest (`services.conf`) and every program image
//!    come from it. An image becomes a memory object only when a service
//!    needs it, once.
//! 2. Parses the manifest: services in start order, each with an image, a
//!    restart policy and the capabilities it is granted.
//! 3. Starts each service with exactly the declared capabilities, in the
//!    declared order: `grant = log` (a log capability), `grant = console`
//!    (the system console), `provide = NAME`
//!    (the server end of a new endpoint NAME), `use = NAME` (a client end
//!    of endpoint NAME, provided by an earlier service; `use = NAME as
//!    ALIAS` lists it as ALIAS, ADR-0035), `grant = devices`
//!    (the PCI device list, read-only), `grant = device:VVVV:DDDD` (the
//!    PCI function with that vendor and device ID, opened exclusively:
//!    what makes a service its driver, ADR-0021), `grant =
//!    device-class:CCSSPP` (the first function of that class, subclass
//!    and programming interface, for standard interfaces such as xHCI),
//!    `grant = console-input` (feeding the console's input and nothing
//!    else: keyboard drivers, ADR-0032).
//! 4. Supervises: one notification, a bit per service, signalled by the
//!    kernel when a service exits. Restart policies `always`, `on-failure`
//!    and `never`, with exponential backoff and a restart limit.
//! 5. **Stops the system** (ADR-0085). init serves the `power` endpoint
//!    (`grant = power`; the notification is bound to it): asked to switch
//!    off or restart, and if the machine can, it answers, stops every
//!    service in reverse start order (the filesystem's clients first),
//!    syncs the filesystem in between, stops the rest, and asks the kernel
//!    (`SYSTEM_POWER`, which needs init's `MANAGE` on the system object).
//!
//! In smoke-test mode (argument 1) init exits once every non-`always`
//! service has settled, or after stopping the system instead of switching
//! off, with 0 only if every `expect-exit` / `expect-runs` line of the
//! manifest holds.
//!
//! No allocator: everything is fixed-size, and strings borrow the mapped,
//! read-only archive, which stays mapped for init's lifetime.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write;

use oceans_archive::Archive;
use oceans_fs_proto::{Kind, Node, flags};

use oceans_rt::{Buffer, Error, Handle, Start, power, prot, rights};

oceans_rt::entry!(main);

const MANIFEST: &str = "services.conf";
const MAX_SERVICES: usize = 32;
/// Grants of one service. The file system holds one per program it
/// publishes in /bin, so this grows with the system's programs.
const MAX_GRANTS: usize = 48;
const MAX_ENDPOINTS: usize = 24;
const DEFAULT_MAX_RESTARTS: u32 = 5;
const BACKOFF_BASE_MS: u64 = 100;
const BACKOFF_MAX_MS: u64 = 2000;

/// Rights handed to services. `DUPLICATE` lets a service (e.g. the shell)
/// pass narrower copies on to programs it starts; it never widens rights.
const LOG_RIGHTS: u32 = rights::WRITE | rights::DUPLICATE | rights::TRANSFER;
const CONSOLE_RIGHTS: u32 = rights::READ | rights::WRITE | rights::DUPLICATE | rights::TRANSFER;
/// Input only: a keyboard driver can type but neither read nor print.
const CONSOLE_INPUT_RIGHTS: u32 = rights::MANAGE | rights::TRANSFER;
const SYSINFO_RIGHTS: u32 = rights::READ | rights::DUPLICATE | rights::TRANSFER;
const USE_RIGHTS: u32 = rights::SEND | rights::DUPLICATE | rights::TRANSFER;
const MODULE_RIGHTS: u32 = rights::READ | rights::MAP | rights::DUPLICATE | rights::TRANSFER;
/// The device list only: opening devices stays with init.
const DEVICES_RIGHTS: u32 = rights::READ | rights::DUPLICATE | rights::TRANSFER;

/// Exit codes of init itself (only reached in test mode, or on fatal
/// configuration errors).
const EXIT_BAD_START: i64 = 2;
const EXIT_BAD_MANIFEST: i64 = 3;
const EXIT_EXPECTATION_FAILED: i64 = 4;
const EXIT_SUPERVISION_FAILED: i64 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Restart {
    Always,
    OnFailure,
    Never,
}

#[derive(Clone, Copy)]
enum Grant {
    Log,
    /// Reading the kept log (ADR-0070).
    LogRead,
    Console,
    SystemInfo,
    Devices,
    /// The PCI function with this vendor and device ID; the text is the
    /// manifest's `VVVV:DDDD`.
    Device(u16, u16, &'static str),
    /// The first PCI function of this class (`0xCCSSPP`); the text is the
    /// manifest's `CCSSPP`.
    DeviceClass(u32, &'static str),
    ConsoleInput,
    /// The screen (ADR-0057): its geometry, the console's text, and taking
    /// the framebuffer over.
    Display,
    Provide(&'static str),
    /// An endpoint: the manifest's `NAME` or `NAME as ALIAS` (named
    /// `ALIAS` in the service's directory); see [`use_parts`]. One string,
    /// so a grant stays small (init's tables live on its stack).
    Use(&'static str),
    Module(&'static str),
    /// A directory of the filesystem (`storage:/PATH`, ADR-0053), created
    /// if missing, opened writable and handed over as `use storage`: the
    /// service's own files and nothing else.
    Storage(&'static str),
    /// Asking init to switch the machine off or restart it (ADR-0085).
    Power,
}

#[derive(Clone, Copy)]
struct Service {
    name: &'static str,
    image: Option<&'static str>,
    restart: Restart,
    max_restarts: u32,
    grants: [Option<Grant>; MAX_GRANTS],
    expect_exit: Option<i64>,
    expect_runs: Option<u32>,
    // Runtime state.
    process: Option<Handle>,
    runs: u32,
    restarts: u32,
    last_exit: Option<i64>,
    settled: bool,
}

impl Service {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            image: None,
            restart: Restart::Never,
            max_restarts: DEFAULT_MAX_RESTARTS,
            grants: [None; MAX_GRANTS],
            expect_exit: None,
            expect_runs: None,
            process: None,
            runs: 0,
            restarts: 0,
            last_exit: None,
            settled: false,
        }
    }
}

const NO_SERVICE: Service = Service::new("");

/// Client ends of endpoints provided by services, by name.
struct Registry {
    names: [&'static str; MAX_ENDPOINTS],
    clients: [Handle; MAX_ENDPOINTS],
    len: usize,
}

impl Registry {
    fn get(&self, name: &str) -> Option<Handle> {
        self.names[..self.len]
            .iter()
            .position(|&n| n == name)
            .map(|i| self.clients[i])
    }

    /// Records (or replaces, after a provider restart) endpoint `name`.
    fn set(&mut self, name: &'static str, client: Handle) -> bool {
        if let Some(i) = self.names[..self.len].iter().position(|&n| n == name) {
            let _ = oceans_rt::close(self.clients[i]);
            self.clients[i] = client;
            return true;
        }
        if self.len == MAX_ENDPOINTS {
            return false;
        }
        self.names[self.len] = name;
        self.clients[self.len] = client;
        self.len += 1;
        true
    }
}

/// Programs unpacked from the archive at most.
const MAX_IMAGES: usize = 64;

struct Init {
    log: Handle,
    console: Handle,
    sysinfo: Handle,
    /// The PCI device bus.
    bus: Handle,
    /// The screen (handle 5, when there is one, ADR-0057).
    display: Option<Handle>,
    /// The boot archive, mapped for init's lifetime.
    archive: Archive<'static>,
    /// Archive files unpacked into memory objects so far.
    images: [(&'static str, Handle); MAX_IMAGES],
    image_count: usize,
    events: Handle,
    /// The client end of init's `power` endpoint, handed out by
    /// `grant = power`.
    power: Handle,
    /// On the heap: `MAX_SERVICES` of them no longer fit on the stack.
    services: Vec<Service>,
    count: usize,
    registry: Registry,
}

fn main(start: Start) -> i64 {
    let test_mode = start.arg == 1;
    // Boot contract (kernel process::init): log, boot archive, console,
    // system information, device bus.
    let (Some(&log), Some(&archive), Some(&console), Some(&sysinfo), Some(&bus)) = (
        start.handles.first(),
        start.handles.get(1),
        start.handles.get(2),
        start.handles.get(3),
        start.handles.get(4),
    ) else {
        return EXIT_BAD_START;
    };
    let say = |args: core::fmt::Arguments<'_>| {
        let mut line = Buffer::<256>::new();
        let _ = line.write_str("init: ");
        let _ = line.write_fmt(args);
        let _ = oceans_rt::debug_write(log, line.as_str());
    };

    let archive = match map_archive(archive) {
        Ok(archive) => archive,
        Err(problem) => {
            say(format_args!("cannot read the boot archive: {problem}"));
            return EXIT_BAD_START;
        }
    };
    let Ok(events) = oceans_rt::notification_create() else {
        return EXIT_BAD_START;
    };
    // The power endpoint (ADR-0085): service exits arrive on it too.
    let Ok((power_server, power)) = oceans_rt::endpoint_create() else {
        return EXIT_BAD_START;
    };
    if oceans_rt::endpoint_bind(power_server, events).is_err() {
        return EXIT_BAD_START;
    }
    let mut init = Init {
        log,
        console,
        sysinfo,
        bus,
        display: start.handles.get(5).copied(),
        archive,
        images: [("", Handle(0)); MAX_IMAGES],
        image_count: 0,
        events,
        power,
        services: alloc::vec![NO_SERVICE; MAX_SERVICES],
        count: 0,
        registry: Registry {
            names: [""; MAX_ENDPOINTS],
            clients: [Handle(0); MAX_ENDPOINTS],
            len: 0,
        },
    };

    let Some(manifest) = archive
        .find(MANIFEST)
        .and_then(|bytes| core::str::from_utf8(bytes).ok())
    else {
        say(format_args!("no valid {MANIFEST} in the boot archive"));
        return EXIT_BAD_MANIFEST;
    };
    match parse(manifest, &mut init.services) {
        Ok(count) => init.count = count,
        Err((line, problem)) => {
            say(format_args!("{MANIFEST}:{line}: {problem}"));
            return EXIT_BAD_MANIFEST;
        }
    }
    say(format_args!(
        "{} services in {MANIFEST}{}",
        init.count,
        if test_mode { " (test mode)" } else { "" }
    ));

    for index in 0..init.count {
        init.start_service(index);
    }

    let mut data = [0u8; 16];
    let mut handles = [Handle(0); 4];
    loop {
        if test_mode && init.settled() {
            return init.verify();
        }
        let got = match oceans_rt::ipc_receive_msg(power_server, &mut data, &mut handles) {
            Ok(got) => got,
            Err(error) => {
                say(format_args!("cannot wait for service events: {error:?}"));
                return EXIT_SUPERVISION_FAILED;
            }
        };
        if got.signals != 0 {
            for index in 0..init.count {
                if got.signals & (1 << index) != 0 {
                    init.on_exit(index);
                }
            }
            continue;
        }
        if got.closed {
            continue;
        }
        // Handles have no place in a power request.
        for &handle in &handles[..got.handles_len] {
            let _ = oceans_rt::close(handle);
        }
        let answer = init.power_answer(got.label);
        let _ = oceans_rt::ipc_reply(0, &[answer]);
        if answer == power::ACCEPTED {
            return init.stop_system(got.label, test_mode);
        }
    }
}

/// Maps the boot archive read-only for init's lifetime and validates it
/// (the kernel did too; init does not rely on that).
fn map_archive(memory: Handle) -> Result<Archive<'static>, &'static str> {
    let size = oceans_rt::memory_size(memory).map_err(|_| "cannot size it")? as usize;
    let base = oceans_rt::memory_map(memory, 0, prot::READ).map_err(|_| "cannot map it")?;
    // SAFETY: the whole object is mapped readable at `base` and stays
    // mapped for init's lifetime; nothing writes it.
    let bytes = unsafe { core::slice::from_raw_parts(base, size) };
    // The object is zero-padded to whole pages; the archive's table says
    // where its files are, so the padding is never read as data.
    Archive::parse(bytes).map_err(|_| "it is damaged")
}

/// Parses the manifest into `services`; returns how many, or the 1-based
/// line number and problem of the first error.
fn parse(text: &'static str, services: &mut [Service]) -> Result<usize, (usize, &'static str)> {
    let mut count = 0;
    for (number, raw) in text.lines().enumerate() {
        let line_number = number + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix("service ") {
            if count == MAX_SERVICES {
                return Err((line_number, "too many services"));
            }
            let name = name.trim();
            if services[..count].iter().any(|s| s.name == name) {
                return Err((line_number, "duplicate service name"));
            }
            services[count] = Service::new(name);
            count += 1;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err((line_number, "expected `service NAME` or `key = value`"));
        };
        let (key, value) = (key.trim(), value.trim());
        let Some(service) = count.checked_sub(1).map(|i| &mut services[i]) else {
            return Err((line_number, "setting outside a service block"));
        };
        let error = |problem| Err((line_number, problem));
        match key {
            "image" => service.image = Some(value),
            "restart" => {
                service.restart = match value {
                    "always" => Restart::Always,
                    "on-failure" => Restart::OnFailure,
                    "never" => Restart::Never,
                    _ => return error("restart must be always, on-failure or never"),
                }
            }
            "max-restarts" => match value.parse() {
                Ok(n) => service.max_restarts = n,
                Err(_) => return error("max-restarts must be a number"),
            },
            "grant" | "provide" | "use" => {
                let grant = match (key, value) {
                    ("grant", "log") => Grant::Log,
                    ("grant", "log-read") => Grant::LogRead,
                    ("grant", "console") => Grant::Console,
                    ("grant", "sysinfo") => Grant::SystemInfo,
                    ("grant", "devices") => Grant::Devices,
                    ("grant", "console-input") => Grant::ConsoleInput,
                    ("grant", "display") => Grant::Display,
                    ("grant", "power") => Grant::Power,
                    ("grant", other) => {
                        if let Some(module) = other.strip_prefix("module:")
                            && !module.is_empty()
                        {
                            Grant::Module(module)
                        } else if let Some(path) = other.strip_prefix("storage:") {
                            if !valid_storage_path(path) {
                                return error(
                                    "storage grants are storage:/DIR[/DIR...] (plain names)",
                                );
                            }
                            Grant::Storage(path)
                        } else if let Some(id) = other.strip_prefix("device:") {
                            match parse_device_id(id) {
                                Some((vendor, device)) => Grant::Device(vendor, device, id),
                                None => return error("device grants are device:VVVV:DDDD (hex)"),
                            }
                        } else if let Some(id) = other.strip_prefix("device-class:") {
                            match parse_class(id) {
                                Some(class) => Grant::DeviceClass(class, id),
                                None => return error("class grants are device-class:CCSSPP (hex)"),
                            }
                        } else {
                            return error(
                                "unknown grant (known: log, log-read, console, console-input, sysinfo, devices, display, power, device:VVVV:DDDD, device-class:CCSSPP, module:NAME, storage:/PATH)",
                            );
                        }
                    }
                    ("provide", name) => Grant::Provide(name),
                    (_, value) => {
                        let (name, alias) = use_parts(value);
                        if name.is_empty()
                            || alias.is_empty()
                            || name.contains(' ')
                            || alias.contains(' ')
                        {
                            return error("use grants are `use = NAME` or `use = NAME as ALIAS`");
                        }
                        Grant::Use(value)
                    }
                };
                let Some(slot) = service.grants.iter_mut().find(|g| g.is_none()) else {
                    return error("too many grants");
                };
                *slot = Some(grant);
            }
            "expect-exit" => match value.parse() {
                Ok(code) => service.expect_exit = Some(code),
                Err(_) => return error("expect-exit must be a number"),
            },
            "expect-runs" => match value.parse() {
                Ok(runs) => service.expect_runs = Some(runs),
                Err(_) => return error("expect-runs must be a number"),
            },
            _ => return error("unknown setting"),
        }
    }

    // Every service needs an image; every `use` an earlier `provide`.
    for (index, service) in services[..count].iter().enumerate() {
        if service.image.is_none() {
            return Err((0, "a service has no image"));
        }
        for grant in service.grants.iter().flatten() {
            if let Grant::Use(value) = grant {
                let (name, _) = use_parts(value);
                let provided = services[..index]
                    .iter()
                    .flat_map(|s| s.grants.iter().flatten())
                    .any(|g| matches!(g, Grant::Provide(p) if *p == name));
                if !provided {
                    return Err((0, "`use` of an endpoint no earlier service provides"));
                }
            }
        }
    }
    Ok(count)
}

impl Init {
    fn say(&self, args: core::fmt::Arguments<'_>) {
        let mut line = Buffer::<256>::new();
        let _ = line.write_str("init: ");
        let _ = line.write_fmt(args);
        let _ = oceans_rt::debug_write(self.log, line.as_str());
    }

    /// A read-only memory object holding archive file `name`, unpacked on
    /// first use and kept.
    fn module(&mut self, name: &str) -> Option<Handle> {
        if let Some(&(_, handle)) = self.images[..self.image_count]
            .iter()
            .find(|&&(image, _)| image == name)
        {
            return Some(handle);
        }
        if self.image_count == MAX_IMAGES {
            return None;
        }
        let file = self.archive.files().find(|f| f.name == name)?;
        let memory = oceans_rt::memory_create(file.data.len().max(1) as u64).ok()?;
        let copied = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
            .map(|base| {
                // SAFETY: just mapped writable, at least `data.len()` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(file.data.as_ptr(), base, file.data.len())
                };
                let _ = oceans_rt::memory_unmap(base);
            })
            .is_ok();
        // Read-only from here on: what services receive are narrower copies.
        let image = copied
            .then(|| oceans_rt::duplicate(memory, MODULE_RIGHTS).ok())
            .flatten();
        let _ = oceans_rt::close(memory);
        let image = image?;
        self.images[self.image_count] = (file.name, image);
        self.image_count += 1;
        Some(image)
    }

    /// Starts service `index` with its declared capabilities. A failure is
    /// logged and settles the service.
    fn start_service(&mut self, index: usize) {
        let service = self.services[index];
        match self.spawn(&service, index) {
            Ok(process) => {
                self.services[index].process = Some(process);
                self.say(format_args!("started {}", service.name));
            }
            Err(error) => {
                self.services[index].settled = true;
                self.say(format_args!("cannot start {}: {error:?}", service.name));
            }
        }
    }

    fn spawn(&mut self, service: &Service, index: usize) -> Result<Handle, Error> {
        let image = service
            .image
            .and_then(|name| self.module(name))
            .ok_or(Error::InvalidImage)?;
        // The service's capabilities, plus a directory describing them as
        // the last handle.
        let mut handles = [Handle(0); MAX_GRANTS + 1];
        let mut directory = Buffer::<1024>::new();
        let mut count = 0;
        let result = (|| {
            for grant in service.grants.iter().flatten() {
                let (handle, kind, name) = match *grant {
                    Grant::Log => (oceans_rt::duplicate(self.log, LOG_RIGHTS)?, "log", "log"),
                    // Reading the kept log (ADR-0070): it may hold what users
                    // and agents did, so only on request.
                    Grant::LogRead => (
                        oceans_rt::duplicate(
                            self.log,
                            rights::READ | rights::DUPLICATE | rights::TRANSFER,
                        )?,
                        "logs",
                        "logs",
                    ),
                    Grant::Console => (
                        oceans_rt::duplicate(self.console, CONSOLE_RIGHTS)?,
                        "console",
                        "console",
                    ),
                    Grant::SystemInfo => (
                        oceans_rt::duplicate(self.sysinfo, SYSINFO_RIGHTS)?,
                        "sysinfo",
                        "sysinfo",
                    ),
                    Grant::Devices => (
                        oceans_rt::duplicate(self.bus, DEVICES_RIGHTS)?,
                        "devices",
                        "devices",
                    ),
                    Grant::Display => (
                        oceans_rt::duplicate(
                            self.display.ok_or(Error::NotFound)?,
                            rights::READ | rights::MANAGE | rights::TRANSFER,
                        )?,
                        "display",
                        "display",
                    ),
                    // Exclusive: a restarted driver reopens it once the old
                    // instance's capability has been closed at its exit.
                    Grant::Device(vendor, device, id) => (
                        oceans_rt::device_open(self.bus, vendor, device, 0)?,
                        "device",
                        id,
                    ),
                    Grant::DeviceClass(class, id) => (
                        oceans_rt::device_open_class(self.bus, class, 0)?,
                        "device",
                        id,
                    ),
                    Grant::ConsoleInput => (
                        oceans_rt::duplicate(self.console, CONSOLE_INPUT_RIGHTS)?,
                        "console-input",
                        "console-input",
                    ),
                    Grant::Provide(name) => {
                        let (server, client) = oceans_rt::endpoint_create()?;
                        if !self.registry.set(name, client) {
                            let _ = oceans_rt::close(server);
                            let _ = oceans_rt::close(client);
                            return Err(Error::TooLarge);
                        }
                        (server, "provide", name)
                    }
                    Grant::Use(value) => {
                        let (name, alias) = use_parts(value);
                        let client = self.registry.get(name).ok_or(Error::InvalidHandle)?;
                        (oceans_rt::duplicate(client, USE_RIGHTS)?, "use", alias)
                    }
                    Grant::Module(name) => {
                        let module = self.module(name).ok_or(Error::InvalidImage)?;
                        (oceans_rt::duplicate(module, MODULE_RIGHTS)?, "module", name)
                    }
                    Grant::Storage(path) => {
                        let fs = self.registry.get("fs").ok_or(Error::NotFound)?;
                        (open_storage(fs, path)?, "use", "storage")
                    }
                    Grant::Power => (
                        oceans_rt::duplicate(self.power, USE_RIGHTS)?,
                        "power",
                        "power",
                    ),
                };
                handles[count] = handle;
                let _ = writeln!(directory, "{count} {kind} {name}");
                count += 1;
            }
            let _ = writeln!(directory, "{count} directory handles");
            handles[count] = publish(directory.as_bytes())?;
            count += 1;
            let process =
                oceans_rt::process_spawn_named(image, 0, &handles[..count], 0, service.name)?;
            oceans_rt::process_watch(process, self.events, 1 << index)?;
            Ok(process)
        })();
        if result.is_err() {
            // Grants not handed over stay ours: close them.
            for &handle in &handles[..count] {
                let _ = oceans_rt::close(handle);
            }
        }
        result
    }

    /// Service `index` exited: record it and apply its restart policy.
    fn on_exit(&mut self, index: usize) {
        let Some(process) = self.services[index].process.take() else {
            return;
        };
        let code = oceans_rt::process_wait(process).unwrap_or(i64::MIN);
        let _ = oceans_rt::close(process);
        let service = &mut self.services[index];
        service.runs += 1;
        service.last_exit = Some(code);

        let wants_restart = match service.restart {
            Restart::Always => true,
            Restart::OnFailure => code != 0,
            Restart::Never => false,
        };
        let (name, restarts, max) = (service.name, service.restarts, service.max_restarts);
        if wants_restart && restarts < max {
            service.restarts += 1;
            let backoff = (BACKOFF_BASE_MS << restarts).min(BACKOFF_MAX_MS);
            self.say(format_args!(
                "{name} exited with {code}; restart {}/{max} in {backoff} ms",
                restarts + 1
            ));
            oceans_rt::sleep_ms(backoff);
            self.start_service(index);
        } else {
            service.settled = true;
            if wants_restart {
                self.say(format_args!(
                    "{name} exited with {code}; giving up after {max} restarts"
                ));
            } else {
                self.say(format_args!("{name} exited with {code}"));
            }
        }
    }

    /// The answer to power request `action`: accepted only if this machine
    /// can do it (the kernel says), before anything stops.
    fn power_answer(&self, action: u64) -> u8 {
        let needed = match action {
            power::OFF => power::CAN_OFF,
            power::RESTART => power::CAN_RESTART,
            _ => return power::INVALID,
        };
        match oceans_rt::power_query(self.sysinfo) {
            Ok(can) if can & needed != 0 => power::ACCEPTED,
            Ok(_) => {
                self.say(format_args!("this machine cannot be switched off"));
                power::NOT_POSSIBLE
            }
            Err(error) => {
                self.say(format_args!("cannot ask the kernel about power: {error:?}"));
                power::NOT_POSSIBLE
            }
        }
    }

    /// Stops the system, then switches off or restarts (`action`). Every
    /// service is stopped in reverse start order: the filesystem's clients
    /// (those started after the service providing `fs`), then the
    /// filesystem is synced, then the rest. Returns only in test mode
    /// (with the expectations' verdict; the kernel ends the boot) or if the
    /// kernel could not do it.
    fn stop_system(&mut self, action: u64, test_mode: bool) -> i64 {
        let what = if action == power::OFF {
            "switch off"
        } else {
            "restart"
        };
        self.say(format_args!("asked to {what}: stopping the system"));
        let fs = self.services[..self.count].iter().position(|service| {
            service
                .grants
                .iter()
                .flatten()
                .any(|grant| matches!(grant, Grant::Provide("fs")))
        });
        let clients = fs.map_or(0, |index| index + 1);
        let mut stopped = 0;
        for index in (clients..self.count).rev() {
            stopped += usize::from(self.stop_service(index));
        }
        // Their open files were closed as they stopped, which commits them;
        // `SYNC` commits the rest and reaches every mounted disk.
        match self.registry.get("fs").map(|fs| Node(fs).sync()) {
            Some(Ok(())) => self.say(format_args!("disks synced")),
            Some(Err(error)) => {
                self.say(format_args!("cannot sync the disks: {}", error.message()))
            }
            None => {}
        }
        for index in (0..clients).rev() {
            stopped += usize::from(self.stop_service(index));
        }
        self.say(format_args!("{stopped} services stopped"));
        if test_mode {
            self.say(format_args!("test mode: the kernel ends the boot instead"));
            return self.verify();
        }
        let error = oceans_rt::system_power(self.sysinfo, action);
        self.say(format_args!("the kernel could not {what}: {error:?}"));
        EXIT_SUPERVISION_FAILED
    }

    /// Stops service `index` if it runs, and records how it ended; `true`
    /// if it was running.
    fn stop_service(&mut self, index: usize) -> bool {
        let Some(process) = self.services[index].process.take() else {
            return false;
        };
        let _ = oceans_rt::process_kill(process);
        let code = oceans_rt::process_wait(process).unwrap_or(i64::MIN);
        let _ = oceans_rt::close(process);
        let service = &mut self.services[index];
        service.runs += 1;
        service.last_exit = Some(code);
        service.settled = true;
        true
    }

    /// Every service that is not meant to run forever has settled.
    fn settled(&self) -> bool {
        self.services[..self.count]
            .iter()
            .all(|s| s.restart == Restart::Always || s.settled)
    }

    /// Checks the manifest's expectations (test mode).
    fn verify(&self) -> i64 {
        let mut ok = true;
        for service in &self.services[..self.count] {
            if let Some(expected) = service.expect_exit
                && service.last_exit != Some(expected)
            {
                self.say(format_args!(
                    "FAIL {}: exit {:?}, expected {expected}",
                    service.name, service.last_exit
                ));
                ok = false;
            }
            if let Some(expected) = service.expect_runs
                && service.runs != expected
            {
                self.say(format_args!(
                    "FAIL {}: {} runs, expected {expected}",
                    service.name, service.runs
                ));
                ok = false;
            }
        }
        if ok {
            self.say(format_args!("all service expectations met"));
            0
        } else {
            EXIT_EXPECTATION_FAILED
        }
    }
}

/// `VVVV:DDDD` in hex.
fn parse_device_id(id: &str) -> Option<(u16, u16)> {
    let (vendor, device) = id.split_once(':')?;
    let hex = |text: &str| {
        (text.len() == 4)
            .then(|| u16::from_str_radix(text, 16).ok())
            .flatten()
    };
    Some((hex(vendor)?, hex(device)?))
}

/// The endpoint and directory name of a `use` grant: `NAME` or `NAME as
/// ALIAS`.
fn use_parts(value: &'static str) -> (&'static str, &'static str) {
    match value.split_once(" as ") {
        Some((name, alias)) => (name.trim(), alias.trim()),
        None => (value, value),
    }
}

/// `CCSSPP`: class, subclass and programming interface in hex.
fn parse_class(id: &str) -> Option<u32> {
    (id.len() == 6)
        .then(|| u32::from_str_radix(id, 16).ok())
        .flatten()
}

/// A read-only memory object holding `text`, for handing to a service.
fn publish(text: &[u8]) -> Result<Handle, Error> {
    let memory = oceans_rt::memory_create(text.len().max(1) as u64)?;
    let result = (|| {
        let page = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)?;
        // SAFETY: just mapped writable, at least `text.len()` bytes.
        unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), page, text.len()) };
        oceans_rt::memory_unmap(page)?;
        oceans_rt::duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER)
    })();
    let _ = oceans_rt::close(memory);
    result
}

/// `storage:` paths: absolute, at least one component, each a plain name.
fn valid_storage_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && path
            .trim_start_matches('/')
            .split('/')
            .all(|part| oceans_fs_proto::valid_name(part.as_bytes()))
}

/// Opens (creating as needed) directory `path` through the filesystem
/// endpoint `fs`, writable, and returns its handle (ADR-0053).
fn open_storage(fs: Handle, path: &str) -> Result<Handle, Error> {
    let mut parent: Option<Node> = None;
    for part in path.trim_start_matches('/').split('/') {
        let from = parent.as_ref().map_or(Node(fs), |node| Node(node.0));
        let opened = from.open(part, flags::CREATE_DIRECTORY | flags::WRITE);
        if let Some(node) = parent.take() {
            node.close();
        }
        match opened {
            Ok((node, Kind::Directory)) => parent = Some(node),
            Ok((node, Kind::File)) => {
                node.close();
                return Err(Error::InvalidArgument);
            }
            Err(oceans_fs_proto::FsError::Ipc(error)) => return Err(error),
            Err(_) => return Err(Error::NotFound),
        }
    }
    parent.map(|node| node.0).ok_or(Error::InvalidArgument)
}
