//! init: the first user process and the Oceans service manager (ADR-0016).
//!
//! 1. Reads the boot module table the kernel passes (handle 1) to find the
//!    manifest (`services.conf`) and the service images.
//! 2. Parses the manifest: services in start order, each with an image, a
//!    restart policy and the capabilities it is granted.
//! 3. Starts each service with exactly the declared capabilities, in the
//!    declared order: `grant = log` (a log capability), `grant = console`
//!    (the system console), `provide = NAME`
//!    (the server end of a new endpoint NAME), `use = NAME` (a client end
//!    of endpoint NAME, provided by an earlier service), `grant = devices`
//!    (the PCI device list, read-only), `grant = device:VVVV:DDDD` (the
//!    PCI function with that vendor and device ID, opened exclusively:
//!    what makes a service its driver, ADR-0021).
//! 4. Supervises: one notification, a bit per service, signalled by the
//!    kernel when a service exits. Restart policies `always`, `on-failure`
//!    and `never`, with exponential backoff and a restart limit.
//!
//! In smoke-test mode (argument 1) init exits once every non-`always`
//! service has settled, with 0 only if every `expect-exit` / `expect-runs`
//! line of the manifest holds.
//!
//! No allocator: everything is fixed-size, and strings borrow the mapped,
//! read-only manifest, which stays mapped for init's lifetime.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_rt::{Buffer, Error, Handle, Start, prot, rights};

oceans_rt::entry!(main);

const MANIFEST: &str = "services.conf";
const MAX_SERVICES: usize = 32;
const MAX_GRANTS: usize = 16;
const MAX_ENDPOINTS: usize = 16;
const DEFAULT_MAX_RESTARTS: u32 = 5;
const BACKOFF_BASE_MS: u64 = 100;
const BACKOFF_MAX_MS: u64 = 2000;

/// Rights handed to services. `DUPLICATE` lets a service (e.g. the shell)
/// pass narrower copies on to programs it starts; it never widens rights.
const LOG_RIGHTS: u32 = rights::WRITE | rights::DUPLICATE | rights::TRANSFER;
const CONSOLE_RIGHTS: u32 = rights::READ | rights::WRITE | rights::DUPLICATE | rights::TRANSFER;
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
    Console,
    SystemInfo,
    Devices,
    /// The PCI function with this vendor and device ID; the text is the
    /// manifest's `VVVV:DDDD`.
    Device(u16, u16, &'static str),
    Provide(&'static str),
    Use(&'static str),
    Module(&'static str),
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

struct Init {
    log: Handle,
    console: Handle,
    sysinfo: Handle,
    /// The PCI device bus.
    bus: Handle,
    /// Lines `<module name> <handle index>`.
    module_table: &'static str,
    start: Start,
    events: Handle,
    services: [Service; MAX_SERVICES],
    count: usize,
    registry: Registry,
}

fn main(start: Start) -> i64 {
    let test_mode = start.arg == 1;
    // Boot contract (kernel process::init): log, module table, console,
    // system information, device bus.
    let (Some(&log), Some(&table), Some(&console), Some(&sysinfo), Some(&bus)) = (
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

    let Some(module_table) = map_text(table) else {
        say(format_args!("cannot read the boot module table"));
        return EXIT_BAD_START;
    };
    let Ok(events) = oceans_rt::notification_create() else {
        return EXIT_BAD_START;
    };
    let mut init = Init {
        log,
        console,
        sysinfo,
        bus,
        module_table,
        start,
        events,
        services: [NO_SERVICE; MAX_SERVICES],
        count: 0,
        registry: Registry {
            names: [""; MAX_ENDPOINTS],
            clients: [Handle(0); MAX_ENDPOINTS],
            len: 0,
        },
    };

    let Some(manifest) = init.module(MANIFEST).and_then(map_text) else {
        say(format_args!("no {MANIFEST} boot module"));
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

    loop {
        if test_mode && init.settled() {
            return init.verify();
        }
        let Ok(bits) = oceans_rt::notification_wait(init.events) else {
            say(format_args!("cannot wait for service events"));
            return EXIT_SUPERVISION_FAILED;
        };
        for index in 0..init.count {
            if bits & (1 << index) != 0 {
                init.on_exit(index);
            }
        }
    }
}

/// Maps a read-only text module and returns its contents (up to the first
/// NUL: objects are zero-padded to whole pages). Text modules must be
/// smaller than one page.
fn map_text(memory: Handle) -> Option<&'static str> {
    let base = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // Modules are small; read up to one page past the start, stopping at
    // the zero padding.
    // SAFETY: the object is mapped readable and is at least one page; it
    // stays mapped for init's lifetime.
    let page = unsafe { core::slice::from_raw_parts(base, 4096) };
    // No terminating zero: the text fills the page and may continue; refuse
    // rather than silently use a truncated manifest (limit: 4 KiB - 1).
    let len = page.iter().position(|&b| b == 0)?;
    core::str::from_utf8(&page[..len]).ok()
}

/// Parses the manifest into `services`; returns how many, or the 1-based
/// line number and problem of the first error.
fn parse(
    text: &'static str,
    services: &mut [Service; MAX_SERVICES],
) -> Result<usize, (usize, &'static str)> {
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
                    ("grant", "console") => Grant::Console,
                    ("grant", "sysinfo") => Grant::SystemInfo,
                    ("grant", "devices") => Grant::Devices,
                    ("grant", other) => {
                        if let Some(module) = other.strip_prefix("module:")
                            && !module.is_empty()
                        {
                            Grant::Module(module)
                        } else if let Some(id) = other.strip_prefix("device:") {
                            match parse_device_id(id) {
                                Some((vendor, device)) => Grant::Device(vendor, device, id),
                                None => return error("device grants are device:VVVV:DDDD (hex)"),
                            }
                        } else {
                            return error(
                                "unknown grant (known: log, console, sysinfo, devices, device:VVVV:DDDD, module:NAME)",
                            );
                        }
                    }
                    ("provide", name) => Grant::Provide(name),
                    (_, name) => Grant::Use(name),
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
            if let Grant::Use(name) = grant {
                let provided = services[..index]
                    .iter()
                    .flat_map(|s| s.grants.iter().flatten())
                    .any(|g| matches!(g, Grant::Provide(p) if p == name));
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

    /// The memory object holding boot module `name`.
    fn module(&self, name: &str) -> Option<Handle> {
        self.module_table.lines().find_map(|line| {
            let (module, index) = line.split_once(' ')?;
            let index: usize = index.trim().parse().ok()?;
            (module == name).then(|| self.start.handles.get(index).copied())?
        })
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
                    // Exclusive: a restarted driver reopens it once the old
                    // instance's capability has been closed at its exit.
                    Grant::Device(vendor, device, id) => (
                        oceans_rt::device_open(self.bus, vendor, device, 0)?,
                        "device",
                        id,
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
                    Grant::Use(name) => {
                        let client = self.registry.get(name).ok_or(Error::InvalidHandle)?;
                        (oceans_rt::duplicate(client, USE_RIGHTS)?, "use", name)
                    }
                    Grant::Module(name) => {
                        let module = self.module(name).ok_or(Error::InvalidImage)?;
                        (oceans_rt::duplicate(module, MODULE_RIGHTS)?, "module", name)
                    }
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
