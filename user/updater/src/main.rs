//! The `updater` service (ADR-0158): finds a newer Oceans, downloads it,
//! checks it and installs it in the slot not in use, when the user asks.
//! It never restarts the machine, and by itself it only ever *looks*.
//!
//! - **Where:** the source is a URL ending in `/` holding a release as
//!   `oceans release` publishes it (ADR-0075): `SHA256SUMS`, its signature
//!   `SHA256SUMS.sig`, and the system package. By default the published
//!   releases' latest; `/system/updates/source` may name a mirror.
//! - **Trust:** the checksums must be signed by a key in `/bin/update.keys`,
//!   the package must match its checksum, and `oceans-update` verifies the
//!   package's own signature and that it is newer before writing anything
//!   (ADR-0071). The transport adds nothing to trust.
//! - **Asking:** its clients are Settings (the `system-update` permission)
//!   and the shell; every request is answered at once and the download goes
//!   on between requests, a piece per tick.
//! - **By itself:** with "check automatically" on (the default, kept in
//!   `/system/updates/auto`), it looks once a day, the first time ten
//!   minutes after the start. That is the only connection Oceans makes
//!   unasked besides the network time (ADR-0146, P1), and it can be
//!   turned off.
//!
//! Grants: `log`, `provide = updater`, `use = net`, `use = fs` (the root:
//! `/bin`, `/system` and the boot partition), and, where there is one, the
//! stick (`use = usbdisk`) for a Secure Boot system (ADR-0092).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_core_proto::updater::{self, state};
use oceans_fs_proto::{Kind, Node, Shared, flags};
use oceans_package::sums::{self, Listed};
use oceans_package::{TrustedKey, Version};
use oceans_rt::{Buffer, Directory, Error, Handle, Start};
use sha2::{Digest, Sha256};

oceans_rt::entry!(main);

/// Notification bits: the timer (a download's next piece, or the next
/// look).
const TIMER: u64 = 1;
/// How often a download takes its next piece.
const TICK_MS: u64 = 20;
/// The first look after the start, and then between looks.
const FIRST_LOOK_MS: u64 = 10 * 60_000;
const DAY_MS: u64 = 24 * 3_600_000;
/// The published releases (ADR-0075).
const PUBLISHED: &str = "https://github.com/kaiserklowns-kkg/kernel/releases/latest/download/";
const DIR: &str = "system/updates";
const DOWNLOAD: &str = "system/updates/download.opk";
const AUTO_FILE: &str = "system/updates/auto";
const SOURCE_FILE: &str = "system/updates/source";
/// The largest checksums or signature read.
const MAX_TEXT: usize = 64 * 1024;
/// How long a small file may take to arrive.
const TEXT_WAIT_MS: u64 = 15_000;
/// Bytes taken per tick.
const PIECE: usize = 64 * 1024;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<200>::new();
    let _ = line.write_str("updater: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// What `oceans-update` says, kept as lines without its `update: `.
#[derive(Default)]
struct Lines(String);

impl Write for Lines {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.push_str(s);
        Ok(())
    }
}

impl oceans_update::Report for Lines {}

impl Lines {
    /// The lines, joined: `update: a\nupdate: b` → `a; b`.
    fn joined(&self) -> String {
        let mut text = String::new();
        for line in self.0.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if !text.is_empty() {
                text.push_str("; ");
            }
            text.push_str(line.strip_prefix("update: ").unwrap_or(line));
        }
        text
    }

    fn last(&self) -> String {
        let line = self.0.lines().map(str::trim).rfind(|l| !l.is_empty());
        let line = line.unwrap_or("");
        line.strip_prefix("update: ").unwrap_or(line).to_string()
    }
}

/// A release found at the source.
struct Found {
    version: Version,
    listed: Listed,
}

/// A download under way.
struct Download {
    response: oceans_web::Response,
    file: Node,
    shared: Shared,
    written: u64,
    total: Option<u64>,
    hasher: Sha256,
    piece: Vec<u8>,
}

struct Updater {
    log: Handle,
    net: Handle,
    root: Node,
    stick: Option<Handle>,
    state: u8,
    message: String,
    auto: bool,
    source: String,
    found: Option<Found>,
    download: Option<Download>,
    next_look: u64,
}

/// The version a package's name says (`oceans-0.1.1-alpha.opk`): only to
/// decide whether to offer it; the package itself is checked again when
/// it is installed.
fn named_version(name: &str) -> Option<Version> {
    let rest = name.strip_prefix("oceans-")?.strip_suffix(".opk")?;
    Version::parse(rest.split('-').next()?)
}

impl Updater {
    fn set(&mut self, state: u8, message: String) {
        say(self.log, format_args!("{message}"));
        self.state = state;
        self.message = message;
    }

    /// The running release and its slots, as `update status` says them.
    fn describe(&mut self) {
        let mut lines = Lines::default();
        let message = match oceans_update::status(&mut lines, &self.root, self.stick) {
            Ok(()) => lines.joined(),
            Err(why) => why,
        };
        self.message = message;
    }

    fn read_text(&self, path: &str) -> Option<String> {
        let bytes = oceans_update::read_file(&self.root, path, 4096).ok()?;
        String::from_utf8(bytes).ok().map(|t| t.trim().to_string())
    }

    fn write_text(&self, path: &str, text: &str) -> Result<(), String> {
        self.make_dir()?;
        let (file, _) = self
            .root
            .walk(path, flags::CREATE_FILE | flags::WRITE)
            .map_err(|e| alloc::format!("{path}: {}", e.message()))?;
        let result = file
            .truncate(0)
            .and_then(|()| file.write_all(0, text.as_bytes()))
            .and_then(|()| file.sync());
        file.close();
        result.map_err(|e| alloc::format!("{path}: {}", e.message()))
    }

    fn make_dir(&self) -> Result<(), String> {
        self.root
            .walk(DIR, flags::CREATE_DIRECTORY | flags::WRITE)
            .map(|(node, _)| node.close())
            .map_err(|e| alloc::format!("{DIR}: {}", e.message()))
    }

    /// A small file at the source, whole.
    fn fetch_text(&self, name: &str) -> Result<String, String> {
        let url = alloc::format!("{}{name}", self.source);
        let mut response =
            oceans_web::get(self.net, &url).map_err(|p| alloc::format!("{name}: {p}"))?;
        if response.head.status != 200 {
            return Err(alloc::format!(
                "{name}: HTTP {} {}",
                response.head.status,
                response.head.reason()
            ));
        }
        let mut bytes = Vec::new();
        let mut piece = [0u8; 4096];
        let started = oceans_rt::clock_ms();
        while !response.ended() {
            let got = response
                .read(&mut piece, 1000)
                .map_err(|p| alloc::format!("{name}: {p}"))?;
            bytes.extend_from_slice(&piece[..got]);
            if bytes.len() > MAX_TEXT {
                return Err(alloc::format!("{name}: too large"));
            }
            if got == 0 && oceans_rt::clock_ms() - started > TEXT_WAIT_MS {
                return Err(alloc::format!("{name}: timed out"));
            }
        }
        String::from_utf8(bytes).map_err(|_| alloc::format!("{name} is not text"))
    }

    /// `CHECK`: the source's newest release, if newer than the running one.
    fn check(&mut self) {
        self.state = state::CHECKING;
        let result = (|| -> Result<(u8, String), String> {
            let (running, release) = oceans_update::running(&self.root)?;
            let keys_text = oceans_update::update_keys(&self.root)?;
            let keys: Vec<TrustedKey<'_>> = oceans_package::trusted_keys(&keys_text).collect();
            let sums_text = self.fetch_text(sums::SUMS)?;
            let signature = self.fetch_text(sums::SIGNATURE)?;
            let listed =
                sums::verify(&sums_text, &signature, &keys).map_err(|e| e.message().to_string())?;
            let package = sums::system_package(&listed)
                .ok_or("the release lists no single system package")?
                .clone();
            let version = named_version(&package.name)
                .ok_or_else(|| alloc::format!("{}: not a release's name", package.name))?;
            if version <= running {
                self.found = None;
                return Ok((
                    state::UP_TO_DATE,
                    alloc::format!("Oceans {release} is up to date"),
                ));
            }
            let text = alloc::format!(
                "Oceans {version} is available ({}); you are running {release}",
                package.name
            );
            self.found = Some(Found {
                version,
                listed: package,
            });
            Ok((state::AVAILABLE, text))
        })();
        match result {
            Ok((state, text)) => self.set(state, text),
            Err(why) => self.set(state::FAILED, alloc::format!("could not check: {why}")),
        }
    }

    /// `INSTALL`: starts the download of what `CHECK` found.
    fn install(&mut self) -> u64 {
        if self.download.is_some() || self.state == state::INSTALLING {
            return updater::BUSY;
        }
        let Some(found) = &self.found else {
            return updater::NOT_NOW;
        };
        let name = found.listed.name.clone();
        let version = found.version;
        let result = (|| -> Result<Download, String> {
            self.make_dir()?;
            let (file, kind) = self
                .root
                .walk(DOWNLOAD, flags::CREATE_FILE | flags::WRITE)
                .map_err(|e| alloc::format!("{DOWNLOAD}: {}", e.message()))?;
            if kind != Kind::File {
                file.close();
                return Err(alloc::format!("{DOWNLOAD}: not a file"));
            }
            let shared = match file.truncate(0).and_then(|()| file.attach(PIECE)) {
                Ok(shared) => shared,
                Err(e) => {
                    file.close();
                    return Err(alloc::format!("{DOWNLOAD}: {}", e.message()));
                }
            };
            let url = alloc::format!("{}{name}", self.source);
            let response = match oceans_web::get(self.net, &url) {
                Ok(response) if response.head.status == 200 => response,
                Ok(response) => {
                    file.close();
                    return Err(alloc::format!(
                        "{name}: HTTP {} {}",
                        response.head.status,
                        response.head.reason()
                    ));
                }
                Err(problem) => {
                    file.close();
                    return Err(alloc::format!("{name}: {problem}"));
                }
            };
            let total = response.head.content_length;
            Ok(Download {
                response,
                file,
                shared,
                written: 0,
                total,
                hasher: Sha256::new(),
                piece: alloc::vec![0; PIECE],
            })
        })();
        match result {
            Ok(download) => {
                self.download = Some(download);
                self.set(
                    state::DOWNLOADING,
                    alloc::format!("downloading Oceans {version} ({name})"),
                );
                updater::OK
            }
            Err(why) => {
                self.set(state::FAILED, alloc::format!("could not download: {why}"));
                updater::NOT_NOW
            }
        }
    }

    /// The download's next piece; at its end, the check and the install.
    fn step(&mut self) {
        let Some(download) = &mut self.download else {
            return;
        };
        let got = match download.response.read(&mut download.piece, 0) {
            Ok(got) => got,
            Err(problem) => return self.fail_download(alloc::format!("{problem}")),
        };
        if got > 0 {
            let written = download.written;
            if written + got as u64 > oceans_update::MAX_UPDATE {
                return self.fail_download("the update is too large".to_string());
            }
            if let Err(e) =
                download
                    .file
                    .write_shared(&download.shared, written, &download.piece[..got])
            {
                return self.fail_download(alloc::format!("{DOWNLOAD}: {}", e.message()));
            }
            download.hasher.update(&download.piece[..got]);
            download.written += got as u64;
        }
        if !download.response.ended() {
            return;
        }
        let download = self.download.take().expect("checked above");
        let synced = download.file.sync();
        download.file.close();
        let Some(found) = self.found.take() else {
            return;
        };
        if let Err(e) = synced {
            return self.set(
                state::FAILED,
                alloc::format!("could not download: {DOWNLOAD}: {}", e.message()),
            );
        }
        let sha256 = sums::hex(&download.hasher.finalize());
        if sha256 != found.listed.sha256 {
            self.remove_download();
            return self.set(
                state::FAILED,
                alloc::format!(
                    "{} does not match its checksum: the download was changed; nothing was installed",
                    found.listed.name
                ),
            );
        }
        self.set(
            state::INSTALLING,
            alloc::format!(
                "Oceans {}: {} bytes, matching its checksum; installing",
                found.version,
                download.written
            ),
        );
        let mut lines = Lines::default();
        let result = oceans_update::apply(&mut lines, &self.root, self.stick, DOWNLOAD);
        self.remove_download();
        match result {
            Ok(()) => self.set(
                state::INSTALLED,
                alloc::format!("{}; restart when you are ready", lines.last()),
            ),
            Err(why) => self.set(state::FAILED, alloc::format!("not installed: {why}")),
        }
    }

    fn fail_download(&mut self, why: String) {
        if let Some(download) = self.download.take() {
            download.file.close();
        }
        self.remove_download();
        self.set(state::FAILED, alloc::format!("could not download: {why}"));
    }

    fn remove_download(&self) {
        if let Ok((dir, _)) = self.root.walk(DIR, flags::WRITE) {
            let _ = dir.remove("download.opk");
            let _ = dir.sync();
            dir.close();
        }
    }

    fn percent(&self) -> u8 {
        match &self.download {
            Some(Download {
                written,
                total: Some(total),
                ..
            }) if *total > 0 => (written * 100 / total).min(100) as u8,
            _ => 0,
        }
    }

    fn status(&mut self) -> Vec<u8> {
        if self.state == state::IDLE && self.message.is_empty() {
            self.describe();
        }
        let mut reply = alloc::vec![self.state, u8::from(self.auto), self.percent()];
        let mut text = self.message.clone();
        if let Some(download) = &self.download {
            text = alloc::format!("{text}: {}%", self.percent());
            if download.total.is_none() {
                text = alloc::format!("{}: {} bytes", self.message, download.written);
            }
        }
        // A reply holds 256 bytes.
        let room = 250 - reply.len();
        let mut end = text.len().min(room);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        reply.extend_from_slice(&text.as_bytes()[..end]);
        reply
    }

    /// Serves one request; the reply status and data.
    fn handle(&mut self, label: u64, data: &[u8]) -> (u64, Vec<u8>) {
        let busy = self.download.is_some() || self.state == state::INSTALLING;
        match label {
            updater::STATUS => (updater::OK, self.status()),
            updater::CHECK if busy => (updater::BUSY, Vec::new()),
            updater::CHECK => {
                self.check();
                (updater::OK, self.status())
            }
            updater::INSTALL => (self.install(), self.status()),
            updater::PREVIOUS if busy => (updater::BUSY, Vec::new()),
            updater::PREVIOUS => {
                let mut lines = Lines::default();
                match oceans_update::previous(&mut lines, &self.root, self.stick) {
                    Ok(()) => {
                        self.set(state::IDLE, lines.last());
                        (updater::OK, self.status())
                    }
                    Err(why) => {
                        self.set(state::FAILED, why);
                        (updater::NOT_NOW, self.status())
                    }
                }
            }
            updater::AUTO => match data {
                &[on] if on <= 1 => {
                    let text = if on == 1 { "on" } else { "off" };
                    match self.write_text(AUTO_FILE, text) {
                        Ok(()) => {
                            self.auto = on == 1;
                            self.next_look = oceans_rt::clock_ms() + FIRST_LOOK_MS;
                            say(self.log, format_args!("checks automatically: {text}"));
                            (updater::OK, self.status())
                        }
                        Err(why) => {
                            self.set(state::FAILED, why);
                            (updater::NOT_NOW, self.status())
                        }
                    }
                }
                _ => (updater::BAD_REQUEST, Vec::new()),
            },
            updater::SOURCE => {
                let Ok(text) = core::str::from_utf8(data) else {
                    return (updater::BAD_REQUEST, Vec::new());
                };
                let text = text.trim();
                let valid = text.is_empty()
                    || ((text.starts_with("https://") || text.starts_with("http://"))
                        && text.ends_with('/')
                        && !text.contains(char::is_whitespace));
                if !valid || busy {
                    return (updater::BAD_REQUEST, Vec::new());
                }
                if let Err(why) = self.write_text(SOURCE_FILE, text) {
                    self.set(state::FAILED, why);
                    return (updater::NOT_NOW, self.status());
                }
                self.source = if text.is_empty() {
                    PUBLISHED.to_string()
                } else {
                    text.to_string()
                };
                self.found = None;
                say(self.log, format_args!("the source is {}", self.source));
                (updater::OK, self.status())
            }
            _ => (updater::BAD_REQUEST, Vec::new()),
        }
    }

    /// The timer for what comes next: a download's next piece, the next
    /// look, or nothing.
    fn arm(&self, timer: Handle) {
        let now = oceans_rt::clock_ms();
        let ms = if self.download.is_some() {
            TICK_MS
        } else if self.auto {
            self.next_look.saturating_sub(now).max(1)
        } else {
            0
        };
        let _ = oceans_rt::timer_set(timer, TIMER, ms);
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(server), Some(net), Some(fs)) = (
        directory.find("log", "log"),
        directory.find("provide", "updater"),
        directory.find("use", "net"),
        directory.find("use", "fs"),
    ) else {
        return 3;
    };
    let Ok(timer) = oceans_rt::notification_create() else {
        return 4;
    };
    if oceans_rt::endpoint_bind(server, timer).is_err() {
        return 4;
    }
    let root = Node(fs);
    oceans_update::detect(&root);
    let mut updater = Updater {
        log,
        net,
        root,
        stick: directory.find("use", "usbdisk"),
        state: state::IDLE,
        message: String::new(),
        auto: true,
        source: PUBLISHED.to_string(),
        found: None,
        download: None,
        next_look: oceans_rt::clock_ms() + FIRST_LOOK_MS,
    };
    // On unless the user turned it off; a test system's configuration may
    // start it off (`args = check-when-asked`), so a long test boot never
    // reaches the internet.
    let asked_only = directory
        .args()
        .split_whitespace()
        .any(|word| word == "check-when-asked");
    updater.auto = match updater.read_text(AUTO_FILE).as_deref() {
        Some("off") => false,
        Some("on") => true,
        _ => !asked_only,
    };
    if let Some(source) = updater.read_text(SOURCE_FILE).filter(|s| !s.is_empty()) {
        updater.source = source;
    }
    // Whatever a stopped download left.
    updater.remove_download();
    say(
        log,
        format_args!(
            "ready; checks automatically: {}",
            if updater.auto { "on" } else { "off" }
        ),
    );
    let mut data = [0u8; 256];
    let mut handles = [Handle(0); 1];
    loop {
        updater.arm(timer);
        let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
            Ok(got) => got,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 5,
        };
        if got.signals != 0 {
            if updater.download.is_some() {
                updater.step();
            } else if updater.auto && oceans_rt::clock_ms() >= updater.next_look {
                updater.next_look = oceans_rt::clock_ms() + DAY_MS;
                if updater.state != state::INSTALLED {
                    updater.check();
                }
            }
            continue;
        }
        if got.closed {
            continue;
        }
        for &handle in &handles[..got.handles_len] {
            let _ = oceans_rt::close(handle);
        }
        let (status, reply) = updater.handle(got.label, &data[..got.data_len]);
        let _ = oceans_rt::ipc_reply(status, &reply);
    }
}
