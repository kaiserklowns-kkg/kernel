//! Oceans app packages (ADR-0046).
//!
//! A package (`.opk`) is an Oceans archive (ADR-0025) holding:
//! - `manifest`: what the app is and what it asks for ([`Manifest`]);
//! - the program (`entry`) and any other files;
//! - `signature`, last: the publisher's Ed25519 signature over a digest
//!   of every other file ([`digest`]).
//!
//! [`Package::open`] accepts a package only if the archive is intact, the
//! signature verifies with a key in the trust list, that key belongs to
//! the publisher the manifest names, and the manifest is valid. Nothing is
//! allocated; everything borrows from the package bytes.
//!
//! [`Permission`] is the catalog of what an app may ask for (ADR-0047).

#![no_std]

#[cfg(feature = "build")]
extern crate alloc;

pub mod web;

#[cfg(test)]
mod tests;

use oceans_archive::{Archive, ArchiveError};
use sha2::{Digest, Sha512};

/// The manifest's file name.
pub const MANIFEST: &str = "manifest";
/// The signature's file name (always the last file).
pub const SIGNATURE: &str = "signature";
/// The signature file: magic, public key, signature.
pub const SIGNATURE_MAGIC: &[u8; 8] = b"OCEANSIG";
pub const SIGNATURE_LEN: usize = 8 + 32 + 64;
/// Domain separation for the signed digest.
const DIGEST_CONTEXT: &[u8] = b"oceans-package-v1\0";

/// The API level this system offers (ADR-0045); manifests name the one
/// they need.
pub const API_LEVEL: u32 = 1;
/// The architecture packages must be built for.
pub const ARCHITECTURE: &str = "x86_64";

pub const MAX_PERMISSIONS: usize = 16;
const MAX_ID: usize = 64;
const MAX_NAME: usize = 40;
const MAX_PUBLISHER: usize = 40;
const MAX_DESCRIPTION: usize = 200;
const MAX_REASON: usize = 120;
const MAX_URL: usize = 200;

/// What an app may ask for. Each maps to the capabilities the app manager
/// grants (ADR-0047).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// Writing to the terminal that started it.
    Console,
    /// Its own private data directory.
    Storage,
    /// Read-only system information (processes, memory, uptime).
    SystemInfo,
    /// The network: connections and name lookups.
    Network,
    /// The user's files (`/home`).
    Files,
    /// Pointer input: mice and tablets.
    Pointer,
    /// Its own windows on the screen (ADR-0059), with the keyboard and
    /// pointer while the user gives one the focus.
    Window,
    /// Notifications on the desktop, framed with the app's name (ADR-0065).
    Notifications,
}

impl Permission {
    pub const ALL: [Self; 8] = [
        Self::Console,
        Self::Storage,
        Self::SystemInfo,
        Self::Network,
        Self::Files,
        Self::Pointer,
        Self::Window,
        Self::Notifications,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Storage => "storage",
            Self::SystemInfo => "system-info",
            Self::Network => "network",
            Self::Files => "files",
            Self::Pointer => "pointer",
            Self::Window => "window",
            Self::Notifications => "notifications",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// What it means, in the words the system shows the user.
    pub fn description(self) -> &'static str {
        match self {
            Self::Console => "write to the terminal that started it",
            Self::Storage => "keep its own data",
            Self::SystemInfo => "see processes, memory use and uptime",
            Self::Network => "connect to the internet and the local network",
            Self::Files => "read and change your files in /home",
            Self::Pointer => "see your mouse and tablet movements and clicks",
            Self::Window => "show windows, and get what you type into them",
            Self::Notifications => "show you notifications on the desktop",
        }
    }

    /// Granted without asking: it reaches nothing beyond the app itself
    /// and read-only information (ADR-0020's low-risk grants). A window
    /// gets keys and clicks only while the user gives it the focus, and
    /// the frame the system draws around it names the app (ADR-0059).
    pub fn automatic(self) -> bool {
        matches!(
            self,
            Self::Console | Self::Storage | Self::SystemInfo | Self::Window
        )
    }
}

/// How the program is run (ADR-0052).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// An x86-64 ELF executable, spawned directly (the default).
    Native,
    /// A WebAssembly module built for wasip1 (a Go program, ADR-0050), run
    /// by the Go host.
    Wasm,
    /// A web app (ADR-0064): a [`web`] bundle, served to a browser by the
    /// bridge; nothing runs on Oceans itself.
    Web,
}

impl Runtime {
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wasm => "wasm",
            Self::Web => "web",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Native, Self::Wasm, Self::Web]
            .into_iter()
            .find(|r| r.name() == name)
    }

    /// Whether `program` starts as this runtime's programs must: an ELF
    /// header, the WebAssembly magic and binary format version 1, or a
    /// whole web bundle that reads. The rest is checked by whoever loads it
    /// (the kernel, the Go host, the bridge).
    pub fn accepts(self, program: &[u8]) -> bool {
        match self {
            Self::Native => program.starts_with(b"\x7fELF"),
            Self::Wasm => program.starts_with(b"\0asm\x01\0\0\0"),
            Self::Web => web::read(program, |_, _| {}).is_ok(),
        }
    }
}

/// A requested permission and the app's reason for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub permission: Permission,
    pub reason: &'a str,
}

/// A `MAJOR.MINOR.PATCH` version, ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut number = || -> Option<u32> {
            let part = parts.next()?;
            // No signs, no leading zeros (except 0 itself).
            if part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return None;
            }
            part.parse().ok()
        };
        let version = Self {
            major: number()?,
            minor: number()?,
            patch: number()?,
        };
        parts.next().is_none().then_some(version)
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A package's manifest: `key = value` lines, `#` comments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Manifest<'a> {
    /// Reverse-DNS identifier, e.g. `app.oceans.hello`.
    pub id: &'a str,
    pub name: &'a str,
    pub version: Version,
    pub publisher: &'a str,
    pub description: &'a str,
    pub architecture: &'a str,
    pub api: u32,
    /// The program's file in the package.
    pub entry: &'a str,
    /// `stable` unless named.
    pub channel: &'a str,
    /// `kind = service` (ADR-0049): runs in the background, can start at
    /// boot and is restarted when it fails; `kind = app` (the default):
    /// started by the user.
    pub service: bool,
    /// `runtime = native` (the default) or `wasm` (ADR-0052).
    pub runtime: Runtime,
    /// Where the source is, if published.
    pub source: Option<&'a str>,
    requests: [Option<Request<'a>>; MAX_PERMISSIONS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// A line that is not `key = value`.
    Syntax(usize),
    UnknownKey(usize),
    DuplicateKey(usize),
    Missing(&'static str),
    BadId,
    BadText(&'static str),
    BadVersion,
    BadApi,
    UnknownPermission(usize),
    DuplicatePermission(usize),
    TooManyPermissions,
}

impl ManifestError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Syntax(_) => "a line is not `key = value`",
            Self::UnknownKey(_) => "unknown key",
            Self::DuplicateKey(_) => "a key appears twice",
            Self::Missing(key) => key,
            Self::BadId => "the id is not reverse-DNS (e.g. app.example.name)",
            Self::BadText(key) => key,
            Self::BadVersion => "the version is not MAJOR.MINOR.PATCH",
            Self::BadApi => "the api level is not a number",
            Self::UnknownPermission(_) => "unknown permission",
            Self::DuplicatePermission(_) => "a permission is asked for twice",
            Self::TooManyPermissions => "too many permissions",
        }
    }
}

impl<'a> Manifest<'a> {
    pub fn parse(text: &'a str) -> Result<Self, ManifestError> {
        let mut fields: [Option<&'a str>; 11] = [None; 11];
        const KEYS: [&str; 11] = [
            "id",
            "name",
            "version",
            "publisher",
            "description",
            "architecture",
            "api",
            "entry",
            "channel",
            "kind",
            "runtime",
        ];
        let mut source = None;
        let mut requests = [None; MAX_PERMISSIONS];
        let mut count = 0;
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(ManifestError::Syntax(number))?;
            let (key, value) = (key.trim(), value.trim());
            if key == "permission" {
                let (name, reason) = match value.split_once(':') {
                    Some((name, reason)) => (name.trim(), reason.trim()),
                    None => (value, ""),
                };
                let permission =
                    Permission::from_name(name).ok_or(ManifestError::UnknownPermission(number))?;
                if requests[..count]
                    .iter()
                    .any(|r: &Option<Request<'_>>| r.is_some_and(|r| r.permission == permission))
                {
                    return Err(ManifestError::DuplicatePermission(number));
                }
                if !text_ok(reason, MAX_REASON) && !reason.is_empty() {
                    return Err(ManifestError::BadText(
                        "a permission reason is too long or not text",
                    ));
                }
                if count == MAX_PERMISSIONS {
                    return Err(ManifestError::TooManyPermissions);
                }
                requests[count] = Some(Request { permission, reason });
                count += 1;
                continue;
            }
            if key == "source" {
                if source.replace(value).is_some() {
                    return Err(ManifestError::DuplicateKey(number));
                }
                continue;
            }
            let slot = KEYS
                .iter()
                .position(|&k| k == key)
                .ok_or(ManifestError::UnknownKey(number))?;
            if fields[slot].replace(value).is_some() {
                return Err(ManifestError::DuplicateKey(number));
            }
        }
        let field =
            |slot: usize, name: &'static str| fields[slot].ok_or(ManifestError::Missing(name));
        let id = field(0, "the manifest has no id")?;
        if !valid_id(id) {
            return Err(ManifestError::BadId);
        }
        let name = field(1, "the manifest has no name")?;
        let version = Version::parse(field(2, "the manifest has no version")?)
            .ok_or(ManifestError::BadVersion)?;
        let publisher = field(3, "the manifest has no publisher")?;
        let description = fields[4].unwrap_or("");
        let architecture = field(5, "the manifest has no architecture")?;
        let api = field(6, "the manifest has no api level")?
            .parse()
            .map_err(|_| ManifestError::BadApi)?;
        let entry = field(7, "the manifest has no entry")?;
        let channel = fields[8].unwrap_or("stable");
        let service = match fields[9] {
            None | Some("app") => false,
            Some("service") => true,
            Some(_) => {
                return Err(ManifestError::BadText(
                    "the kind is neither app nor service",
                ));
            }
        };
        let runtime = match fields[10] {
            None => Runtime::Native,
            Some(name) => Runtime::from_name(name).ok_or(ManifestError::BadText(
                "the runtime is not native, wasm or web",
            ))?,
        };
        if service && runtime == Runtime::Web {
            return Err(ManifestError::BadText(
                "a web app cannot be a service: it runs in a browser",
            ));
        }
        // A web app reaches the system only through the bridge: its own
        // data (ADR-0064), and the network from its page (ADR-0066).
        if runtime == Runtime::Web
            && requests
                .iter()
                .flatten()
                .any(|r| !matches!(r.permission, Permission::Storage | Permission::Network))
        {
            return Err(ManifestError::BadText(
                "a web app may ask only for storage and network",
            ));
        }
        let checks = [
            (
                text_ok(name, MAX_NAME),
                "the name is empty, too long or not text",
            ),
            (
                text_ok(publisher, MAX_PUBLISHER),
                "the publisher is empty, too long or not text",
            ),
            (
                description.is_empty() || text_ok(description, MAX_DESCRIPTION),
                "the description is too long or not text",
            ),
            (word_ok(architecture), "the architecture is not a word"),
            (
                oceans_archive_name(entry) && entry != MANIFEST && entry != SIGNATURE,
                "the entry is not a file name",
            ),
            (word_ok(channel), "the channel is not a word"),
            (
                source.is_none_or(|url| text_ok(url, MAX_URL) && !url.contains(' ')),
                "the source is not a URL",
            ),
        ];
        if let Some(&(_, problem)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(ManifestError::BadText(problem));
        }
        Ok(Self {
            id,
            name,
            version,
            publisher,
            description,
            architecture,
            api,
            entry,
            channel,
            service,
            runtime,
            source,
            requests,
        })
    }

    /// The permissions asked for, in manifest order.
    pub fn requests(&self) -> impl Iterator<Item = Request<'a>> + '_ {
        self.requests.iter().flatten().copied()
    }

    pub fn asks_for(&self, permission: Permission) -> bool {
        self.requests().any(|r| r.permission == permission)
    }
}

/// Printable text (no control characters), 1 to `max` bytes.
fn text_ok(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

/// Lowercase letters, digits, `-` and `_`.
fn word_ok(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 16
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn oceans_archive_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= oceans_archive::MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Reverse-DNS: 2 to 8 labels of lowercase letters, digits and `-`, each
/// starting with a letter; at most 64 bytes. Ids name directories, so
/// nothing else is allowed.
pub fn valid_id(id: &str) -> bool {
    let labels = id.split('.').count();
    id.len() <= MAX_ID
        && (2..=8).contains(&labels)
        && id.split('.').all(|label| {
            (1..=32).contains(&label.len())
                && label.as_bytes()[0].is_ascii_lowercase()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// A publisher key the system trusts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedKey<'a> {
    pub publisher: &'a str,
    pub key: [u8; 32],
}

/// A trust list line: a key, and the last day it is trusted, if limited
/// (ADR-0067).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustEntry<'a> {
    pub key: TrustedKey<'a>,
    /// Days since 1970-01-01 ([`date`]): trusted through that day.
    pub until: Option<u32>,
}

impl TrustEntry<'_> {
    /// Whether the key is trusted on `today` (days since 1970-01-01; `None`
    /// if the time is not known: a limited key is then not trusted).
    pub fn valid_on(&self, today: Option<u32>) -> bool {
        match self.until {
            None => true,
            Some(until) => today.is_some_and(|today| today <= until),
        }
    }
}

/// The trust list: lines `KEY-HEX [until=YYYY-MM-DD] PUBLISHER NAME`, `#`
/// comments. Lines that do not parse are skipped (and reported by
/// [`trust_errors`]).
pub fn trust_entries(text: &str) -> impl Iterator<Item = TrustEntry<'_>> {
    text.lines().filter_map(parse_trust_line)
}

/// The keys of a trust list without limits (the boot image's): a limited
/// line is not trusted here.
pub fn trusted_keys(text: &str) -> impl Iterator<Item = TrustedKey<'_>> {
    trust_entries(text)
        .filter(|entry| entry.until.is_none())
        .map(|entry| entry.key)
}

/// Calendar dates as days since 1970-01-01 (ADR-0067).
pub mod date {
    /// `YYYY-MM-DD` (years 1970 to 9999).
    pub fn parse(text: &str) -> Option<u32> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let number = |range: core::ops::Range<usize>| -> Option<u32> {
            let part = &text[range];
            part.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| part.parse().ok())?
        };
        let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
        if year < 1970 || !(1..=12).contains(&month) || day == 0 || day > days_in(year, month) {
            return None;
        }
        Some(days_from_civil(year, month, day))
    }

    /// The date of `days` as `(year, month, day)`.
    pub fn civil(days: u32) -> (u32, u32, u32) {
        // Howard Hinnant's civil_from_days, for days >= 0.
        let z = days + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + u32::from(month <= 2);
        (year, month, day)
    }

    /// Today, from milliseconds since 1970 (UTC).
    pub fn today(unix_ms: u64) -> u32 {
        (unix_ms / 86_400_000) as u32
    }

    fn days_from_civil(year: u32, month: u32, day: u32) -> u32 {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y / 400;
        let yoe = y - era * 400;
        let mp = if month > 2 { month - 3 } else { month + 9 };
        let doy = (153 * mp + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn days_in(year: u32, month: u32) -> u32 {
        match month {
            4 | 6 | 9 | 11 => 30,
            2 if year.is_multiple_of(4)
                && (!year.is_multiple_of(100) || year.is_multiple_of(400)) =>
            {
                29
            }
            2 => 28,
            _ => 31,
        }
    }
}

/// How many non-comment lines of a trust list do not parse.
pub fn trust_errors(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#') && parse_trust_line(line).is_none()
        })
        .count()
}

fn parse_trust_line(line: &str) -> Option<TrustEntry<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (hex, rest) = line.split_once(' ')?;
    let rest = rest.trim();
    let (until, publisher) = match rest.strip_prefix("until=") {
        Some(dated) => {
            let (day, publisher) = dated.split_once(' ')?;
            (Some(date::parse(day)?), publisher.trim())
        }
        None => (None, rest),
    };
    let key = parse_hex32(hex)?;
    text_ok(publisher, MAX_PUBLISHER).then_some(TrustEntry {
        key: TrustedKey { publisher, key },
        until,
    })
}

fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let nibble = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = (nibble(hex[2 * i])? << 4) | nibble(hex[2 * i + 1])?;
    }
    Some(key)
}

/// The digest a package's signature covers: every file but the signature,
/// in archive order, as `[name length u32][name][size u64][SHA-512]`,
/// after a context string. Its layout in the archive does not matter.
pub fn digest<'a>(files: impl Iterator<Item = (&'a str, &'a [u8])>) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(DIGEST_CONTEXT);
    for (name, data) in files {
        hasher.update((name.len() as u32).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((data.len() as u64).to_le_bytes());
        hasher.update(Sha512::digest(data));
    }
    hasher.finalize().into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageError {
    Archive(ArchiveError),
    /// No signature, or one in the wrong place or form.
    Unsigned,
    /// The signature does not verify: the package was changed.
    BadSignature,
    /// Signed with a key the system does not trust.
    UntrustedKey,
    /// The key belongs to another publisher than the manifest names.
    WrongPublisher,
    NoManifest,
    Manifest(ManifestError),
    /// The entry the manifest names is not in the package.
    NoEntry,
    /// Built for another architecture.
    WrongArchitecture,
    /// Needs a newer API level than the system offers.
    ApiTooNew,
    /// The program is not in its runtime's format (ADR-0052).
    WrongFormat(Runtime),
}

impl PackageError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Archive(_) => "not a valid package (damaged archive)",
            Self::Unsigned => "the package is not signed",
            Self::BadSignature => "the signature does not match: the package was changed",
            Self::UntrustedKey => "signed with a key this system does not trust",
            Self::WrongPublisher => "the signing key belongs to another publisher",
            Self::NoManifest => "the package has no manifest",
            Self::Manifest(error) => error.message(),
            Self::NoEntry => "the program named by the manifest is missing",
            Self::WrongArchitecture => "built for another architecture",
            Self::ApiTooNew => "needs a newer version of Oceans",
            Self::WrongFormat(Runtime::Native) => "the program is not an ELF executable",
            Self::WrongFormat(Runtime::Wasm) => "the program is not a WebAssembly module",
            Self::WrongFormat(Runtime::Web) => "the web bundle is not valid",
        }
    }
}

/// A verified package.
#[derive(Clone, Copy, Debug)]
pub struct Package<'a> {
    archive: Archive<'a>,
    pub manifest: Manifest<'a>,
    /// The publisher key that signed it.
    pub key: [u8; 32],
}

impl<'a> Package<'a> {
    /// Verifies and opens a package: archive, signature, trust, manifest,
    /// in that order (nothing unverified is interpreted), then that the
    /// program is in its runtime's format.
    pub fn open(bytes: &'a [u8], trusted: &[TrustedKey<'_>]) -> Result<Self, PackageError> {
        let archive = Archive::parse(bytes).map_err(PackageError::Archive)?;
        let count = archive.len();
        let last = count
            .checked_sub(1)
            .and_then(|i| archive.file(i))
            .ok_or(PackageError::Unsigned)?;
        if last.name != SIGNATURE
            || last.data.len() != SIGNATURE_LEN
            || &last.data[..8] != SIGNATURE_MAGIC
        {
            return Err(PackageError::Unsigned);
        }
        let key: [u8; 32] = last.data[8..40].try_into().unwrap();
        let signature: [u8; 64] = last.data[40..].try_into().unwrap();
        let signed = digest(
            (0..count - 1)
                .filter_map(|i| archive.file(i))
                .map(|f| (f.name, f.data)),
        );
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&key)
            .map_err(|_| PackageError::BadSignature)?;
        verifying
            .verify_strict(&signed, &ed25519_dalek::Signature::from_bytes(&signature))
            .map_err(|_| PackageError::BadSignature)?;
        let trust = trusted
            .iter()
            .find(|t| t.key == key)
            .ok_or(PackageError::UntrustedKey)?;

        let text = archive.find(MANIFEST).ok_or(PackageError::NoManifest)?;
        let text = core::str::from_utf8(text)
            .map_err(|_| PackageError::Manifest(ManifestError::Syntax(0)))?;
        let manifest = Manifest::parse(text).map_err(PackageError::Manifest)?;
        if manifest.publisher != trust.publisher {
            return Err(PackageError::WrongPublisher);
        }
        if manifest.architecture != ARCHITECTURE {
            return Err(PackageError::WrongArchitecture);
        }
        if manifest.api > API_LEVEL {
            return Err(PackageError::ApiTooNew);
        }
        let entry = archive.find(manifest.entry).ok_or(PackageError::NoEntry)?;
        if !manifest.runtime.accepts(entry) {
            return Err(PackageError::WrongFormat(manifest.runtime));
        }
        Ok(Self {
            archive,
            manifest,
            key,
        })
    }

    /// The program.
    pub fn entry(&self) -> &'a [u8] {
        self.archive.find(self.manifest.entry).unwrap_or(&[])
    }

    /// Another file of the package.
    pub fn file(&self, name: &str) -> Option<&'a [u8]> {
        self.archive.find(name)
    }
}

/// Builds a signed package (xtask and tests): `files` in order (the
/// manifest among them), then the signature made with the Ed25519 key
/// whose 32-byte seed is `seed`.
#[cfg(feature = "build")]
pub fn build(
    files: &[(&str, &[u8])],
    seed: &[u8; 32],
) -> Result<alloc::vec::Vec<u8>, ArchiveError> {
    use ed25519_dalek::Signer;

    let signing = ed25519_dalek::SigningKey::from_bytes(seed);
    let signed = digest(files.iter().map(|&(name, data)| (name, data)));
    let mut block = [0u8; SIGNATURE_LEN];
    block[..8].copy_from_slice(SIGNATURE_MAGIC);
    block[8..40].copy_from_slice(signing.verifying_key().as_bytes());
    block[40..].copy_from_slice(&signing.sign(&signed).to_bytes());
    let mut all: alloc::vec::Vec<(&str, &[u8])> = files.to_vec();
    all.push((SIGNATURE, &block));
    let mut out = alloc::vec![0u8; oceans_archive::archive_len(&all)];
    oceans_archive::write(&all, &mut out)?;
    Ok(out)
}

/// The public key of a signing seed, as the trust list writes it.
#[cfg(feature = "build")]
pub fn public_key_hex(seed: &[u8; 32]) -> alloc::string::String {
    use core::fmt::Write;

    let key = ed25519_dalek::SigningKey::from_bytes(seed).verifying_key();
    let mut hex = alloc::string::String::new();
    for byte in key.as_bytes() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
