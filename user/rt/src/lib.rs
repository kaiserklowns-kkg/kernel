//! Minimal Oceans userspace runtime (ABI version 3).
//!
//! Provides the program entry point ([`entry!`]), safe wrappers for the
//! system calls in `oceans-abi`, a panic handler and a small formatting
//! buffer. Programs use only these APIs, never kernel internals.

#![no_std]

use core::arch::asm;
use core::fmt;

pub use oceans_abi::Error;
pub use oceans_abi::{MessageDesc, prot, rights};
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

// ---- ABI 2 -----------------------------------------------------------------

/// Derives a handle with a subset of `handle`'s rights (`oceans_abi::rights`).
pub fn duplicate(handle: Handle, rights: u32) -> Result<Handle, Error> {
    call(
        nr::HANDLE_DUPLICATE,
        [handle.0, u64::from(rights), 0, 0, 0, 0],
    )
    .map(|(h, _)| Handle(h))
}

/// A new IPC endpoint: (server end, client end).
pub fn endpoint_create() -> Result<(Handle, Handle), Error> {
    call(nr::ENDPOINT_CREATE, [0; 6]).map(|(server, client)| (Handle(server), Handle(client)))
}

/// What a `*_msg` receive delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received {
    pub label: u64,
    pub data_len: usize,
    pub handles_len: usize,
}

fn send_desc(label: u64, data: &[u8], handles: &[Handle]) -> MessageDesc {
    MessageDesc {
        label,
        data: data.as_ptr() as u64,
        data_len: data.len() as u64,
        handles: handles.as_ptr() as u64,
        handles_len: handles.len() as u64,
    }
}

fn receive_desc(data: &mut [u8], handles: &mut [Handle]) -> MessageDesc {
    MessageDesc {
        label: 0,
        data: data.as_mut_ptr() as u64,
        data_len: data.len() as u64,
        handles: handles.as_mut_ptr() as u64,
        handles_len: handles.len() as u64,
    }
}

fn received(desc: &MessageDesc) -> Received {
    Received {
        label: desc.label,
        data_len: desc.data_len as usize,
        handles_len: desc.handles_len as usize,
    }
}

/// Calls the server behind `client`, moving `handles` to it (each needs
/// `TRANSFER`); the reply's data and handles land in the reply buffers.
pub fn ipc_call_msg(
    client: Handle,
    label: u64,
    data: &[u8],
    handles: &[Handle],
    reply_data: &mut [u8],
    reply_handles: &mut [Handle],
) -> Result<Received, Error> {
    let request = send_desc(label, data, handles);
    let mut reply = receive_desc(reply_data, reply_handles);
    let args = [
        client.0,
        &raw const request as u64,
        &raw mut reply as u64,
        0,
        0,
        0,
    ];
    call(nr::IPC_CALL_MSG, args)?;
    Ok(received(&reply))
}

/// Waits for a call on `server`, receiving its data and handles.
pub fn ipc_receive_msg(
    server: Handle,
    data: &mut [u8],
    handles: &mut [Handle],
) -> Result<Received, Error> {
    let mut desc = receive_desc(data, handles);
    call(
        nr::IPC_RECEIVE_MSG,
        [server.0, &raw mut desc as u64, 0, 0, 0, 0],
    )?;
    Ok(received(&desc))
}

/// Answers the pending call, moving `handles` to the caller.
pub fn ipc_reply_msg(label: u64, data: &[u8], handles: &[Handle]) -> Result<(), Error> {
    let desc = send_desc(label, data, handles);
    call(nr::IPC_REPLY_MSG, [&raw const desc as u64, 0, 0, 0, 0, 0]).map(drop)
}

/// A zero-filled memory object of at least `size` bytes.
pub fn memory_create(size: u64) -> Result<Handle, Error> {
    call(nr::MEMORY_CREATE, [size, 0, 0, 0, 0, 0]).map(|(h, _)| Handle(h))
}

/// Maps the whole object at `addr` (0: the kernel chooses) with
/// `oceans_abi::prot` bits; returns the address.
pub fn memory_map(memory: Handle, addr: u64, prot: u64) -> Result<*mut u8, Error> {
    call(nr::MEMORY_MAP, [memory.0, addr, prot, 0, 0, 0]).map(|(a, _)| a as *mut u8)
}

/// Removes the mapping that starts at `addr`.
pub fn memory_unmap(addr: *mut u8) -> Result<(), Error> {
    call(nr::MEMORY_UNMAP, [addr as u64, 0, 0, 0, 0, 0]).map(drop)
}

/// Starts a process from the ELF image in `image` (`len` 0: whole object),
/// moving `handles` to it; returns a process handle.
pub fn process_spawn(
    image: Handle,
    len: u64,
    handles: &[Handle],
    arg: u64,
) -> Result<Handle, Error> {
    let args = [
        image.0,
        len,
        handles.as_ptr() as u64,
        handles.len() as u64,
        arg,
        0,
    ];
    call(nr::PROCESS_SPAWN, args).map(|(h, _)| Handle(h))
}

/// Like [`process_spawn`], naming the process `<our name>/<name>` in logs
/// (`name` is truncated to `oceans_abi::PROCESS_NAME_MAX` bytes).
pub fn process_spawn_named(
    image: Handle,
    len: u64,
    handles: &[Handle],
    arg: u64,
    name: &str,
) -> Result<Handle, Error> {
    let mut buffer = [0u8; oceans_abi::PROCESS_NAME_MAX];
    let mut take = name.len().min(buffer.len());
    while !name.is_char_boundary(take) {
        take -= 1;
    }
    buffer[..take].copy_from_slice(&name.as_bytes()[..take]);
    let args = [
        image.0,
        len,
        handles.as_ptr() as u64,
        handles.len() as u64,
        arg,
        buffer.as_ptr() as u64,
    ];
    call(nr::PROCESS_SPAWN, args).map(|(h, _)| Handle(h))
}

/// Blocks until the process exits; returns its exit code.
pub fn process_wait(process: Handle) -> Result<i64, Error> {
    call(nr::PROCESS_WAIT, [process.0, 0, 0, 0, 0, 0]).map(|(_, code)| code as i64)
}

// ---- ABI 3 -----------------------------------------------------------------

/// A new notification (latched 64-bit signal word).
pub fn notification_create() -> Result<Handle, Error> {
    call(nr::NOTIFICATION_CREATE, [0; 6]).map(|(h, _)| Handle(h))
}

/// Sets `bits` on the notification (needs `SIGNAL`). Never blocks.
pub fn notification_signal(notification: Handle, bits: u64) -> Result<(), Error> {
    call(nr::NOTIFICATION_SIGNAL, [notification.0, bits, 0, 0, 0, 0]).map(drop)
}

/// Blocks until any bit is set; returns and clears them (needs `WAIT`).
pub fn notification_wait(notification: Handle) -> Result<u64, Error> {
    call(nr::NOTIFICATION_WAIT, [notification.0, 0, 0, 0, 0, 0]).map(|(bits, _)| bits)
}

/// Signals `bits` on `notification` when `process` exits.
pub fn process_watch(process: Handle, notification: Handle, bits: u64) -> Result<(), Error> {
    call(
        nr::PROCESS_WATCH,
        [process.0, notification.0, bits, 0, 0, 0],
    )
    .map(drop)
}

/// Blocks for at least `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    call(nr::SLEEP, [ms, 0, 0, 0, 0, 0]).ok();
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
