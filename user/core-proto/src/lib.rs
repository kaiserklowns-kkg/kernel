//! The Oceans Core protocol (ADR-0045), between the `core` service (apps,
//! packages, permissions) and its clients: the shell today, the system UI
//! later.
//!
//! One endpoint, `core`. Its unbadged client end is full authority over
//! apps: installing, running, stopping and removing them, and deciding
//! their permissions. It goes to the user's agent (the shell), never to an
//! app. A holder can mint **narrower** ends (`MINT`, ADR-0048): badged
//! client ends carrying a subset of its own [`access`] rights, to hand to
//! a program or an agent session. Requests are IPC calls; labels select the operation and replies
//! carry a [`Status`] label. Apps are named by their id (`app.oceans.hello`).
//!
//! Permissions are numbered as `oceans_package::Permission::ALL` lists them
//! (see [`permission_index`] in the service).

#![no_std]

use oceans_rt::{Error, Handle};

/// Operations (request labels).
pub mod op {
    /// handles = `[package file]` (an fs node opened for reading) →
    /// `[outcome u8]` + `ID\0VERSION\0PREVIOUS` ([`super::outcome`]).
    /// A refused package: `Invalid` with the reason as data.
    pub const INSTALL: u64 = 1;
    /// data = `[index u32]` → `[running u8]` + `ID\0VERSION\0NAME`;
    /// `NotFound` past the last app.
    pub const LIST: u64 = 2;
    /// data = `[field u8][id]` → the field as text ([`super::field`]).
    pub const INFO: u64 = 3;
    /// data = `[index u8][id]` → `[permission u8][decision u8][reason]`
    /// for the app's `index`th request; `NotFound` past the last.
    pub const PERMISSION: u64 = 4;
    /// data = `[flags u8][id length u8][id][arguments]`, handles = `[out]`
    /// (the console the app may write to, if it asks for `console`).
    /// → handles = `[process]` (`WAIT` only) unless detached.
    /// `NeedsConsent` while a requested permission is undecided.
    pub const RUN: u64 = 5;
    /// data = id: kills the running app.
    pub const STOP: u64 = 6;
    /// data = `[keep data u8][id]`: stops and uninstalls the app.
    pub const REMOVE: u64 = 7;
    /// data = `[permission u8][allow u8][source u8][id]`: records the
    /// user's decision ([`super::source`]): `allow` 1 allows, 0 denies, 2
    /// forgets (the next run asks again, ADR-0057). Denying or forgetting a
    /// permission a running app holds stops it → `[stopped u8]`.
    pub const DECIDE: u64 = 8;
    /// data = id: goes back to the previous version → its version.
    pub const ROLLBACK: u64 = 9;
    /// data = `[index u32]` → an audit entry, newest first; `NotFound`
    /// past the oldest kept.
    pub const AUDIT: u64 = 10;
    /// data = `[rights u8]` ([`super::access`]) → handles = a new client
    /// end with those rights, which must be among the caller's own
    /// (ADR-0048).
    pub const MINT: u64 = 11;
    /// data = id: a service (ADR-0049) starts now and at every boot, and is
    /// restarted when it fails. `NeedsConsent` while a permission is
    /// undecided.
    pub const ENABLE: u64 = 12;
    /// data = id: stops the service and no longer starts it at boot.
    pub const DISABLE: u64 = 13;
    /// handles = `[windows]`: the display service's window endpoint
    /// (ADR-0059), a server end with only `MANAGE`, so Core can mint a
    /// badged client end for each app given `window` (replacing an earlier
    /// one).
    pub const WINDOWS: u64 = 14;
    /// data = `[badge u64]` → `[grants u8]` + `ID\0VERSION\0NAME` of the
    /// running app whose display end carries that badge, `grants` being
    /// [`super::display_grant`] bits; `NotFound` once it has ended. The
    /// display service asks before showing a window or a notification
    /// (ADR-0059, ADR-0065).
    pub const WINDOW_OWNER: u64 = 15;
    /// data = `[length u64]`, handles = `[package]` (a memory object
    /// holding the package's `length` bytes): proposes installing it for
    /// the user to confirm on the device (the Store, ADR-0061). Core
    /// verifies it as `INSTALL` would and keeps a copy → `[number u32]`.
    /// One proposal waits at a time (`Pending` otherwise).
    pub const PROPOSE: u64 = 16;
    /// → `[number u32]` + `ID\0VERSION\0NAME\0PUBLISHER\0PERMISSIONS\0
    /// PREVIOUS\0DESCRIPTION` of the waiting proposal (permissions joined
    /// by `,`; previous: the installed version it updates, or empty; the
    /// description last, cut if long); `NotFound` if none waits.
    pub const PENDING: u64 = 17;
    /// data = `[number u32][install u8]`: the user's answer. 1 installs
    /// exactly the bytes proposed; 0 discards them. → as `INSTALL`.
    pub const ACCEPT: u64 = 18;
    /// data = `[add u8][key: 64 hex digits][ until=YYYY-MM-DD] [publisher]`:
    /// trusts a developer's publisher key (1), through a date if given
    /// (ADR-0067), or no longer trusts one added so (0; data = the key).
    /// Keys from the boot image cannot be removed; a publisher name already
    /// trusted under another key is refused (`Invalid`, ADR-0063).
    pub const TRUST: u64 = 19;
    /// data = `[index u32]` → `[flags u8]` + `KEY [until=YYYY-MM-DD]
    /// PUBLISHER`, the index-th trusted key (flags: 1 the user added it, 2
    /// it has expired); `NotFound` past the last.
    pub const TRUSTED: u64 = 20;
    /// data = id → handles = `[bundle]` (a read-only memory object holding
    /// the web app's bundle, verified as for a start), data = `[length
    /// u64]` (the bundle's bytes; the object is rounded up to pages) + its
    /// version.
    /// `CannotStart` if the app is not a web app (ADR-0064).
    pub const WEB_BUNDLE: u64 = 21;
    /// → `[level u8][muted u8]`: the system volume (ADR-0100).
    pub const VOLUME: u64 = 22;
    /// data = `[level u8][muted u8]`: sets the system volume, kept across
    /// reboots (ADR-0100) → it as set. `NotFound` without a sound device.
    pub const SET_VOLUME: u64 = 23;
    /// data = `[query]` (UTF-8, up to `oceans_search::MAX_QUERY` bytes) →
    /// the files of Home whose names match it, best first, as paths
    /// relative to Home joined by `\n`, as many as fit (ADR-0108). Names
    /// only: nothing of what they hold.
    pub const FIND: u64 = 24;
}

/// What a `core` client end may do (ADR-0048). The unbadged end has all.
pub mod access {
    /// `LIST`, `INFO`, `PERMISSION`.
    pub const QUERY: u8 = 1 << 0;
    /// `RUN` (and `STOP`) of installed apps.
    pub const RUN: u8 = 1 << 1;
    /// `INSTALL`, `REMOVE`, `ROLLBACK`.
    pub const MANAGE: u8 = 1 << 2;
    /// `DECIDE`: answering consent, granting and revoking permissions.
    pub const DECIDE: u8 = 1 << 3;
    /// `AUDIT`.
    pub const AUDIT: u8 = 1 << 4;
    /// `PROPOSE`: installing only once the user confirms (ADR-0061).
    pub const PROPOSE: u8 = 1 << 5;
    pub const ALL: u8 = QUERY | RUN | MANAGE | DECIDE | AUDIT | PROPOSE;

    /// Rights from names joined by `+` (`query+run`); `None` for an
    /// unknown name.
    pub fn parse(names: &str) -> Option<u8> {
        names.split('+').try_fold(0, |rights, name| {
            Some(
                rights
                    | match name {
                        "query" => QUERY,
                        "run" => RUN,
                        "manage" => MANAGE,
                        "decide" => DECIDE,
                        "audit" => AUDIT,
                        "propose" => PROPOSE,
                        "all" => ALL,
                        _ => return None,
                    },
            )
        })
    }

    /// The right an operation needs.
    pub fn needed(op: u64) -> Option<u8> {
        use super::op;
        Some(match op {
            op::LIST
            | op::INFO
            | op::PERMISSION
            | op::WINDOW_OWNER
            | op::PENDING
            | op::TRUSTED
            | op::WEB_BUNDLE
            | op::VOLUME
            | op::FIND => QUERY,
            op::RUN | op::STOP => RUN,
            op::INSTALL
            | op::REMOVE
            | op::ROLLBACK
            | op::ENABLE
            | op::DISABLE
            | op::WINDOWS
            | op::TRUST => MANAGE,
            // The user's setting, as a decision is (ADR-0100).
            op::DECIDE | op::ACCEPT | op::SET_VOLUME => DECIDE,
            op::PROPOSE => PROPOSE,
            op::AUDIT => AUDIT,
            // Minting gives only rights the caller already has.
            op::MINT => 0,
            _ => return None,
        })
    }
}

/// What an app's display end may be used for (`WINDOW_OWNER`).
pub mod display_grant {
    /// Windows (`window`, ADR-0059).
    pub const WINDOW: u8 = 1 << 0;
    /// Notifications (`notifications`, ADR-0065).
    pub const NOTIFICATIONS: u8 = 1 << 1;
}

/// `RUN` flags.
pub mod run_flags {
    /// Run in the background: no process handle comes back.
    pub const DETACH: u8 = 1 << 0;
}

/// `INSTALL` outcomes.
pub mod outcome {
    pub const INSTALLED: u8 = 0;
    pub const UPDATED: u8 = 1;
}

/// `INFO` fields.
pub mod field {
    pub const NAME: u8 = 0;
    pub const VERSION: u8 = 1;
    pub const PUBLISHER: u8 = 2;
    pub const DESCRIPTION: u8 = 3;
    pub const CHANNEL: u8 = 4;
    /// The signing key's fingerprint (first 8 bytes, hex).
    pub const KEY: u8 = 5;
    /// `running` or `installed`.
    pub const STATE: u8 = 6;
    /// The version a rollback returns to, or empty.
    pub const PREVIOUS: u8 = 7;
    pub const SOURCE: u8 = 8;
    /// `app` or `service`; a service also says whether it is enabled.
    pub const KIND: u8 = 9;
    /// `native`, or `wasm` for a WebAssembly program run by the Go host
    /// (ADR-0052).
    pub const RUNTIME: u8 = 10;
    /// The kinds of file it opens, as extensions separated by spaces (ADR-0099);
    /// empty for none.
    pub const OPENS: u8 = 11;
}

/// Who made a `DECIDE` decision (kept in the audit log).
pub mod source {
    /// The user answered a consent prompt.
    pub const PROMPT: u8 = 0;
    /// The user typed a command (`app grant`, `app revoke`).
    pub const COMMAND: u8 = 1;
    /// The user answered a permission dialog of the desktop (ADR-0057).
    pub const DIALOG: u8 = 2;
    /// The user decided in Settings (ADR-0081).
    pub const SETTINGS: u8 = 3;
}

/// `DECIDE` values of `allow`.
pub mod decision {
    pub const DENY: u8 = 0;
    pub const ALLOW: u8 = 1;
    /// Forget the decision: ask again at the next run.
    pub const FORGET: u8 = 2;
}

/// A permission's state for one app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Decision {
    /// Granted without asking (low-risk, ADR-0047).
    Automatic = 0,
    Allowed = 1,
    Denied = 2,
    /// Not asked yet: the next run asks.
    Undecided = 3,
}

impl Decision {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Automatic),
            1 => Some(Self::Allowed),
            2 => Some(Self::Denied),
            3 => Some(Self::Undecided),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Undecided => "not decided",
        }
    }
}

/// Reply status (reply label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    /// No such app (or entry).
    NotFound = 1,
    BadRequest = 2,
    /// The package was refused; the reply data says why.
    Invalid = 3,
    /// A requested permission is undecided: ask the user, `DECIDE`, retry.
    NeedsConsent = 4,
    NotRunning = 5,
    /// An update that is not newer than what is installed.
    NotNewer = 6,
    /// An update signed with another key than the installed version.
    KeyChanged = 7,
    /// No previous version to roll back to.
    NoRollback = 8,
    /// The app is already running (one instance at a time).
    AlreadyRunning = 9,
    /// Storage failed.
    IoError = 10,
    /// The app cannot be started (bad program, out of memory).
    CannotStart = 11,
    /// This client end lacks the right (ADR-0048).
    Denied = 12,
    /// `ENABLE` of an app that is not a service.
    NotAService = 13,
    /// Another proposed install waits for the user (ADR-0061).
    Pending = 14,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            1 => Self::NotFound,
            3 => Self::Invalid,
            4 => Self::NeedsConsent,
            5 => Self::NotRunning,
            6 => Self::NotNewer,
            7 => Self::KeyChanged,
            8 => Self::NoRollback,
            9 => Self::AlreadyRunning,
            10 => Self::IoError,
            11 => Self::CannotStart,
            12 => Self::Denied,
            13 => Self::NotAService,
            14 => Self::Pending,
            _ => Self::BadRequest,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotFound => "not installed",
            Self::BadRequest => "bad request",
            Self::Invalid => "package refused",
            Self::NeedsConsent => "needs your permission",
            Self::NotRunning => "not running",
            Self::NotNewer => "not newer than the installed version",
            Self::KeyChanged => "signed with a different key than the installed version",
            Self::NoRollback => "no previous version to roll back to",
            Self::AlreadyRunning => "already running",
            Self::IoError => "storage failed",
            Self::CannotStart => "cannot be started",
            Self::Denied => "not allowed by this capability",
            Self::NotAService => "not a service (only services start at boot)",
            Self::Pending => "another install is waiting for confirmation on the device",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreError {
    Status(Status),
    Ipc(Error),
}

impl CoreError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Status(status) => status.message(),
            Self::Ipc(Error::PeerClosed) => "the app service is unavailable",
            Self::Ipc(_) => "the request failed",
        }
    }
}

/// Largest request or reply data.
pub const MAX_DATA: usize = 248;
/// Longest app id.
pub const MAX_ID: usize = 64;

/// A reply: its data length and the handle it carried, if any. `Invalid`
/// replies come back as `Err` with their data (the reason) still written.
pub struct Reply {
    pub len: usize,
    pub handle: Option<Handle>,
}

/// The `core` endpoint.
#[derive(Clone, Copy, Debug)]
pub struct Core(pub Handle);

impl Core {
    /// One request. On error the reply data (e.g. why a package was
    /// refused) is still in `reply`, `len` bytes of it.
    pub fn call(
        &self,
        op: u64,
        data: &[u8],
        handles: &[Handle],
        reply: &mut [u8],
    ) -> Result<Reply, (CoreError, usize)> {
        let mut received = [Handle(0); 1];
        let got = oceans_rt::ipc_call_msg(self.0, op, data, handles, reply, &mut received)
            .map_err(|error| (CoreError::Ipc(error), 0))?;
        let handle = (got.handles_len == 1).then_some(received[0]);
        match Status::from_label(got.label) {
            Status::Ok => Ok(Reply {
                len: got.data_len,
                handle,
            }),
            status => {
                if let Some(handle) = handle {
                    let _ = oceans_rt::close(handle);
                }
                Err((CoreError::Status(status), got.data_len))
            }
        }
    }

    /// A request about one app: `prefix` bytes, then its id.
    pub fn about(
        &self,
        op: u64,
        prefix: &[u8],
        id: &str,
        reply: &mut [u8],
    ) -> Result<Reply, (CoreError, usize)> {
        let mut data = [0u8; MAX_DATA];
        let len = prefix.len() + id.len();
        if id.is_empty() || id.len() > MAX_ID || len > MAX_DATA {
            return Err((CoreError::Status(Status::BadRequest), 0));
        }
        data[..prefix.len()].copy_from_slice(prefix);
        data[prefix.len()..len].copy_from_slice(id.as_bytes());
        self.call(op, &data[..len], &[], reply)
    }
}

/// Splits `\0`-separated reply text into its parts.
pub fn parts(bytes: &[u8]) -> impl Iterator<Item = &str> {
    bytes
        .split(|&b| b == 0)
        .map(|part| core::str::from_utf8(part).unwrap_or("?"))
}
