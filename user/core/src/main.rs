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
//!
//! Manifest grants: `log`, `provide = core`, `use = fs` (the root: core
//! keeps `/apps`, `/system` and hands out `/home`), and what it passes on:
//! `use = net`, `use = input`, `sysinfo`; `module:trust.keys` (the trusted
//! publisher keys, from the boot image).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_core_proto::{Decision, MAX_DATA, Status, field, op, outcome, run_flags, source};
use oceans_fs_proto::{FsError, Kind, Node, flags};
use oceans_package::{Package, PackageError, Permission, TrustedKey, Version};
use oceans_rt::{Buffer, Directory, Handle, Start, prot, rights};

oceans_rt::entry!(main);

/// Most apps running at once (one notification bit each).
const MAX_RUNNING: usize = 32;
/// Largest package accepted.
const MAX_PACKAGE: u64 = 32 << 20;
/// Bytes moved per filesystem request.
const FILE_BUFFER: usize = 128 * 1024;
/// Audit entries kept in memory for `AUDIT` (all go to the log file).
const AUDIT_KEPT: usize = 64;
/// Handles an app may get: one per permission, its identity, arguments,
/// directory.
const MAX_APP_HANDLES: usize = 9;

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
    let Ok(notification) = oceans_rt::notification_create() else {
        return EXIT_BAD_START;
    };
    if oceans_rt::endpoint_bind(server, notification).is_err() {
        return EXIT_BAD_START;
    }
    core.notification = notification;
    core.serve(server)
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
    image: Handle,
    granted: &'a [Permission],
    args: &'a str,
}

struct Running {
    id: String,
    process: Handle,
    /// The permissions it was started with.
    granted: Vec<Permission>,
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
    trusted: Vec<(String, [u8; 32])>,
    apps: BTreeMap<String, App>,
    /// The user's decisions: `true` allowed, `false` denied.
    decisions: BTreeMap<(String, Permission), bool>,
    running: [Option<Running>; MAX_RUNNING],
    notification: Handle,
    audit: VecDeque<String>,
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
        let trusted: Vec<(String, [u8; 32])> = oceans_package::trusted_keys(trust_text)
            .map(|t| (t.publisher.to_owned(), t.key))
            .collect();
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
            trusted,
            apps: BTreeMap::new(),
            decisions: BTreeMap::new(),
            running: [const { None }; MAX_RUNNING],
            notification: Handle(0),
            audit: VecDeque::new(),
        };
        core.load_apps();
        core.load_decisions();
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

    fn trust(&self) -> Vec<TrustedKey<'_>> {
        self.trusted
            .iter()
            .map(|(publisher, key)| TrustedKey {
                publisher,
                key: *key,
            })
            .collect()
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

    fn decision(&self, id: &str, permission: Permission) -> Decision {
        if permission.automatic() {
            return Decision::Automatic;
        }
        match self.decisions.get(&(id.to_owned(), permission)) {
            Some(true) => Decision::Allowed,
            Some(false) => Decision::Denied,
            None => Decision::Undecided,
        }
    }

    fn serve(&mut self, server: Handle) -> i64 {
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
            if got.signals != 0 {
                self.reap(got.signals);
                continue;
            }
            if got.closed {
                continue;
            }
            let received = &handles[..got.handles_len];
            let mut reply = Vec::new();
            let mut reply_handle = None;
            let result = self.request(
                got.label,
                &data[..got.data_len],
                received,
                &mut reply,
                &mut reply_handle,
            );
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

    fn app(&self, id: &str) -> Result<&App, Refusal> {
        self.apps.get(id).ok_or_else(|| Status::NotFound.into())
    }

    // ---- Packages (ADR-0046) ------------------------------------------------

    fn install(&mut self, file: &Node, reply: &mut Vec<u8>) -> Result<(), Refusal> {
        let size = file.stat().map_err(io)?.size;
        if size > MAX_PACKAGE {
            return Err(Refusal {
                status: Status::Invalid,
                text: Some("the package is larger than 32 MiB".to_owned()),
            });
        }
        let bytes = read_node(file, size).map_err(io)?;
        let trust = self.trust();
        let package = Package::open(&bytes, &trust).map_err(invalid)?;
        let id = package.manifest.id.to_owned();
        let installed = self.apps.get(&id);
        if let Some(installed) = installed {
            if installed.key != package.key {
                return Err(Status::KeyChanged.into());
            }
            if package.manifest.version <= installed.version {
                return Err(Status::NotNewer.into());
            }
        }
        let previous = installed.map(|app| app.version);

        // Written beside the installed version, then renamed into place:
        // the swap is one atomic commit on the Oceans volume.
        let (dir, _) = self
            .apps_dir
            .open(&id, flags::CREATE_DIRECTORY | flags::WRITE)
            .map_err(io)?;
        let placed = (|| {
            write_file(&dir, "incoming.opk", &bytes)?;
            if previous.is_some() {
                dir.rename("package.opk", "previous.opk")?;
            }
            dir.rename("incoming.opk", "package.opk")?;
            let (data, _) = dir.open("data", flags::CREATE_DIRECTORY | flags::WRITE)?;
            data.close();
            dir.sync()
        })();
        dir.close();
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
        let allow = *allow != 0;
        // A decision that cannot be stored is not applied.
        let key = (id.clone(), permission);
        let before = self.decisions.insert(key.clone(), allow);
        if let Err(refusal) = self.save_decisions() {
            match before {
                Some(old) => self.decisions.insert(key, old),
                None => self.decisions.remove(&key),
            };
            return Err(refusal);
        }
        let how = if *by == source::PROMPT {
            "at its prompt"
        } else {
            "by command"
        };
        self.record(format_args!(
            "{} {id} {} ({how})",
            if allow { "allowed" } else { "denied" },
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
            let detach = flags_byte & run_flags::DETACH != 0;
            let app = self.app(&id)?;
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
            let slot = self
                .running
                .iter()
                .position(Option::is_none)
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
            let image = memory_with(package.entry()).ok_or(Status::CannotStart)?;
            let launch = Launch {
                id: &id,
                name: entry_name,
                info: &alloc::format!("{id} {version}"),
                image,
                granted: &granted,
                args,
            };
            let process = self.spawn(&launch, &mut out);
            let _ = oceans_rt::close(image);
            let process = process?;
            let _ = oceans_rt::process_watch(process, self.notification, 1 << slot);
            self.running[slot] = Some(Running {
                id: id.clone(),
                process,
                granted: granted.clone(),
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

    /// Spawns the app with a handle per granted permission, its arguments
    /// and the directory describing them.
    fn spawn(&self, launch: &Launch<'_>, out: &mut Option<Handle>) -> Result<Handle, Refusal> {
        let Launch {
            id,
            name,
            info,
            image,
            granted,
            args,
        } = *launch;
        let mut handles: Vec<Handle> = Vec::new();
        let mut directory = String::new();
        let failed = |handles: &[Handle]| {
            for &handle in handles {
                let _ = oceans_rt::close(handle);
            }
            Refusal::from(Status::CannotStart)
        };
        for &permission in granted {
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
        self.record(format_args!(
            "stopped {id}: {why} (exit {})",
            code.unwrap_or(oceans_rt::EXIT_KILLED)
        ));
        true
    }

    /// Apps whose exit was signalled (one bit per slot).
    fn reap(&mut self, bits: u64) {
        for slot in 0..MAX_RUNNING {
            if bits & (1 << slot) == 0 {
                continue;
            }
            if let Some(running) = self.running[slot].take() {
                let code = oceans_rt::process_wait(running.process).unwrap_or(-1);
                let _ = oceans_rt::close(running.process);
                say(
                    self.log,
                    format_args!("core: {} exited with code {code}", running.id),
                );
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
