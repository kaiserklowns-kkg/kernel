//! The Oceans system call ABI, version 4 (ADR-0014 to ADR-0017).
//!
//! Shared by the kernel and userspace so both sides agree by construction.
//! The ABI is versioned: numbers and meanings below never change within a
//! version; additions bump [`ABI_VERSION`].
//!
//! # Calling convention (x86_64)
//!
//! `syscall` with the number in `rax` and up to six arguments in `rdi`,
//! `rsi`, `rdx`, `r10`, `r8`, `r9`. Results come back in `rax` (status or
//! primary value) and `rdx` (secondary value). `rcx` and `r11` are
//! clobbered by the instruction; all other registers are preserved.
//!
//! A negative `rax` is an [`Error`]; zero or positive is success.

#![no_std]

/// Version history: 1 = ADR-0014 (syscalls 0–7); 2 = ADR-0015 (8–17);
/// 3 = ADR-0016 (18–22); 4 = ADR-0017 (23–24). Versions only add; existing
/// numbers keep their meaning.
pub const ABI_VERSION: u64 = 4;

/// System call numbers.
pub mod nr {
    /// `() -> ABI_VERSION`
    pub const ABI_VERSION: u64 = 0;
    /// `(log_handle, ptr, len) -> 0` — needs `WRITE` on a log capability.
    pub const DEBUG_WRITE: u64 = 1;
    /// `(code) -> !` — ends the calling process.
    pub const EXIT: u64 = 2;
    /// `() -> 0`
    pub const YIELD: u64 = 3;
    /// `(handle) -> 0` — closes a capability.
    pub const HANDLE_CLOSE: u64 = 4;
    /// `(client_handle, label, ptr, len, reply_ptr, reply_capacity)
    /// -> (reply_len, reply_label)` — needs `SEND`.
    pub const IPC_CALL: u64 = 5;
    /// `(server_handle, ptr, capacity) -> (len, label)` — needs `RECEIVE`.
    /// The call becomes the thread's pending call, answered by `IPC_REPLY`.
    pub const IPC_RECEIVE: u64 = 6;
    /// `(label, ptr, len) -> 0` — answers the thread's pending call.
    pub const IPC_REPLY: u64 = 7;

    // ABI 2

    /// `(handle, rights) -> new_handle` — needs `DUPLICATE`; `rights` must
    /// be a subset of the source's.
    pub const HANDLE_DUPLICATE: u64 = 8;
    /// `() -> (server_handle, client_handle)` — a new IPC endpoint.
    pub const ENDPOINT_CREATE: u64 = 9;
    /// `(client, request: *const MessageDesc, reply: *mut MessageDesc) -> 0`
    /// — like `IPC_CALL`, moving the request's handles to the server and
    /// receiving the reply's handles. Needs `SEND`; sent handles need
    /// `TRANSFER`.
    pub const IPC_CALL_MSG: u64 = 10;
    /// `(server, message: *mut MessageDesc) -> 0` — needs `RECEIVE`.
    pub const IPC_RECEIVE_MSG: u64 = 11;
    /// `(reply: *const MessageDesc) -> 0` — answers the pending call.
    pub const IPC_REPLY_MSG: u64 = 12;
    /// `(size) -> handle` — a zero-filled memory object (rounded to pages).
    pub const MEMORY_CREATE: u64 = 13;
    /// `(handle, addr, prot) -> addr` — maps the whole object at `addr`
    /// (page-aligned; 0 = kernel chooses). Needs `MAP`, plus `READ`,
    /// `WRITE`, `EXECUTE` for the requested [`prot`] bits.
    pub const MEMORY_MAP: u64 = 14;
    /// `(addr) -> 0` — removes the mapping that starts at `addr`.
    pub const MEMORY_UNMAP: u64 = 15;
    /// `(image, image_len, handles: *const u64, handles_len, arg, name) ->
    /// process_handle` — starts a process from the ELF image held in the
    /// memory object `image` (needs `READ`), moving `handles` (each needs
    /// `TRANSFER`) to it as its initial capabilities. `name` (ABI 3): 0, or
    /// a pointer to a [`PROCESS_NAME_MAX`]-byte buffer holding a UTF-8 name,
    /// zero-padded; the process is named `<parent>/<name>` in logs.
    pub const PROCESS_SPAWN: u64 = 16;
    /// `(process) -> (0, exit_code)` — blocks until the process exits.
    /// Needs `WAIT`.
    pub const PROCESS_WAIT: u64 = 17;

    // ABI 3

    /// `() -> handle` — a notification (latched 64-bit signal word).
    pub const NOTIFICATION_CREATE: u64 = 18;
    /// `(notification, bits) -> 0` — needs `SIGNAL`. Never blocks.
    pub const NOTIFICATION_SIGNAL: u64 = 19;
    /// `(notification) -> bits` — blocks until any bit is set, returns and
    /// clears them. Needs `WAIT`.
    pub const NOTIFICATION_WAIT: u64 = 20;
    /// `(process, notification, bits) -> 0` — signals `bits` on the
    /// notification when the process exits (at once if it already has).
    /// Needs `WAIT` on the process and `SIGNAL` on the notification.
    pub const PROCESS_WATCH: u64 = 21;
    /// `(milliseconds) -> 0` — blocks for at least that long.
    pub const SLEEP: u64 = 22;

    // ABI 4

    /// `(console, ptr, capacity) -> count` — blocks until console input is
    /// available, then returns up to `capacity` raw bytes. Needs `READ`.
    pub const CONSOLE_READ: u64 = 23;
    /// `(console, ptr, len) -> len` — writes raw bytes (no log prefix, no
    /// translation) to the console. Needs `WRITE`.
    pub const CONSOLE_WRITE: u64 = 24;
}

/// `MEMORY_MAP` protection bits. Writable and executable together are
/// refused (`InvalidArgument`), and so is any mapping that would make one
/// memory object both writable and executable across mappings.
pub mod prot {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const EXECUTE: u64 = 1 << 2;
}

/// Capability rights as passed to `HANDLE_DUPLICATE` (same bits as the
/// kernel's `Rights`).
pub mod rights {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const EXECUTE: u32 = 1 << 2;
    pub const MAP: u32 = 1 << 3;
    pub const SEND: u32 = 1 << 4;
    pub const RECEIVE: u32 = 1 << 5;
    pub const SIGNAL: u32 = 1 << 6;
    pub const WAIT: u32 = 1 << 7;
    pub const MANAGE: u32 = 1 << 8;
    pub const DUPLICATE: u32 = 1 << 9;
    pub const TRANSFER: u32 = 1 << 10;
}

/// An IPC message in user memory, for the `*_MSG` calls.
///
/// Sending: `data`/`data_len` and `handles`/`handles_len` describe what to
/// send. Receiving: they describe the buffers and their capacities; the
/// kernel writes the received `label` and the actual lengths back. If the
/// message does not fit, the call fails with `TooLarge` and any
/// capabilities it carried are closed (authority is never leaked).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageDesc {
    pub label: u64,
    pub data: u64,
    pub data_len: u64,
    pub handles: u64,
    pub handles_len: u64,
}

/// Capabilities one message may carry.
pub const IPC_MAX_HANDLES: usize = 4;

/// Size of the `PROCESS_SPAWN` name buffer.
pub const PROCESS_NAME_MAX: usize = 32;

/// Largest program image accepted by `PROCESS_SPAWN`, in bytes.
pub const SPAWN_MAX_IMAGE: usize = 16 * 1024 * 1024;

/// Error codes, returned as negative values in `rax`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    /// No such system call in this ABI version.
    UnknownSyscall = -1,
    /// The handle does not name a capability in the caller's table.
    InvalidHandle = -2,
    /// The capability lacks a required right.
    MissingRights = -3,
    /// The capability names an object of the wrong type.
    WrongType = -4,
    /// A pointer/length pair is not readable or writable user memory.
    BadAddress = -5,
    /// The other end of an IPC endpoint is closed.
    PeerClosed = -6,
    /// The server dropped the call without replying.
    NoReply = -7,
    /// A message or buffer exceeds the ABI limits.
    TooLarge = -8,
    /// `IPC_REPLY` without a pending call.
    NoPendingCall = -9,
    /// The kernel could not allocate memory for the request.
    OutOfMemory = -10,
    /// The capability was revoked.
    Revoked = -11,
    /// An argument is malformed (misaligned, unknown bits, W+X, …).
    InvalidArgument = -12,
    /// The requested address range is already mapped.
    AddressInUse = -13,
    /// The program image is not an acceptable executable.
    InvalidImage = -14,
}

impl Error {
    pub const fn code(self) -> i64 {
        self as i64
    }

    pub const fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            -1 => Self::UnknownSyscall,
            -2 => Self::InvalidHandle,
            -3 => Self::MissingRights,
            -4 => Self::WrongType,
            -5 => Self::BadAddress,
            -6 => Self::PeerClosed,
            -7 => Self::NoReply,
            -8 => Self::TooLarge,
            -9 => Self::NoPendingCall,
            -10 => Self::OutOfMemory,
            -11 => Self::Revoked,
            -12 => Self::InvalidArgument,
            -13 => Self::AddressInUse,
            -14 => Self::InvalidImage,
            _ => return None,
        })
    }
}

/// Largest inline IPC payload, in bytes.
pub const IPC_MAX_INLINE: usize = 256;

/// Largest single `DEBUG_WRITE`, in bytes.
pub const DEBUG_WRITE_MAX: usize = 1024;

/// Largest single `CONSOLE_READ` or `CONSOLE_WRITE`, in bytes.
pub const CONSOLE_IO_MAX: usize = 4096;

/// Initial register state of a process's first thread: `rdi` holds the
/// number of initial capabilities, `rsi` a pointer to that many `u64`
/// handles (on its stack), `rdx` the process's argument word.
pub mod start {
    pub const MAX_INITIAL_HANDLES: usize = 16;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_round_trip_and_are_negative() {
        for code in -14..=-1 {
            let error = Error::from_code(code).expect("defined");
            assert_eq!(error.code(), code);
            assert!(error.code() < 0);
        }
        assert_eq!(Error::from_code(0), None);
        assert_eq!(Error::from_code(-15), None);
    }
}
