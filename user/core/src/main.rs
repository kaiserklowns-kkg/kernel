//! Oceans Core: the app manager and permission broker (ADR-0045 to
//! ADR-0047).
//!
//! - **Packages** (ADR-0046): `INSTALL` verifies a package (archive,
//!   Ed25519 signature, trusted publisher key, manifest) and keeps it as
//!   `/apps/ID/package.opk`; an update must be newer and signed by the same
//!   key, and the version it replaces stays as `previous.opk` for
//!   `ROLLBACK`. The package is verified again at every start, so a file
//!   changed on disk never runs.
//! - **Permissions** (ADR-0047): an app gets only what its manifest asks
//!   for, and of that only what is automatic (console, its own storage,
//!   system information) or what the user allowed. Undecided requests make
//!   `RUN` answer `NeedsConsent`; the client asks the user and sends the
//!   answer (`DECIDE`). Decisions persist in `/system/permissions`; every
//!   install, start, stop, decision and removal is appended to
//!   `/system/audit.log` and the kernel log. Denying a permission a running
//!   app holds stops it: capabilities cannot be taken back from a process,
//!   so the process goes.
//! - **Lifecycle** (ADR-0045): `RUN` spawns the app from its verified
//!   package with exactly the granted capabilities and a handle directory,
//!   in the foreground (the caller gets a wait-only process handle) or
//!   detached; `STOP` kills it (ABI 12); exits are noticed through a
//!   notification bound to the endpoint.
//! - **Windows** (ADR-0059): an app given `window` gets a client end of the
//!   display service's window endpoint, minted by Core with a badge of
//!   its own; the display service asks `WINDOW_OWNER` whose it is, and
//!   frames the app's windows with its verified name. The display service
//!   registers the endpoint (`WINDOWS`) as a server end Core can only mint
//!   from.
//! - **Runtimes** (ADR-0052): a `native` program is spawned directly; a
//!   `wasm` one (a Go program) is run by the Go host, spawned with the
//!   same capabilities plus the program as a read-only `module`.
//!
//! Manifest grants: `log`, `provide = core`, `use = fs` (the root: core
//! keeps `/apps`, `/system` and hands out `/home`), and what it passes on:
//! `use = net`, `use = input`, `sysinfo`; `module:trust.keys` (the trusted
//! publisher keys, from the boot image); `module:gohost` (the Go host,
//! for `wasm` apps).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_core_proto::{
    Decision, MAX_DATA, Status, access, decision, display_grant, field, op, outcome, run_flags,
    source,
};
use oceans_fs_proto::{FsError, Kind, Node, flags};
use oceans_package::{Package, PackageError, Permission, Runtime, TrustedKey, Version};
use oceans_rt::{Buffer, Directory, Handle, Start, prot, rights};

oceans_rt::entry!(main);

/// Most apps running at once (one notification bit each).
const MAX_RUNNING: usize = 32;
/// The notification bit of the restart timer (ADR-0049).
const RESTART_TIMER: u64 = 1 << 63;
/// Restarts of a failing service per boot, the first after a second, each
/// following one waiting twice as long.
const MAX_RESTARTS: u32 = 5;
const RESTART_DELAY_MS: u64 = 1000;
/// Largest package accepted.
const MAX_PACKAGE: u64 = 32 << 20;
/// Bytes moved per filesystem request.
const FILE_BUFFER: usize = 128 * 1024;
/// Why a web app does not start on Oceans (ADR-0064).
const WEB_APP: &str = "a web app: open it from Apps in the Oceans web experience";
/// Developers' keys the user trusts (ADR-0063), in `/system`.
const TRUST_FILE: &str = "trust.keys";
/// Audit entries kept in memory for `AUDIT` (all go to the log file).
const AUDIT_KEPT: usize = 64;
/// Handles an app may get besides its directory: one per permission, a
/// log (services), its identity, arguments, and its program module
/// (`wasm`).
const MAX_APP_HANDLES: usize = Permission::ALL.len() + 4;

const EXIT_BAD_START: i64 = 2;
const EXIT_NO_STORAGE: i64 = 3;
const EXIT_RECEIVE: i64 = 4;

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(server), Some(fs)) = (
        directory.find("log", "log"),
        directory.find("provide", "core"),
        directory.find("use", "fs"),
    ) else {
        return EXIT_BAD_START;
    };
    let mut core = match Core::start(log, &directory, Node(fs)) {
        Ok(core) => core,
        Err(problem) => {
            say(log, format_args!("core: {problem}"));
            return EXIT_NO_STORAGE;
        }
    };
    // init's stop notification (ADR-0086) when it gives one: Core's own
    // bits (apps' exits, the restart timer) go on it too.
    let notification = match directory.find("stop", "stop") {
        Some(stop) => stop,
        None => match oceans_rt::notification_create() {
            Ok(notification) => notification,
            Err(_) => return EXIT_BAD_START,
        },
    };
    if oceans_rt::endpoint_bind(server, notification).is_err() {
        return EXIT_BAD_START;
    }
    core.notification = notification;
    core.start_services();
    core.serve(server)
}

/// A copy of a module's bytes (the whole object: rounded up to pages).
fn module_bytes(memory: Handle) -> Option<Vec<u8>> {
    let size = usize::try_from(oceans_rt::memory_size(memory).ok()?).ok()?;
    let base = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: the whole object (`size` bytes) is mapped readable at `base`
    // until it is unmapped below.
    let bytes = unsafe { core::slice::from_raw_parts(base, size) }.to_vec();
    let _ = oceans_rt::memory_unmap(base);
    Some(bytes)
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<240>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// An installed app, from its verified manifest.
struct App {
    name: String,
    version: Version,
    publisher: String,
    description: String,
    channel: String,
    source: Option<String>,
    key: [u8; 32],
    /// What the manifest asks for, with the app's reasons.
    requests: Vec<(Permission, String)>,
    /// The version `ROLLBACK` returns to.
    previous: Option<Version>,
    /// A background service (ADR-0049).
    service: bool,
    /// How its program runs (ADR-0052).
    runtime: Runtime,
}

impl App {
    fn from(package: &Package<'_>, previous: Option<Version>) -> Self {
        let manifest = &package.manifest;
        Self {
            name: manifest.name.to_owned(),
            version: manifest.version,
            publisher: manifest.publisher.to_owned(),
            description: manifest.description.to_owned(),
            channel: manifest.channel.to_owned(),
            source: manifest.source.map(ToOwned::to_owned),
            key: package.key,
            requests: manifest
                .requests()
                .map(|r| (r.permission, r.reason.to_owned()))
                .collect(),
            previous,
            service: manifest.service,
            runtime: manifest.runtime,
        }
    }

    fn asks_for(&self, permission: Permission) -> bool {
        self.requests.iter().any(|(p, _)| *p == permission)
    }
}

/// What starting an app needs.
#[derive(Clone, Copy)]
struct Launch<'a> {
    id: &'a str,
    /// The process name (the program's name in the package).
    name: &'a str,
    /// `ID VERSION`, given to the app as `app info`.
    info: &'a str,
    /// What is spawned: the program, or the Go host for a `wasm` one.
    image: Handle,
    /// A `wasm` program, read-only, handed to the Go host as `module NAME`
    /// (moved to the process, or closed if it cannot start).
    module: Option<Handle>,
    granted: &'a [Permission],
    args: &'a str,
    /// Services get a log (ADR-0049).
    service: bool,
    /// The badge of its window end (`window`, ADR-0059).
    window: Option<u64>,
}

/// A publisher key Core trusts (ADR-0046, ADR-0063), perhaps until a date
/// (ADR-0067).
struct Trusted {
    publisher: String,
    key: [u8; 32],
    /// The last day it is trusted (days since 1970-01-01).
    until: Option<u32>,
}

impl Trusted {
    fn valid_on(&self, today: Option<u32>) -> bool {
        self.until
            .is_none_or(|until| today.is_some_and(|today| today <= until))
    }

    /// ` until YYYY-MM-DD`, or nothing.
    fn until_text(&self) -> String {
        self.until.map_or_else(String::new, |days| {
            let (y, m, d) = oceans_package::date::civil(days);
            alloc::format!(" until {y:04}-{m:02}-{d:02}")
        })
    }

    /// Its trust-list line: `KEY [until=YYYY-MM-DD] PUBLISHER`.
    fn line(&self) -> String {
        match self.until {
            None => alloc::format!("{} {}", hex32(&self.key), self.publisher),
            Some(days) => {
                let (y, m, d) = oceans_package::date::civil(days);
                alloc::format!(
                    "{} until={y:04}-{m:02}-{d:02} {}",
                    hex32(&self.key),
                    self.publisher
                )
            }
        }
    }
}

/// Today (UTC), if the wall clock is known.
fn today() -> Option<u32> {
    oceans_rt::unix_time_ms().map(oceans_package::date::today)
}

/// An install waiting for the user's confirmation (ADR-0061).
struct Proposal {
    number: u32,
    /// The package as verified: what is installed if the user agrees.
    bytes: Vec<u8>,
    id: String,
    version: Version,
    /// What `PENDING` answers after the number.
    summary: String,
}

struct Running {
    id: String,
    process: Handle,
    /// The permissions it was started with.
    granted: Vec<Permission>,
    /// The badge of its window end, if it was given one (ADR-0059).
    window: Option<u64>,
}

/// A failed request: its status, and text for the client when there is
/// more to say (why a package was refused).
struct Refusal {
    status: Status,
    text: Option<String>,
}

impl From<Status> for Refusal {
    fn from(status: Status) -> Self {
        Self { status, text: None }
    }
}

fn invalid(error: PackageError) -> Refusal {
    Refusal {
        status: Status::Invalid,
        text: Some(error.message().to_owned()),
    }
}

fn io(_: FsError) -> Refusal {
    Status::IoError.into()
}

struct Core {
    log: Handle,
    root: Node,
    apps_dir: Node,
    system_dir: Node,
    net: Option<Handle>,
    input: Option<Handle>,
    sysinfo: Option<Handle>,
    /// The audio service's endpoint, to ask for player ends (ADR-0094).
    audio: Option<Handle>,
    /// The display service's window endpoint (`WINDOWS`, ADR-0059): a
    /// server end Core may only mint client ends of.
    windows: Option<Handle>,
    next_window_badge: u64,
    /// The Go host's image, for `wasm` apps (ADR-0052).
    gohost: Option<Handle>,
    trusted: Vec<Trusted>,
    /// How many of `trusted` came with the boot image (the rest the user
    /// added, ADR-0063).
    boot_keys: usize,
    /// Core's own endpoint, to mint `manage-apps` ends (ADR-0081).
    server: Handle,
    /// The ids of the apps the image brings (ADR-0080): with the image's
    /// key, the system's own apps (ADR-0081).
    bundled: BTreeSet<String>,
    apps: BTreeMap<String, App>,
    /// The user's decisions: `true` allowed, `false` denied.
    decisions: BTreeMap<(String, Permission), bool>,
    running: [Option<Running>; MAX_RUNNING],
    notification: Handle,
    audit: VecDeque<String>,
    /// Rights of the client ends minted by `MINT` (ADR-0048), by badge.
    minted: BTreeMap<u64, u8>,
    next_badge: u64,
    /// The install waiting for the user (ADR-0061), and the last number.
    proposal: Option<Proposal>,
    next_proposal: u32,
    /// Services started at boot (ADR-0049), kept in `/system/services`.
    enabled: BTreeSet<String>,
    /// Restarts so far this boot, and those waiting for their time.
    restarts: BTreeMap<String, u32>,
    pending_restarts: Vec<(String, u64)>,
    /// Slots freed by `stop` whose exit signal has not arrived yet: not
    /// reused until it has (the signal would be taken for the new app's).
    stale: u64,
}

impl Core {
    fn start(log: Handle, directory: &Directory, root: Node) -> Result<Self, &'static str> {
        let dir = |name: &str| {
            root.open(name, flags::CREATE_DIRECTORY | flags::WRITE)
                .ok()
                .filter(|(_, kind)| *kind == Kind::Directory)
                .map(|(node, _)| node)
        };
        let apps_dir = dir("apps").ok_or("cannot open /apps")?;
        let system_dir = dir("system").ok_or("cannot open /system")?;
        // The user's files, handed to apps allowed `files`.
        dir("home").ok_or("cannot open /home")?.close();

        let trust_text = directory
            .find("module", "trust.keys")
            .and_then(oceans_rt::map_text)
            .unwrap_or("");
        let mut trusted: Vec<Trusted> = oceans_package::trusted_keys(trust_text)
            .map(|t| Trusted {
                publisher: t.publisher.to_owned(),
                key: t.key,
                until: None,
            })
            .collect();
        let boot_keys = trusted.len();
        // Developers' keys the user added (ADR-0063), after the image's,
        // some until a date (ADR-0067).
        if let Ok(bytes) = read_path(&system_dir, TRUST_FILE) {
            let text = core::str::from_utf8(&bytes).unwrap_or("");
            for entry in oceans_package::trust_entries(text) {
                let key = entry.key;
                if !trusted
                    .iter()
                    .any(|t| t.key == key.key || t.publisher == key.publisher)
                {
                    trusted.push(Trusted {
                        publisher: key.publisher.to_owned(),
                        key: key.key,
                        until: entry.until,
                    });
                }
            }
        }
        let bad = oceans_package::trust_errors(trust_text);
        if bad > 0 {
            say(
                log,
                format_args!("core: {bad} unreadable lines in trust.keys ignored"),
            );
        }

        let mut core = Self {
            log,
            root,
            apps_dir,
            system_dir,
            net: directory.find("use", "net"),
            input: directory.find("use", "input"),
            sysinfo: directory.find("sysinfo", "sysinfo"),
            audio: directory.find("use", "audio"),
            windows: None,
            next_window_badge: 1,
            proposal: None,
            next_proposal: 0,
            gohost: directory.find("module", "gohost"),
            trusted,
            boot_keys,
            server: Handle(0),
            bundled: BTreeSet::new(),
            apps: BTreeMap::new(),
            decisions: BTreeMap::new(),
            running: [const { None }; MAX_RUNNING],
            notification: Handle(0),
            audit: VecDeque::new(),
            minted: BTreeMap::new(),
            next_badge: 1,
            enabled: BTreeSet::new(),
            restarts: BTreeMap::new(),
            pending_restarts: Vec::new(),
            stale: 0,
        };
        core.load_apps();
        core.load_decisions();
        core.install_bundled(directory);
        core.load_enabled();
        say(
            log,
            format_args!(
                "core: ready, {} apps installed, {} trusted publisher keys",
                core.apps.len(),
                core.trusted.len()
            ),
        );
        Ok(core)
    }

    /// The keys trusted today: a dated key past its day (or with no known
    /// time) is not (ADR-0067).
    fn trust(&self) -> Vec<TrustedKey<'_>> {
        let today = today();
        self.trusted
            .iter()
            .filter(|t| t.valid_on(today))
            .map(|t| TrustedKey {
                publisher: &t.publisher,
                key: t.key,
            })
            .collect()
    }

    /// The apps the system image brings (ADR-0080): its `module:NAME.opk`
    /// grants. Each is installed when missing or older than the image's,
    /// through every check an install makes; one already as new is left.
    fn install_bundled(&mut self, directory: &Directory) {
        let mut packages = Vec::new();
        for line in directory.lines() {
            let mut words = line.split_whitespace();
            let (Some(_), Some("module"), Some(name)) = (words.next(), words.next(), words.next())
            else {
                continue;
            };
            if !name.ends_with(".opk") {
                continue;
            }
            let Some(bytes) = directory.find("module", name).and_then(module_bytes) else {
                say(
                    self.log,
                    format_args!("core: {name} from the image is unreadable"),
                );
                continue;
            };
            // A module is rounded up to whole pages: only the archive.
            let len = oceans_archive::Archive::parse(&bytes)
                .map_or(bytes.len(), |archive| archive.extent());
            packages.push((name, bytes[..len].to_vec()));
        }
        // Every bundled id first: they are the system's own (ADR-0081)
        // when the image's key signed them, checked as each installs.
        let trust = self.trust();
        let ids: Vec<String> = packages
            .iter()
            .filter_map(|(_, bytes)| {
                Package::open(bytes, &trust)
                    .ok()
                    .map(|package| package.manifest.id.to_owned())
            })
            .collect();
        self.bundled.extend(ids);
        for (name, bytes) in packages {
            let mut reply = Vec::new();
            match self.install_bytes(&bytes, &mut reply) {
                Ok(()) => say(
                    self.log,
                    format_args!("core: installed {name} from the system image"),
                ),
                Err(refusal) if refusal.status == Status::NotNewer => {}
                Err(refusal) => say(
                    self.log,
                    format_args!(
                        "core: {name} from the system image not installed: {}",
                        refusal.text.as_deref().unwrap_or(refusal.status.message())
                    ),
                ),
            }
        }
    }

    /// Every `/apps/ID/package.opk` that still verifies.
    fn load_apps(&mut self) {
        let mut names = Vec::new();
        let mut name = [0u8; oceans_fs_proto::MAX_NAME];
        for index in 0.. {
            match self.apps_dir.entry(index, &mut name) {
                Ok(Some((Kind::Directory, len))) => {
                    if let Ok(id) = core::str::from_utf8(&name[..len]) {
                        names.push(id.to_owned());
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        for id in names {
            let loaded = self.read_app_file(&id, "package.opk").and_then(|bytes| {
                let trust = self.trust();
                let package = Package::open(&bytes, &trust).map_err(invalid)?;
                if package.manifest.id != id {
                    return Err(invalid(PackageError::NoManifest));
                }
                let previous = self
                    .read_app_file(&id, "previous.opk")
                    .ok()
                    .and_then(|old| Package::open(&old, &trust).ok().map(|p| p.manifest.version));
                Ok(App::from(&package, previous))
            });
            match loaded {
                Ok(app) => {
                    self.apps.insert(id, app);
                }
                Err(refusal) => say(
                    self.log,
                    format_args!(
                        "core: {id}: not loaded: {}",
                        refusal.text.as_deref().unwrap_or(refusal.status.message())
                    ),
                ),
            }
        }
    }

    fn load_enabled(&mut self) {
        let Ok(bytes) = read_path(&self.system_dir, "services") else {
            return;
        };
        for id in core::str::from_utf8(&bytes).unwrap_or("").lines() {
            if self.apps.get(id).is_some_and(|app| app.service) {
                self.enabled.insert(id.to_owned());
            }
        }
    }

    fn save_enabled(&self) -> Result<(), Refusal> {
        let mut text = String::new();
        for id in &self.enabled {
            let _ = writeln!(text, "{id}");
        }
        write_file(&self.system_dir, "services", text.as_bytes()).map_err(io)
    }

    /// Starts the enabled services (at boot, once the endpoint is bound).
    fn start_services(&mut self) {
        let ids: Vec<String> = self.enabled.iter().cloned().collect();
        for id in ids {
            match self.start_service(&id) {
                Ok(()) => say(self.log, format_args!("core: started service {id}")),
                Err(refusal) => say(
                    self.log,
                    format_args!(
                        "core: service {id} not started: {}",
                        refusal.status.message()
                    ),
                ),
            }
        }
    }

    fn start_service(&mut self, id: &str) -> Result<(), Refusal> {
        let mut data = Vec::with_capacity(2 + id.len());
        data.push(run_flags::DETACH);
        data.push(id.len() as u8);
        data.extend_from_slice(id.as_bytes());
        self.start_app(&data, None).map(drop)
    }

    fn enable(&mut self, data: &[u8]) -> Result<(), Refusal> {
        let id = id_of(data)?.to_owned();
        if !self.app(&id)?.service {
            return Err(Status::NotAService.into());
        }
        if self.slot_of(&id).is_none() {
            match self.start_service(&id) {
                Ok(())
                | Err(Refusal {
                    status: Status::AlreadyRunning,
                    ..
                }) => {}
                Err(refusal) => return Err(refusal),
            }
        }
        if self.enabled.insert(id.clone()) {
            self.save_enabled()?;
            self.record(format_args!("enabled service {id} (starts at boot)"));
        }
        self.restarts.remove(&id);
        Ok(())
    }

    fn disable(&mut self, data: &[u8]) -> Result<(), Refusal> {
        let id = id_of(data)?.to_owned();
        self.app(&id)?;
        self.pending_restarts.retain(|(pending, _)| *pending != id);
        if self.enabled.remove(&id) {
            self.save_enabled()?;
            self.record(format_args!("disabled service {id}"));
        }
        self.stop(&id, "service disabled");
        Ok(())
    }

    /// A failed service is restarted later, while it has restarts left.
    fn schedule_restart(&mut self, id: &str, code: i64) {
        let count = self.restarts.entry(id.to_owned()).or_insert(0);
        *count += 1;
        let count = *count;
        if count > MAX_RESTARTS {
            self.record(format_args!(
                "gave up on service {id}: failed {MAX_RESTARTS} times (last exit {code})"
            ));
            return;
        }
        let delay = RESTART_DELAY_MS << (count - 1);
        say(
            self.log,
            format_args!(
                "core: service {id} failed (exit {code}); restarting in {} s ({count} of {MAX_RESTARTS})",
                delay / 1000
            ),
        );
        self.pending_restarts
            .push((id.to_owned(), oceans_rt::clock_ms() + delay));
        self.arm_restart_timer();
    }

    fn arm_restart_timer(&self) {
        if let Some(due) = self.pending_restarts.iter().map(|(_, due)| *due).min() {
            let wait = due.saturating_sub(oceans_rt::clock_ms()).max(1);
            let _ = oceans_rt::timer_set(self.notification, RESTART_TIMER, wait);
        }
    }

    fn run_due_restarts(&mut self) {
        let now = oceans_rt::clock_ms();
        let (due, later): (Vec<_>, Vec<_>) = core::mem::take(&mut self.pending_restarts)
            .into_iter()
            .partition(|(_, at)| *at <= now);
        self.pending_restarts = later;
        for (id, _) in due {
            if !self.enabled.contains(&id) || self.slot_of(&id).is_some() {
                continue;
            }
            match self.start_service(&id) {
                Ok(()) => say(self.log, format_args!("core: restarted service {id}")),
                Err(refusal) => say(
                    self.log,
                    format_args!(
                        "core: service {id} not restarted: {}",
                        refusal.status.message()
                    ),
                ),
            }
        }
        self.arm_restart_timer();
    }

    fn load_decisions(&mut self) {
        let Ok(bytes) = read_path(&self.system_dir, "permissions") else {
            return;
        };
        let text = core::str::from_utf8(&bytes).unwrap_or("");
        for line in text.lines() {
            let mut words = line.split_whitespace();
            if let (Some(id), Some(permission), Some(decision)) =
                (words.next(), words.next(), words.next())
                && let Some(permission) = Permission::from_name(permission)
                && self.apps.contains_key(id)
            {
                self.decisions
                    .insert((id.to_owned(), permission), decision == "allow");
            }
        }
    }

    fn save_decisions(&self) -> Result<(), Refusal> {
        let mut text = String::new();
        for ((id, permission), allowed) in &self.decisions {
            let _ = writeln!(
                text,
                "{id} {} {}",
                permission.name(),
                if *allowed { "allow" } else { "deny" }
            );
        }
        write_file(&self.system_dir, "permissions", text.as_bytes()).map_err(io)
    }

    /// Records an event: kernel log, `/system/audit.log`, and the recent
    /// entries `AUDIT` returns.
    fn record(&mut self, args: core::fmt::Arguments<'_>) {
        let mut entry = String::new();
        let _ = entry.write_fmt(args);
        say(self.log, format_args!("core: audit: {entry}"));
        let mut line = String::new();
        let _ = writeln!(line, "{} {entry}", timestamp());
        if append_file(&self.system_dir, "audit.log", line.as_bytes()).is_err() {
            say(self.log, format_args!("core: cannot write the audit log"));
        }
        if self.audit.len() == AUDIT_KEPT {
            self.audit.pop_back();
        }
        line.pop();
        self.audit.push_front(line);
    }

    fn read_app_file(&self, id: &str, name: &str) -> Result<Vec<u8>, Refusal> {
        let (dir, _) = self.apps_dir.open(id, 0).map_err(io)?;
        let bytes = read_path(&dir, name);
        dir.close();
        bytes.map_err(io)
    }

    /// Signed by a key the image brought (ADR-0081): the system's own.
    fn system_key(&self, key: &[u8; 32]) -> bool {
        self.trusted[..self.boot_keys].iter().any(|t| t.key == *key)
    }

    /// Brought by the image and signed by its key: the system's own
    /// (ADR-0081). An example signed with the development key is not, nor
    /// a package of the same id from elsewhere.
    fn system_app(&self, id: &str) -> bool {
        self.bundled.contains(id)
            && self
                .apps
                .get(id)
                .is_some_and(|app| self.system_key(&app.key))
    }

    fn decision(&self, id: &str, permission: Permission) -> Decision {
        // The system's own apps get what they ask for, as the system does
        // (ADR-0081).
        if permission.automatic() || self.system_app(id) {
            return Decision::Automatic;
        }
        match self.decisions.get(&(id.to_owned(), permission)) {
            Some(true) => Decision::Allowed,
            Some(false) => Decision::Denied,
            None => Decision::Undecided,
        }
    }

    fn serve(&mut self, server: Handle) -> i64 {
        self.server = server;
        let mut data = [0u8; 256];
        let mut handles = [Handle(0); 4];
        loop {
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                // Every client end is gone (init keeps one, so: shutdown).
                Err(oceans_rt::Error::PeerClosed) => return 0,
                Err(error) => {
                    say(self.log, format_args!("core: receive failed: {error:?}"));
                    return EXIT_RECEIVE;
                }
            };
            if got.signals & oceans_rt::STOP != 0 {
                // The system is stopping (ADR-0086): apps end first, so
                // their files are closed (and committed) before the disks
                // are synced.
                self.stop_all();
                return 0;
            }
            if got.signals != 0 {
                self.reap(got.signals & !RESTART_TIMER);
                if got.signals & RESTART_TIMER != 0 {
                    self.run_due_restarts();
                }
                continue;
            }
            if got.closed {
                self.minted.remove(&got.badge);
                continue;
            }
            let received = &handles[..got.handles_len];
            let mut reply = Vec::new();
            let mut reply_handle = None;
            // The unbadged end has every right; a minted one its own.
            let rights = match got.badge {
                0 => access::ALL,
                badge => self.minted.get(&badge).copied().unwrap_or(0),
            };
            let allowed = access::needed(got.label).is_some_and(|needed| rights & needed == needed);
            let result = if !allowed {
                for &handle in received {
                    let _ = oceans_rt::close(handle);
                }
                Err(if access::needed(got.label).is_some() {
                    Status::Denied.into()
                } else {
                    Status::BadRequest.into()
                })
            } else if got.label == op::MINT {
                for &handle in received {
                    let _ = oceans_rt::close(handle);
                }
                self.mint(server, rights, &data[..got.data_len], &mut reply_handle)
            } else {
                self.request(
                    got.label,
                    &data[..got.data_len],
                    received,
                    &mut reply,
                    &mut reply_handle,
                )
            };
            let status = match result {
                Ok(()) => Status::Ok,
                Err(refusal) => {
                    reply.clear();
                    if let Some(text) = refusal.text {
                        reply.extend_from_slice(text.as_bytes());
                    }
                    refusal.status
                }
            };
            reply.truncate(MAX_DATA);
            let sent: &[Handle] = match &reply_handle {
                Some(handle) => core::slice::from_ref(handle),
                None => &[],
            };
            if oceans_rt::ipc_reply_msg(status as u64, &reply, sent).is_err()
                && let Some(handle) = reply_handle
            {
                let _ = oceans_rt::close(handle);
            }
        }
    }

    /// One request. Handles received are consumed (moved on or closed).
    fn request(
        &mut self,
        label: u64,
        data: &[u8],
        received: &[Handle],
        reply: &mut Vec<u8>,
        reply_handle: &mut Option<Handle>,
    ) -> Result<(), Refusal> {
        let close_all = |handles: &[Handle]| {
            for &handle in handles {
                let _ = oceans_rt::close(handle);
            }
        };
        match label {
            op::INSTALL => {
                let [file] = received else {
                    close_all(received);
                    return Err(Status::BadRequest.into());
                };
                let node = Node(*file);
                let result = self.install(&node, reply);
                node.close();
                result
            }
            op::PROPOSE => {
                let [memory] = received else {
                    close_all(received);
                    return Err(Status::BadRequest.into());
                };
                let bytes = read_memory(*memory, data);
                let _ = oceans_rt::close(*memory);
                self.propose(bytes?, reply)
            }
            op::WINDOWS => {
                let [windows] = received else {
                    close_all(received);
                    return Err(Status::BadRequest.into());
                };
                if let Some(old) = self.windows.replace(*windows) {
                    let _ = oceans_rt::close(old);
                }
                say(
                    self.log,
                    format_args!("core: apps given `window` now get a window end"),
                );
                Ok(())
            }
            op::RUN => {
                let out = match received {
                    [] => None,
                    [out] => Some(*out),
                    _ => {
                        close_all(received);
                        return Err(Status::BadRequest.into());
                    }
                };
                self.run(data, out, reply_handle)
            }
            _ => {
                close_all(received);
                match label {
                    op::LIST => self.list(data, reply),
                    op::INFO => self.info(data, reply),
                    op::PERMISSION => self.permission(data, reply),
                    op::STOP => {
                        let id = id_of(data)?;
                        self.app(id)?;
                        if self.stop(id, "stopped by the user") {
                            Ok(())
                        } else {
                            Err(Status::NotRunning.into())
                        }
                    }
                    op::REMOVE => self.remove(data),
                    op::DECIDE => self.decide(data, reply),
                    op::ROLLBACK => self.rollback(data, reply),
                    op::ENABLE => self.enable(data),
                    op::DISABLE => self.disable(data),
                    op::WINDOW_OWNER => self.window_owner(data, reply),
                    op::PENDING => self.pending(reply),
                    op::WEB_BUNDLE => self.web_bundle(data, reply, reply_handle),
                    op::TRUST => self.change_trust(data),
                    op::TRUSTED => {
                        let index = u32_at(data)? as usize;
                        let t = self.trusted.get(index).ok_or(Status::NotFound)?;
                        let mut flags = 0;
                        if index >= self.boot_keys {
                            flags |= 1;
                        }
                        if !t.valid_on(today()) {
                            flags |= 2;
                        }
                        reply.push(flags);
                        let _ = write!(Text(reply), "{}", t.line());
                        Ok(())
                    }
                    op::ACCEPT => self.accept(data, reply),
                    op::AUDIT => {
                        let index = u32_at(data)? as usize;
                        let entry = self.audit.get(index).ok_or(Status::NotFound)?;
                        reply.extend_from_slice(entry.as_bytes());
                        Ok(())
                    }
                    _ => Err(Status::BadRequest.into()),
                }
            }
        }
    }

    /// `MINT` (ADR-0048): a client end with a subset of the caller's
    /// rights.
    fn mint(
        &mut self,
        server: Handle,
        rights: u8,
        data: &[u8],
        reply_handle: &mut Option<Handle>,
    ) -> Result<(), Refusal> {
        let &[wanted] = data else {
            return Err(Status::BadRequest.into());
        };
        if wanted == 0 || wanted & !access::ALL != 0 {
            return Err(Status::BadRequest.into());
        }
        if wanted & !rights != 0 {
            return Err(Status::Denied.into());
        }
        let badge = self.next_badge;
        let end = oceans_rt::endpoint_mint(server, badge).map_err(|_| Status::CannotStart)?;
        self.next_badge += 1;
        self.minted.insert(badge, wanted);
        *reply_handle = Some(end);
        Ok(())
    }

    /// `WINDOW_OWNER` (ADR-0059): the running app holding the window end
    /// badged `badge`.
    fn window_owner(&self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let badge = data
            .try_into()
            .map(u64::from_le_bytes)
            .map_err(|_| Status::BadRequest)?;
        let running = self
            .running
            .iter()
            .flatten()
            .find(|r| r.window == Some(badge))
            .ok_or(Status::NotFound)?;
        let app = self.app(&running.id)?;
        let mut grants = 0;
        if running.granted.contains(&Permission::Window) {
            grants |= display_grant::WINDOW;
        }
        if running.granted.contains(&Permission::Notifications) {
            grants |= display_grant::NOTIFICATIONS;
        }
        reply.push(grants);
        reply.extend_from_slice(
            alloc::format!("{}\0{}\0{}", running.id, app.version, app.name).as_bytes(),
        );
        Ok(())
    }

    fn app(&self, id: &str) -> Result<&App, Refusal> {
        self.apps.get(id).ok_or_else(|| Status::NotFound.into())
    }

    // ---- Packages (ADR-0046) ------------------------------------------------

    fn install(&mut self, file: &Node, reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let bytes = read_package(file)?;
        self.install_bytes(&bytes, reply)
    }

    /// Whether `package` may be installed over what is there: by the same
    /// key, and newer. Returns the version it replaces.
    fn installable(&self, package: &Package<'_>) -> Result<Option<Version>, Refusal> {
        let Some(installed) = self.apps.get(package.manifest.id) else {
            return Ok(None);
        };
        if installed.key != package.key {
            return Err(Status::KeyChanged.into());
        }
        if package.manifest.version <= installed.version {
            return Err(Status::NotNewer.into());
        }
        Ok(Some(installed.version))
    }

    fn install_bytes(&mut self, bytes: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let trust = self.trust();
        let package = Package::open(bytes, &trust).map_err(invalid)?;
        if let Some(request) = package
            .manifest
            .requests()
            .find(|r| r.permission.system_only())
            && !(self.bundled.contains(package.manifest.id) && self.system_key(&package.key))
        {
            return Err(Refusal {
                status: Status::Invalid,
                text: Some(alloc::format!(
                    "{} is only for the system's own apps",
                    request.permission.name()
                )),
            });
        }
        if oceans_package::system_id(package.manifest.id) {
            // A system update (ADR-0071): `update` applies it to the boot
            // partition; it is never an app.
            return Err(Refusal {
                status: Status::Invalid,
                text: Some("a system update, not an app: apply it with `update`".to_owned()),
            });
        }
        let id = package.manifest.id.to_owned();
        let previous = self.installable(&package)?;

        // A folder already there (an app removed with its data kept) is
        // never removed below.
        let existed = self
            .apps_dir
            .open(&id, 0)
            .map(|(node, _)| node.close())
            .is_ok();
        // Written beside the installed version, then renamed into place:
        // the swap is one atomic commit on the Oceans volume.
        let (dir, _) = self
            .apps_dir
            .open(&id, flags::CREATE_DIRECTORY | flags::WRITE)
            .map_err(io)?;
        let placed = (|| {
            write_file(&dir, "incoming.opk", bytes)?;
            if previous.is_some() {
                dir.rename("package.opk", "previous.opk")?;
            }
            dir.rename("incoming.opk", "package.opk")?;
            let (data, _) = dir.open("data", flags::CREATE_DIRECTORY | flags::WRITE)?;
            data.close();
            dir.sync()
        })();
        if placed.is_err() {
            // Nothing half-written stays behind to fill the disk: the
            // partial copy goes, and the folder if this install made it.
            let _ = dir.remove("incoming.opk");
        }
        dir.close();
        if placed.is_err() && !existed {
            let _ = oceans_fs_proto::tree::remove_tree(&self.apps_dir, &id);
        }
        placed.map_err(io)?;

        let app = App::from(&package, previous);
        // Decisions about permissions it no longer asks for are dropped.
        self.decisions
            .retain(|(app_id, permission), _| *app_id != id || app.asks_for(*permission));
        let _ = self.save_decisions();
        let version = app.version;
        let publisher = app.publisher.clone();
        self.apps.insert(id.clone(), app);
        match previous {
            Some(old) => self.record(format_args!("updated {id} {old} -> {version}")),
            None => self.record(format_args!("installed {id} {version} from {publisher}")),
        }
        reply.push(if previous.is_some() {
            outcome::UPDATED
        } else {
            outcome::INSTALLED
        });
        let _ = write!(
            Text(reply),
            "{id}\0{version}\0{}",
            previous.map(|v| alloc::format!("{v}")).unwrap_or_default()
        );
        Ok(())
    }

    // ---- Web apps (ADR-0064) ---------------------------------------------------

    /// `WEB_BUNDLE`: a web app's files, verified as a start verifies a
    /// program, for the bridge to serve.
    fn web_bundle(
        &self,
        data: &[u8],
        reply: &mut Vec<u8>,
        reply_handle: &mut Option<Handle>,
    ) -> Result<(), Refusal> {
        let id = id_of(data)?;
        let app = self.app(id)?;
        if app.runtime != Runtime::Web {
            return Err(Refusal {
                status: Status::CannotStart,
                text: Some("not a web app".to_owned()),
            });
        }
        let (key, version) = (app.key, app.version);
        let bytes = self.read_app_file(id, "package.opk")?;
        let trust = self.trust();
        let package = Package::open(&bytes, &trust).map_err(invalid)?;
        if package.manifest.id != id || package.key != key || package.manifest.version != version {
            return Err(invalid(PackageError::BadSignature));
        }
        let bundle = package.entry();
        let memory = memory_with(bundle).ok_or(Status::CannotStart)?;
        let shared = oceans_rt::duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER);
        let _ = oceans_rt::close(memory);
        *reply_handle = Some(shared.map_err(|_| Status::CannotStart)?);
        reply.extend_from_slice(&(bundle.len() as u64).to_le_bytes());
        let _ = write!(Text(reply), "{version}");
        Ok(())
    }

    // ---- Developers' keys (ADR-0063) -------------------------------------------

    /// `TRUST`: adds or removes a developer's publisher key, perhaps until
    /// a date (ADR-0067); kept in `/system/trust.keys` and audited.
    fn change_trust(&mut self, data: &[u8]) -> Result<(), Refusal> {
        let (&add, line) = data.split_first().ok_or(Status::BadRequest)?;
        let line = core::str::from_utf8(line).map_err(|_| Status::BadRequest)?;
        let refuse = |why: &str| Refusal {
            status: Status::Invalid,
            text: Some(why.to_owned()),
        };
        match add {
            1 => {
                let entry = oceans_package::trust_entries(line)
                    .next()
                    .filter(|_| line.lines().count() == 1)
                    .ok_or_else(|| {
                        refuse("give the key as 64 hex digits, then the publisher's name")
                    })?;
                let key = entry.key;
                if self.trusted.iter().any(|t| t.key == key.key) {
                    return Err(refuse("that key is already trusted"));
                }
                if self.trusted.iter().any(|t| t.publisher == key.publisher) {
                    return Err(refuse("another key is trusted for that publisher name"));
                }
                if !entry.valid_on(today()) {
                    return Err(refuse("that date has passed (or the time is not known)"));
                }
                let added = Trusted {
                    publisher: key.publisher.to_owned(),
                    key: key.key,
                    until: entry.until,
                };
                let limit = added.until_text();
                let (publisher, short) = (added.publisher.clone(), hex32(&added.key));
                self.trusted.push(added);
                if let Err(refusal) = self.save_trust() {
                    self.trusted.pop();
                    return Err(refusal);
                }
                self.record(format_args!(
                    "now trusts key {} for publisher {publisher}{limit} (added at the console)",
                    &short[..16]
                ));
            }
            0 => {
                let index = self
                    .trusted
                    .iter()
                    .position(|t| hex32(&t.key) == line.trim())
                    .ok_or(Status::NotFound)?;
                if index < self.boot_keys {
                    return Err(refuse("keys from the boot image cannot be removed"));
                }
                let removed = self.trusted.remove(index);
                let (publisher, short) = (removed.publisher.clone(), hex32(&removed.key));
                if let Err(refusal) = self.save_trust() {
                    self.trusted.insert(index, removed);
                    return Err(refusal);
                }
                self.record(format_args!(
                    "no longer trusts key {} for publisher {publisher}: its apps no longer start",
                    &short[..16]
                ));
            }
            _ => return Err(Status::BadRequest.into()),
        }
        Ok(())
    }

    fn save_trust(&self) -> Result<(), Refusal> {
        let mut text = String::from(
            "# Developers' publisher keys the user trusts (ADR-0063, ADR-0067):\n\
             # `KEY-HEX [until=YYYY-MM-DD] PUBLISHER`.\n",
        );
        for trusted in &self.trusted[self.boot_keys..] {
            let _ = writeln!(text, "{}", trusted.line());
        }
        write_file(&self.system_dir, TRUST_FILE, text.as_bytes()).map_err(io)
    }

    // ---- The Store (ADR-0061) --------------------------------------------------

    /// `PROPOSE`: a verified package waits for the user's confirmation on
    /// the device. Its bytes are kept, so what is installed is exactly
    /// what the user was shown.
    fn propose(&mut self, bytes: Vec<u8>, reply: &mut Vec<u8>) -> Result<(), Refusal> {
        if self.proposal.is_some() {
            return Err(Status::Pending.into());
        }
        let trust = self.trust();
        let package = Package::open(&bytes, &trust).map_err(invalid)?;
        let previous = self.installable(&package)?;
        let manifest = &package.manifest;
        let mut permissions = String::new();
        for (i, request) in manifest.requests().enumerate() {
            if i > 0 {
                permissions.push(',');
            }
            permissions.push_str(request.permission.name());
        }
        // The description last: a long one is what gets cut.
        let summary = alloc::format!(
            "{}\0{}\0{}\0{}\0{permissions}\0{}\0{}",
            manifest.id,
            manifest.version,
            manifest.name,
            manifest.publisher,
            previous.map(|v| alloc::format!("{v}")).unwrap_or_default(),
            manifest.description,
        );
        let id = manifest.id.to_owned();
        let version = manifest.version;
        let publisher = manifest.publisher.to_owned();
        self.next_proposal = self.next_proposal.wrapping_add(1).max(1);
        let number = self.next_proposal;
        self.proposal = Some(Proposal {
            number,
            bytes,
            id: id.clone(),
            version,
            summary,
        });
        self.record(format_args!(
            "proposed installing {id} {version} from {publisher} (waiting for the user on the device)"
        ));
        reply.extend_from_slice(&number.to_le_bytes());
        Ok(())
    }

    /// `PENDING`: the proposal waiting, if any.
    fn pending(&self, reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let proposal = self.proposal.as_ref().ok_or(Status::NotFound)?;
        reply.extend_from_slice(&proposal.number.to_le_bytes());
        reply.extend_from_slice(proposal.summary.as_bytes());
        Ok(())
    }

    /// `ACCEPT`: the user's answer to proposal `number`.
    fn accept(&mut self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let &[a, b, c, d, install] = data else {
            return Err(Status::BadRequest.into());
        };
        let number = u32::from_le_bytes([a, b, c, d]);
        if self.proposal.as_ref().is_none_or(|p| p.number != number) {
            return Err(Status::NotFound.into());
        }
        let Some(proposal) = self.proposal.take() else {
            return Err(Status::NotFound.into());
        };
        let (id, version) = (&proposal.id, proposal.version);
        if install != 1 {
            self.record(format_args!(
                "install of {id} {version} declined on the device"
            ));
            return Ok(());
        }
        self.record(format_args!(
            "install of {id} {version} confirmed on the device"
        ));
        self.install_bytes(&proposal.bytes, reply)
    }

    fn rollback(&mut self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let id = id_of(data)?;
        let current = self.app(id)?;
        let key = current.key;
        let newer = current.version;
        let bytes = self
            .read_app_file(id, "previous.opk")
            .map_err(|_| Refusal::from(Status::NoRollback))?;
        let trust = self.trust();
        let package = Package::open(&bytes, &trust).map_err(invalid)?;
        if package.key != key || package.manifest.id != id {
            return Err(Status::KeyChanged.into());
        }
        let (dir, _) = self.apps_dir.open(id, flags::WRITE).map_err(io)?;
        let swapped = dir
            .rename("previous.opk", "package.opk")
            .and_then(|()| dir.sync());
        dir.close();
        swapped.map_err(io)?;
        let app = App::from(&package, None);
        let version = app.version;
        self.decisions
            .retain(|(app_id, permission), _| app_id != id || app.asks_for(*permission));
        let _ = self.save_decisions();
        let id = id.to_owned();
        self.apps.insert(id.clone(), app);
        self.record(format_args!("rolled back {id} {newer} -> {version}"));
        let _ = write!(Text(reply), "{version}");
        Ok(())
    }

    fn remove(&mut self, data: &[u8]) -> Result<(), Refusal> {
        let (&keep, id) = data.split_first().ok_or(Status::BadRequest)?;
        let id = id_of(id)?.to_owned();
        self.app(&id)?;
        self.stop(&id, "removed");
        let removed = if keep != 0 {
            let (dir, _) = self.apps_dir.open(&id, flags::WRITE).map_err(io)?;
            let result = ["package.opk", "previous.opk", "incoming.opk"]
                .into_iter()
                .try_for_each(|name| match dir.remove(name) {
                    Err(FsError::Status(oceans_fs_proto::Status::NotFound)) => Ok(()),
                    other => other,
                });
            dir.close();
            result
        } else {
            oceans_fs_proto::tree::remove_tree(&self.apps_dir, &id)
        };
        removed.and_then(|()| self.apps_dir.sync()).map_err(io)?;
        self.apps.remove(&id);
        self.decisions.retain(|(app_id, _), _| *app_id != id);
        if self.enabled.remove(&id) {
            let _ = self.save_enabled();
        }
        self.pending_restarts.retain(|(pending, _)| *pending != id);
        let _ = self.save_decisions();
        self.record(format_args!(
            "removed {id}{}",
            if keep != 0 { " (its data kept)" } else { "" }
        ));
        Ok(())
    }

    // ---- Queries ----------------------------------------------------------------

    fn list(&self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let index = u32_at(data)? as usize;
        let (id, app) = self.apps.iter().nth(index).ok_or(Status::NotFound)?;
        reply.push(u8::from(self.slot_of(id).is_some()));
        let _ = write!(Text(reply), "{id}\0{}\0{}", app.version, app.name);
        Ok(())
    }

    fn info(&self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let (&which, id) = data.split_first().ok_or(Status::BadRequest)?;
        let id = id_of(id)?;
        let app = self.app(id)?;
        let mut text = Text(reply);
        let _ = match which {
            field::NAME => text.write_str(&app.name),
            field::VERSION => write!(text, "{}", app.version),
            field::PUBLISHER => text.write_str(&app.publisher),
            field::DESCRIPTION => text.write_str(&app.description),
            field::CHANNEL => text.write_str(&app.channel),
            field::KEY => app.key[..8]
                .iter()
                .try_for_each(|byte| write!(text, "{byte:02x}")),
            field::STATE => text.write_str(if self.slot_of(id).is_some() {
                "running"
            } else {
                "installed"
            }),
            field::PREVIOUS => match app.previous {
                Some(version) => write!(text, "{version}"),
                None => Ok(()),
            },
            field::SOURCE => text.write_str(app.source.as_deref().unwrap_or("")),
            field::KIND => text.write_str(match (app.service, self.enabled.contains(id)) {
                (false, _) => "app",
                (true, false) => "service",
                (true, true) => "service, enabled (starts at boot)",
            }),
            field::RUNTIME => text.write_str(app.runtime.name()),
            _ => return Err(Status::BadRequest.into()),
        };
        Ok(())
    }

    fn permission(&self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let (&index, id) = data.split_first().ok_or(Status::BadRequest)?;
        let id = id_of(id)?;
        let app = self.app(id)?;
        let (permission, reason) = app
            .requests
            .get(usize::from(index))
            .ok_or(Status::NotFound)?;
        reply.push(permission_index(*permission));
        reply.push(self.decision(id, *permission) as u8);
        reply.extend_from_slice(reason.as_bytes());
        Ok(())
    }

    // ---- Permissions (ADR-0047) -------------------------------------------------

    fn decide(&mut self, data: &[u8], reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let [permission, allow, by, id @ ..] = data else {
            return Err(Status::BadRequest.into());
        };
        let permission = *Permission::ALL
            .get(usize::from(*permission))
            .ok_or(Status::BadRequest)?;
        let id = id_of(id)?.to_owned();
        let app = self.app(&id)?;
        if !app.asks_for(permission) || permission.automatic() {
            return Err(Status::BadRequest.into());
        }
        let decided = match *allow {
            decision::DENY => Some(false),
            decision::ALLOW => Some(true),
            decision::FORGET => None,
            _ => return Err(Status::BadRequest.into()),
        };
        let allow = decided == Some(true);
        // A decision that cannot be stored is not applied.
        let key = (id.clone(), permission);
        let before = match decided {
            Some(allowed) => self.decisions.insert(key.clone(), allowed),
            None => self.decisions.remove(&key),
        };
        if let Err(refusal) = self.save_decisions() {
            match before {
                Some(old) => self.decisions.insert(key, old),
                None => self.decisions.remove(&key),
            };
            return Err(refusal);
        }
        let how = match *by {
            source::PROMPT => "at its prompt",
            source::DIALOG => "in a permission dialog",
            source::SETTINGS => "in Settings",
            _ => "by command",
        };
        self.record(format_args!(
            "{} {id} {} ({how})",
            match decided {
                Some(true) => "allowed",
                Some(false) => "denied",
                None => "reset (ask again)",
            },
            permission.name()
        ));
        // A capability cannot be taken back: the process holding it goes.
        let holds = self
            .slot_of(&id)
            .and_then(|slot| self.running[slot].as_ref())
            .is_some_and(|running| running.granted.contains(&permission));
        let stopped = !allow && holds && self.stop(&id, "a permission it used was revoked");
        reply.push(u8::from(stopped));
        Ok(())
    }

    // ---- Lifecycle (ADR-0045) ---------------------------------------------------

    fn slot_of(&self, id: &str) -> Option<usize> {
        self.running
            .iter()
            .position(|r| r.as_ref().is_some_and(|r| r.id == id))
    }

    fn run(
        &mut self,
        data: &[u8],
        out: Option<Handle>,
        reply_handle: &mut Option<Handle>,
    ) -> Result<(), Refusal> {
        let result = self.start_app(data, out);
        match result {
            Ok((process, detach)) => {
                if !detach {
                    match oceans_rt::duplicate(process, rights::WAIT | rights::TRANSFER) {
                        Ok(handle) => *reply_handle = Some(handle),
                        Err(_) => return Err(Status::CannotStart.into()),
                    }
                }
                Ok(())
            }
            Err(refusal) => Err(refusal),
        }
    }

    /// Starts an app; `out` is consumed either way.
    fn start_app(&mut self, data: &[u8], out: Option<Handle>) -> Result<(Handle, bool), Refusal> {
        let mut out = out;
        let result = (|| {
            let [flags_byte, id_len, rest @ ..] = data else {
                return Err(Status::BadRequest.into());
            };
            let id_len = usize::from(*id_len);
            let id = id_of(rest.get(..id_len).ok_or(Status::BadRequest)?)?.to_owned();
            let args = core::str::from_utf8(&rest[id_len..]).map_err(|_| Status::BadRequest)?;
            let app = self.app(&id)?;
            if app.runtime == Runtime::Web {
                return Err(Refusal {
                    status: Status::CannotStart,
                    text: Some(WEB_APP.to_owned()),
                });
            }
            let detach = flags_byte & run_flags::DETACH != 0 || app.service;
            let service = app.service;
            if self.slot_of(&id).is_some() {
                return Err(Status::AlreadyRunning.into());
            }
            if app
                .requests
                .iter()
                .any(|(p, _)| self.decision(&id, *p) == Decision::Undecided)
            {
                return Err(Status::NeedsConsent.into());
            }
            let stale = self.stale;
            let slot = self
                .running
                .iter()
                .enumerate()
                .position(|(slot, r)| r.is_none() && stale & (1 << slot) == 0)
                .ok_or(Status::CannotStart)?;
            let granted: Vec<Permission> = app
                .requests
                .iter()
                .map(|(p, _)| *p)
                .filter(|p| {
                    matches!(
                        self.decision(&id, *p),
                        Decision::Automatic | Decision::Allowed
                    )
                })
                .collect();
            let key = app.key;
            let version = app.version;

            // Verified again: the package on disk is what runs.
            let bytes = self.read_app_file(&id, "package.opk")?;
            let trust = self.trust();
            let package = Package::open(&bytes, &trust).map_err(invalid)?;
            if package.manifest.id != id
                || package.key != key
                || package.manifest.version != version
            {
                return Err(invalid(PackageError::BadSignature));
            }
            let entry_name = package.manifest.entry;
            let program = memory_with(package.entry()).ok_or(Status::CannotStart)?;
            let (image, module) = match package.manifest.runtime {
                Runtime::Native => (program, None),
                // The Go host runs it (ADR-0052), with the app's
                // capabilities and the program as a read-only module.
                Runtime::Wasm => {
                    let module = oceans_rt::duplicate(
                        program,
                        rights::READ | rights::MAP | rights::TRANSFER,
                    );
                    let _ = oceans_rt::close(program);
                    let module = module.map_err(|_| Status::CannotStart)?;
                    match self.gohost {
                        Some(host) => (host, Some(module)),
                        None => {
                            let _ = oceans_rt::close(module);
                            return Err(Refusal {
                                status: Status::CannotStart,
                                text: Some("this system has no Go host (gohost)".to_owned()),
                            });
                        }
                    }
                }
                // Refused above; never spawned.
                Runtime::Web => {
                    let _ = oceans_rt::close(program);
                    return Err(Refusal {
                        status: Status::CannotStart,
                        text: Some(WEB_APP.to_owned()),
                    });
                }
            };
            // A window end of its own, badged so the display service can ask
            // whose it is (ADR-0059).
            let window = ((granted.contains(&Permission::Window)
                || granted.contains(&Permission::Notifications))
                && self.windows.is_some())
            .then(|| {
                self.next_window_badge += 1;
                self.next_window_badge - 1
            });
            let launch = Launch {
                id: &id,
                name: entry_name,
                info: &alloc::format!("{id} {version}"),
                image,
                module,
                granted: &granted,
                args,
                service,
                window,
            };
            let process = self.spawn(&launch, &mut out);
            // The Go host's image is kept for the next `wasm` app.
            if module.is_none() {
                let _ = oceans_rt::close(image);
            }
            let process = process?;
            let _ = oceans_rt::process_watch(process, self.notification, 1 << slot);
            self.running[slot] = Some(Running {
                id: id.clone(),
                process,
                granted: granted.clone(),
                window,
            });
            let mut names = String::new();
            for (i, permission) in granted.iter().enumerate() {
                let _ = write!(
                    names,
                    "{}{}",
                    if i > 0 { ", " } else { "" },
                    permission.name()
                );
            }
            if names.is_empty() {
                names.push_str("no permissions");
            }
            self.record(format_args!("started {id} {version} with {names}"));
            Ok((process, detach))
        })();
        if let Some(out) = out {
            let _ = oceans_rt::close(out);
        }
        result
    }

    /// Spawns the app with its program module (`wasm`), a handle per
    /// granted permission, its arguments and the directory describing
    /// them.
    fn spawn(&mut self, launch: &Launch<'_>, out: &mut Option<Handle>) -> Result<Handle, Refusal> {
        let Launch {
            id,
            name,
            info,
            image,
            module,
            granted,
            args,
            service,
            window,
        } = *launch;
        let mut handles: Vec<Handle> = Vec::new();
        let mut directory = String::new();
        let failed = |handles: &[Handle]| {
            for &handle in handles {
                let _ = oceans_rt::close(handle);
            }
            Refusal::from(Status::CannotStart)
        };
        // First, so every failure below closes it; the Go host runs the
        // first `module` of its directory (entry names have no spaces).
        if let Some(module) = module {
            let _ = writeln!(directory, "{} module {name}", handles.len());
            handles.push(module);
        }
        for &permission in granted {
            // What the system's settings show (ADR-0096): reader ends of
            // the network and of the sound, for the system's own apps only.
            if permission == Permission::SystemSettings {
                if !self.system_app(id) {
                    continue;
                }
                let net = self
                    .net
                    .and_then(|h| oceans_net_proto::reader(h).ok())
                    .map(|h| (h, "net-info"));
                let audio = self
                    .audio
                    .and_then(|h| oceans_audio_proto::reader(h).ok())
                    .map(|h| (h, "audio-info"));
                for (given, what) in [(net, "the network"), (audio, "the sound")] {
                    match given {
                        Some((handle, label)) => {
                            let _ = writeln!(directory, "{} use {label}", handles.len());
                            handles.push(handle);
                        }
                        None => say(
                            self.log,
                            format_args!("core: {id}: system-settings: {what} unavailable"),
                        ),
                    }
                }
                continue;
            }
            let given = match permission {
                Permission::Console => out.take().map(|out| (out, "console", "out")),
                Permission::Storage => self
                    .apps_dir
                    .walk(
                        &alloc::format!("{id}/data"),
                        flags::CREATE_DIRECTORY | flags::WRITE,
                    )
                    .ok()
                    .map(|(node, _)| (node.0, "use", "storage")),
                Permission::Files => self
                    .root
                    .open("home", flags::WRITE)
                    .ok()
                    .map(|(node, _)| (node.0, "use", "files")),
                Permission::SystemInfo => self
                    .sysinfo
                    .and_then(|h| oceans_rt::duplicate(h, rights::READ | rights::TRANSFER).ok())
                    .map(|h| (h, "sysinfo", "sysinfo")),
                Permission::Network => self
                    .net
                    .and_then(|h| oceans_rt::duplicate(h, rights::SEND | rights::TRANSFER).ok())
                    .map(|h| (h, "use", "net")),
                Permission::Pointer => self
                    .input
                    .and_then(|h| oceans_rt::duplicate(h, rights::SEND | rights::TRANSFER).ok())
                    .map(|h| (h, "use", "input")),
                // One display end for both, after the loop.
                Permission::Window | Permission::Notifications => continue,
                // A Core end that may query, decide, manage and audit
                // (ADR-0081), only for the system's own apps.
                Permission::ManageApps if self.system_app(id) => {
                    let badge = self.next_badge;
                    oceans_rt::endpoint_mint(self.server, badge)
                        .ok()
                        .map(|end| {
                            self.next_badge += 1;
                            self.minted.insert(
                                badge,
                                access::QUERY | access::DECIDE | access::MANAGE | access::AUDIT,
                            );
                            (end, "use", "core")
                        })
                }
                Permission::ManageApps => None,
                // A player end: playing sessions only, never the input
                // (ADR-0094).
                Permission::Sound => self
                    .audio
                    .and_then(|h| oceans_audio_proto::player(h).ok())
                    .map(|h| (h, "use", "audio")),
                // Given above, as two ends.
                Permission::SystemSettings => continue,
            };
            match given {
                Some((handle, kind, label)) => {
                    let _ = writeln!(directory, "{} {kind} {label}", handles.len());
                    handles.push(handle);
                }
                // A permission the system cannot provide right now (no
                // console given, no network service): the app runs without.
                None => say(
                    self.log,
                    format_args!("core: {id}: {} unavailable", permission.name()),
                ),
            }
        }
        // The display end (ADR-0059, ADR-0065): windows and notifications,
        // badged so the display service can ask whose it is.
        if let Some(badge) = window {
            match self
                .windows
                .and_then(|server| oceans_rt::endpoint_mint(server, badge).ok())
            {
                Some(handle) => {
                    let _ = writeln!(directory, "{} use windows", handles.len());
                    handles.push(handle);
                }
                None => say(
                    self.log,
                    format_args!("core: {id}: the display is unavailable"),
                ),
            }
        }
        // A service writes to the system log (it has no terminal).
        if service {
            match oceans_rt::duplicate(self.log, rights::WRITE | rights::TRANSFER) {
                Ok(handle) => {
                    let _ = writeln!(directory, "{} log log", handles.len());
                    handles.push(handle);
                }
                Err(_) => return Err(failed(&handles)),
            }
        }
        // Its own identity: `ID VERSION`.
        match oceans_rt::publish_text(info.as_bytes()) {
            Ok(handle) => {
                let _ = writeln!(directory, "{} app info", handles.len());
                handles.push(handle);
            }
            Err(_) => return Err(failed(&handles)),
        }
        if !args.is_empty() {
            match oceans_rt::publish_text(args.as_bytes()) {
                Ok(handle) => {
                    let _ = writeln!(directory, "{} args args", handles.len());
                    handles.push(handle);
                }
                Err(_) => return Err(failed(&handles)),
            }
        }
        let _ = writeln!(directory, "{} directory handles", handles.len());
        match oceans_rt::publish_text(directory.as_bytes()) {
            Ok(handle) => handles.push(handle),
            Err(_) => return Err(failed(&handles)),
        }
        if handles.len() > MAX_APP_HANDLES + 1 {
            return Err(failed(&handles));
        }
        oceans_rt::process_spawn_named(image, 0, &handles, 0, name).map_err(|error| {
            say(
                self.log,
                format_args!("core: {id}: cannot start: {error:?}"),
            );
            failed(&handles)
        })
    }

    /// Kills a running app and waits for it to end; `false` if it was not
    /// running.
    fn stop(&mut self, id: &str, why: &str) -> bool {
        let Some(slot) = self.slot_of(id) else {
            return false;
        };
        let Some(running) = self.running[slot].take() else {
            return false;
        };
        let _ = oceans_rt::process_kill(running.process);
        let code = oceans_rt::process_wait(running.process);
        let _ = oceans_rt::close(running.process);
        // Its exit signal is still to come: the slot waits for it.
        self.stale |= 1 << slot;
        self.record(format_args!(
            "stopped {id}: {why} (exit {})",
            code.unwrap_or(oceans_rt::EXIT_KILLED)
        ));
        true
    }

    /// Stops every running app and service (ADR-0086).
    fn stop_all(&mut self) {
        let ids: Vec<String> = self
            .running
            .iter()
            .flatten()
            .map(|running| running.id.clone())
            .collect();
        for id in &ids {
            self.stop(id, "the system is stopping");
        }
        say(
            self.log,
            format_args!("core: {} apps stopped; the system is stopping", ids.len()),
        );
    }

    /// Apps whose exit was signalled (one bit per slot).
    fn reap(&mut self, bits: u64) {
        for slot in 0..MAX_RUNNING {
            if bits & (1 << slot) == 0 {
                continue;
            }
            // The exit of an app `stop` already waited for.
            if self.stale & (1 << slot) != 0 {
                self.stale &= !(1 << slot);
                continue;
            }
            if let Some(running) = self.running[slot].take() {
                let code = oceans_rt::process_wait(running.process).unwrap_or(-1);
                let _ = oceans_rt::close(running.process);
                say(
                    self.log,
                    format_args!("core: {} exited with code {code}", running.id),
                );
                let failed = code != 0 && code != oceans_rt::EXIT_KILLED;
                if failed && self.enabled.contains(&running.id) {
                    self.schedule_restart(&running.id, code);
                }
            }
        }
    }
}

/// The index `Permission::ALL` gives `permission` (its protocol number).
fn permission_index(permission: Permission) -> u8 {
    Permission::ALL
        .iter()
        .position(|&p| p == permission)
        .unwrap_or(0) as u8
}

fn id_of(bytes: &[u8]) -> Result<&str, Refusal> {
    core::str::from_utf8(bytes)
        .ok()
        .filter(|id| oceans_package::valid_id(id))
        .ok_or_else(|| Status::BadRequest.into())
}

fn u32_at(data: &[u8]) -> Result<u32, Refusal> {
    data.get(..4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| Status::BadRequest.into())
}

/// Formatting into a byte vector.
struct Text<'a>(&'a mut Vec<u8>);

impl core::fmt::Write for Text<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// A read-only memory object holding `bytes` (a program image).
fn memory_with(bytes: &[u8]) -> Option<Handle> {
    let memory = oceans_rt::memory_create(bytes.len().max(1) as u64).ok()?;
    let Ok(base) = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE) else {
        let _ = oceans_rt::close(memory);
        return None;
    };
    // SAFETY: just mapped at least `bytes.len()` writable bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), base, bytes.len()) };
    let _ = oceans_rt::memory_unmap(base);
    Some(memory)
}

fn read_path(dir: &Node, name: &str) -> Result<Vec<u8>, FsError> {
    let (file, kind) = dir.open(name, 0)?;
    let result = if kind == Kind::File {
        file.stat().and_then(|stat| read_node(&file, stat.size))
    } else {
        Err(FsError::Status(oceans_fs_proto::Status::IsADirectory))
    };
    file.close();
    result
}

/// The whole file, through a shared buffer.
fn read_node(file: &Node, size: u64) -> Result<Vec<u8>, FsError> {
    let mut bytes = alloc::vec![0u8; size as usize];
    let shared = file.attach(FILE_BUFFER)?;
    let mut done = 0;
    while done < bytes.len() {
        match file.read_shared(&shared, done as u64, &mut bytes[done..])? {
            0 => break,
            n => done += n,
        }
    }
    bytes.truncate(done);
    Ok(bytes)
}

/// Replaces file `name` in `dir` with `bytes`, durably.
fn write_file(dir: &Node, name: &str, bytes: &[u8]) -> Result<(), FsError> {
    let (file, _) = dir.open(name, flags::CREATE_FILE | flags::WRITE)?;
    let result = (|| {
        file.truncate(0)?;
        if !bytes.is_empty() {
            let shared =
                file.attach(FILE_BUFFER.min(bytes.len().max(oceans_fs_proto::MIN_SHARED)))?;
            file.write_shared(&shared, 0, bytes)?;
        }
        file.sync()
    })();
    file.close();
    result
}

/// Appends `bytes` to file `name` in `dir`, durably.
fn append_file(dir: &Node, name: &str, bytes: &[u8]) -> Result<(), FsError> {
    let (file, _) = dir.open(name, flags::CREATE_FILE | flags::WRITE)?;
    let result = file
        .stat()
        .and_then(|stat| file.write_all(stat.size, bytes))
        .and_then(|()| file.sync());
    file.close();
    result
}

/// `YYYY-MM-DD HH:MM:SS` UTC, or the uptime if the clock is unknown.
fn timestamp() -> String {
    let mut text = String::new();
    match oceans_rt::unix_time_ms() {
        Some(ms) => {
            let seconds = ms / 1000;
            let (days, rest) = (seconds / 86_400, seconds % 86_400);
            let (year, month, day) = civil_from_days(days as i64);
            let _ = write!(
                text,
                "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
                rest / 3600,
                rest / 60 % 60,
                rest % 60
            );
        }
        None => {
            let _ = write!(text, "+{}ms", oceans_rt::clock_ms());
        }
    }
    text
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A package file's bytes, refused past 32 MiB.
fn read_package(file: &Node) -> Result<Vec<u8>, Refusal> {
    let size = file.stat().map_err(io)?.size;
    if size > MAX_PACKAGE {
        return Err(Refusal {
            status: Status::Invalid,
            text: Some("the package is larger than 32 MiB".to_owned()),
        });
    }
    read_node(file, size).map_err(io)
}

/// The `length` bytes (`data`, a u64) of a memory object holding a
/// package; refused past 32 MiB or past the object's end.
fn read_memory(memory: Handle, data: &[u8]) -> Result<Vec<u8>, Refusal> {
    let length = data
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| Status::BadRequest)?;
    let size = oceans_rt::memory_size(memory).map_err(|_| Status::BadRequest)?;
    if length == 0 || length > size {
        return Err(Status::BadRequest.into());
    }
    if length > MAX_PACKAGE {
        return Err(Refusal {
            status: Status::Invalid,
            text: Some("the package is larger than 32 MiB".to_owned()),
        });
    }
    let base = oceans_rt::memory_map(memory, 0, prot::READ).map_err(|_| Status::BadRequest)?;
    // SAFETY: the object (`size` bytes, `length <= size`) is mapped
    // readable at `base` until the unmap below.
    let bytes = unsafe { core::slice::from_raw_parts(base, length as usize) }.to_vec();
    let _ = oceans_rt::memory_unmap(base);
    Ok(bytes)
}

/// A key as 64 lowercase hex digits.
fn hex32(key: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in key {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
