//! The Oceans system call ABI, version 1 (ADR-0014).
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

pub const ABI_VERSION: u64 = 1;

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
}

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
            _ => return None,
        })
    }
}

/// Largest inline IPC payload, in bytes.
pub const IPC_MAX_INLINE: usize = 256;

/// Largest single `DEBUG_WRITE`, in bytes.
pub const DEBUG_WRITE_MAX: usize = 1024;

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
        for code in -11..=-1 {
            let error = Error::from_code(code).expect("defined");
            assert_eq!(error.code(), code);
            assert!(error.code() < 0);
        }
        assert_eq!(Error::from_code(0), None);
        assert_eq!(Error::from_code(-12), None);
    }
}
