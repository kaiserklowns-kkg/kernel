//! The Oceans SDK for Rust apps (ADR-0062): the System API at API level 1,
//! by area, so an app depends on this crate alone and never on kernel or
//! service internals.
//!
//! | Area | Here | What it reaches |
//! |---|---|---|
//! | Application | [`app`] | the entry point, the handle directory, the app's identity and arguments, the console |
//! | Storage | [`storage`] | the app's own data directory (`storage`), the user's files (`files`) |
//! | Network | [`network`] | TCP, UDP and names through the network service (`network`) |
//! | UI | [`ui`] | windows the system frames (`window`, ADR-0059) |
//! | System | [`system`] | memory and uptime, read-only (`system-info`) |
//! | Notification | [`notification`] | notifications on the desktop (`notifications`) |
//! | Permission | [`permission`] | the permission names a manifest may ask for |
//!
//! An app gets only what its manifest asks for and the user allows: each
//! area's handle is found in the directory (`None` when not granted), and
//! an app must keep working without it.
//!
//! The AI runtime is not open to apps at API level 1.

#![no_std]

extern crate alloc;

/// The app itself: its entry point, its handles, its identity.
pub mod app {
    pub use oceans_rt::{Buffer, Directory, Error, Handle, Out, Start, entry, exit};

    /// The app's identity as Oceans Core started it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Info<'a> {
        pub id: &'a str,
        pub version: &'a str,
    }

    /// The app's id and version (`app info`), if started by Oceans Core.
    pub fn info(directory: &Directory) -> Option<Info<'static>> {
        let text = directory
            .find("app", "info")
            .and_then(oceans_rt::map_text)?;
        let (id, version) = text.trim().split_once(' ')?;
        Some(Info { id, version })
    }

    /// The console the app was started from (`console`), if any.
    pub fn console(directory: &Directory) -> Option<Out> {
        directory.find("console", "out").map(Out::new)
    }

    /// The arguments given at start (empty if none).
    pub fn args(directory: &Directory) -> &'static str {
        directory.args()
    }
}

/// Files: the app's own data directory, and the user's files.
pub mod storage {
    pub use oceans_fs_proto::{FsError, Kind, Node, flags};

    use super::app::Directory;

    /// The app's own data directory (`storage`).
    pub fn data(directory: &Directory) -> Option<Node> {
        directory.find("use", "storage").map(Node)
    }

    /// The user's files, `/home` (`files`).
    pub fn files(directory: &Directory) -> Option<Node> {
        directory.find("use", "files").map(Node)
    }
}

/// The network service: TCP streams, UDP sockets, name lookups.
pub mod network {
    pub use oceans_net_proto::*;

    use super::app::{Directory, Handle};

    /// The network service (`network`).
    pub fn service(directory: &Directory) -> Option<Handle> {
        directory.find("use", "net")
    }
}

/// Windows (ADR-0059): the system draws the frame, the app the pixels.
pub mod ui {
    pub use oceans_display_proto::{Event, Status, Window, WindowError, events, kind, proto};

    use super::app::{Directory, Handle};

    /// The window endpoint (`window`).
    pub fn windows(directory: &Directory) -> Option<Handle> {
        directory.find("use", "windows")
    }
}

/// Notifications on the desktop (`notifications`, ADR-0065): one short
/// line, shown after the app's name as the system knows it.
pub mod notification {
    pub use oceans_display_proto::WindowError as Error;

    use super::app::Directory;

    /// Shows `text` (one line, at most 120 bytes; one every 3 s).
    pub fn notify(directory: &Directory, text: &str) -> Result<(), Error> {
        let display = super::ui::windows(directory)
            .ok_or(Error::Refused(oceans_display_proto::Status::NotAllowed))?;
        oceans_display_proto::notify(display, text)
    }
}

/// Read-only system information (`system-info`).
pub mod system {
    use oceans_abi::sysinfo;

    use super::app::{Directory, Error, Handle};

    /// Memory, in bytes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Memory {
        pub total: u64,
        pub free: u64,
    }

    /// The system-information capability.
    pub fn handle(directory: &Directory) -> Option<Handle> {
        directory.find("sysinfo", "sysinfo")
    }

    pub fn memory(sysinfo: Handle) -> Result<Memory, Error> {
        let mut record = [0u8; sysinfo::MemoryInfo::SIZE];
        oceans_rt::system_info(sysinfo, sysinfo::MEMORY, &mut record)?;
        let info = sysinfo::MemoryInfo::decode(&record).ok_or(Error::InvalidArgument)?;
        Ok(Memory {
            total: info.total_frames * info.page_size,
            free: info.free_frames * info.page_size,
        })
    }

    /// Milliseconds since boot.
    pub fn uptime_ms() -> u64 {
        oceans_rt::clock_ms()
    }
}

/// The permissions a manifest may ask for (`permission = NAME: reason`).
pub mod permission {
    pub use oceans_package::Permission;
}
