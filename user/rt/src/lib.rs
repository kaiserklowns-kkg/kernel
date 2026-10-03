//! Minimal Oceans userspace runtime (ABI version 1).
//!
//! Provides the program entry point ([`entry!`]), safe wrappers for the
//! system calls in `oceans-abi`, a panic handler and a small formatting
//! buffer. Programs use only these APIs, never kernel internals.

#![no_std]

use core::arch::asm;
use core::fmt;

pub use oceans_abi::Error;
use oceans_abi::{nr, start::MAX_INITIAL_HANDLES};

/// A capability handle in this process's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Handle(pub u64);

/// What the kernel passed to the process at start.
pub struct Start {
    pub handles: &'static [Handle],
    pub arg: u64,
}

/// Defines the program entry point: `fn main(Start) -> i64`, whose return
/// value is the exit code.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        extern "C" fn _start(count: u64, handles: *const u64, arg: u64) -> ! {
            // SAFETY: the kernel passes `count` handles at `handles` on the
            // initial stack (ABI `start`), which lives as long as the process.
            let start = unsafe { $crate::start_info(count, handles, arg) };
            let main: fn($crate::Start) -> i64 = $main;
            $crate::exit(main(start))
        }
    };
}

/// # Safety
///
/// Only for [`entry!`]: the arguments must be the kernel's start registers.
#[doc(hidden)]
pub unsafe fn start_info(count: u64, handles: *const u64, arg: u64) -> Start {
    let count = (count as usize).min(MAX_INITIAL_HANDLES);
    let handles = if count == 0 {
        &[]
    } else {
        // SAFETY: caller contract; `Handle` is `repr(transparent)` over u64.
        unsafe { core::slice::from_raw_parts(handles.cast::<Handle>(), count) }
    };
    Start { handles, arg }
}

/// Raw system call. Returns (`rax`, `rdx`).
///
/// # Safety
///
/// Pointer arguments must be valid for the call's contract (the kernel
/// checks them, but a wrong length could still overwrite our own memory).
#[inline]
pub unsafe fn syscall(number: u64, args: [u64; 6]) -> (i64, u64) {
    let rax: i64;
    let rdx: u64;
    // SAFETY: the Oceans syscall ABI: arguments in rdi, rsi, rdx, r10, r8,
    // r9; results in rax, rdx; rcx and r11 clobbered. The kernel also
    // zeroes the other argument registers, declared as clobbered here.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number as i64 => rax,
            inlateout("rdi") args[0] => _,
            inlateout("rsi") args[1] => _,
            inlateout("rdx") args[2] => rdx,
            inlateout("r10") args[3] => _,
            inlateout("r8") args[4] => _,
            inlateout("r9") args[5] => _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    (rax, rdx)
}

fn check((rax, rdx): (i64, u64)) -> Result<(u64, u64), Error> {
    if rax < 0 {
        Err(Error::from_code(rax).unwrap_or(Error::UnknownSyscall))
    } else {
        Ok((rax as u64, rdx))
    }
}

fn call(number: u64, args: [u64; 6]) -> Result<(u64, u64), Error> {
    // SAFETY: every wrapper below passes pointers derived from live slices
    // with their real lengths.
    check(unsafe { syscall(number, args) })
}

pub fn abi_version() -> u64 {
    call(nr::ABI_VERSION, [0; 6]).map_or(0, |(version, _)| version)
}

/// Writes `text` to the kernel log (needs a log capability with `WRITE`).
pub fn debug_write(log: Handle, text: &str) -> Result<(), Error> {
    call(
        nr::DEBUG_WRITE,
        [log.0, text.as_ptr() as u64, text.len() as u64, 0, 0, 0],
    )
    .map(drop)
}

pub fn exit(code: i64) -> ! {
    call(nr::EXIT, [code as u64, 0, 0, 0, 0, 0]).ok();
    // The kernel never returns from EXIT.
    loop {
        core::hint::spin_loop();
    }
}

pub fn yield_now() {
    call(nr::YIELD, [0; 6]).ok();
}

pub fn close(handle: Handle) -> Result<(), Error> {
    call(nr::HANDLE_CLOSE, [handle.0, 0, 0, 0, 0, 0]).map(drop)
}

/// Calls the server behind `client`; returns the reply length and label.
pub fn ipc_call(
    client: Handle,
    label: u64,
    data: &[u8],
    reply: &mut [u8],
) -> Result<(usize, u64), Error> {
    let args = [
        client.0,
        label,
        data.as_ptr() as u64,
        data.len() as u64,
        reply.as_mut_ptr() as u64,
        reply.len() as u64,
    ];
    call(nr::IPC_CALL, args).map(|(len, label)| (len as usize, label))
}

/// Waits for a call on `server`; returns its length and label. Answer it
/// with [`ipc_reply`].
pub fn ipc_receive(server: Handle, buffer: &mut [u8]) -> Result<(usize, u64), Error> {
    let args = [
        server.0,
        buffer.as_mut_ptr() as u64,
        buffer.len() as u64,
        0,
        0,
        0,
    ];
    call(nr::IPC_RECEIVE, args).map(|(len, label)| (len as usize, label))
}

pub fn ipc_reply(label: u64, data: &[u8]) -> Result<(), Error> {
    call(
        nr::IPC_REPLY,
        [label, data.as_ptr() as u64, data.len() as u64, 0, 0, 0],
    )
    .map(drop)
}

/// Fixed-capacity text buffer for formatting without an allocator.
pub struct Buffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Buffer<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are appended, so the contents are UTF-8.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl<const N: usize> Default for Buffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for Buffer<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self
            .len
            .checked_add(s.len())
            .filter(|&end| end <= N)
            .ok_or(fmt::Error)?;
        self.bytes[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Exit code of a process that panicked.
pub const PANIC_EXIT_CODE: i64 = -2;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    exit(PANIC_EXIT_CODE)
}
