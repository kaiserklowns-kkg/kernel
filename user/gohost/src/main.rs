//! The Oceans Go host (ADR-0050): runs a Go program built for WebAssembly
//! (`GOOS=wasip1 GOARCH=wasm`) as an Oceans process.
//!
//! Its authority is the process's, granted as usual (init's `services.conf`
//! or Core): the host gives the module nothing else. The module sees:
//! - **the `oceans` host functions**: handle-directory lookup, IPC (call,
//!   receive, reply, mint), notifications and timers, published text
//!   (making it, and reading it: `read_text`, ADR-0052), system
//!   information, and reading memory objects the process holds (a boot
//!   module, or one handed over IPC; ADR-0054). Each checks every pointer
//!   range against the
//!   module's memory and passes the call to the kernel for the process's
//!   own capabilities. This is the binding `go/oceans` wraps.
//! - **a WASI preview 1 subset**, enough for the Go runtime: standard
//!   output and error (to the console if granted, else the log), arguments,
//!   clocks, random numbers, sleeping (`poll_oneoff` on clocks), exit. No
//!   files or sockets: those are Oceans services, reached over IPC.
//!
//! The program is the first `module` in the handle directory: a boot
//! module (`grant = module:NAME.wasm`), or the program of a `wasm` app,
//! which Oceans Core hands over read-only (ADR-0052). The interpreter is
//! wasmi; it uses
//! no floating-point hardware state (soft float), as Oceans user code must
//! not.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_rt::{Buffer, Directory, Error, Handle, Out, Start, prot};
use wasmi::{Caller, Engine, Extern, Linker, Memory, Module, Store};

oceans_rt::entry!(main);

const EXIT_BAD_START: i64 = 2;
const EXIT_NO_MODULE: i64 = 3;
const EXIT_BAD_MODULE: i64 = 4;
const EXIT_TRAP: i64 = 5;

/// WASI errno values used here.
const ERRNO_SUCCESS: i32 = 0;
const ERRNO_BADF: i32 = 8;
const ERRNO_FAULT: i32 = 21;
const ERRNO_INVAL: i32 = 28;
const ERRNO_NOSYS: i32 = 52;
const ERRNO_NOTSUP: i32 = 58;

/// Inline IPC limits (ADR-0013).
const MAX_DATA: usize = 256;
const MAX_HANDLES: usize = 4;

struct Host {
    directory: Directory,
    name: &'static str,
    log: Option<Handle>,
    out: Option<Out>,
    /// Standard output and error without a console: complete lines go to
    /// the log.
    line: Vec<u8>,
}

/// The interpreter's stack. wasmi runs WebAssembly calls on its own stack
/// (loop dispatch, `portable-dispatch`: measured at 11 KiB of native stack
/// for gohello); this leaves room for translating larger modules beyond a
/// process's 64 KiB.
const INTERPRETER_STACK: usize = 1 << 20;

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    oceans_rt::with_stack(INTERPRETER_STACK, || run(directory)).unwrap_or(EXIT_BAD_START)
}

fn run(directory: Directory) -> i64 {
    let log = directory.find("log", "log");
    let say = |args: core::fmt::Arguments<'_>| {
        if let Some(log) = log {
            let mut line = Buffer::<200>::new();
            let _ = line.write_fmt(args);
            let _ = oceans_rt::debug_write(log, line.as_str());
        }
    };
    let Some((name, image)) = module(&directory) else {
        say(format_args!("gohost: no program module granted"));
        return EXIT_NO_MODULE;
    };
    let out = directory
        .find("console", "out")
        .or_else(|| directory.find("console", "console"))
        .map(Out::new);
    let engine = Engine::default();
    let module = match Module::new(&engine, image) {
        Ok(module) => module,
        Err(error) => {
            say(format_args!("gohost: {name}: not a valid module: {error}"));
            return EXIT_BAD_MODULE;
        }
    };
    let mut store = Store::new(
        &engine,
        Host {
            directory,
            name,
            log,
            out,
            line: Vec::new(),
        },
    );
    let mut linker = Linker::<Host>::new(&engine);
    if let Err(error) = define(&mut linker) {
        say(format_args!(
            "gohost: cannot define host functions: {error}"
        ));
        return EXIT_BAD_MODULE;
    }
    let instance = match linker.instantiate_and_start(&mut store, &module) {
        Ok(instance) => instance,
        Err(error) => {
            say(format_args!("gohost: {name}: cannot instantiate: {error}"));
            return EXIT_BAD_MODULE;
        }
    };
    let Ok(entry) = instance.get_typed_func::<(), ()>(&store, "_start") else {
        say(format_args!(
            "gohost: {name}: no _start (not a wasip1 command)"
        ));
        return EXIT_BAD_MODULE;
    };
    let code = match entry.call(&mut store, ()) {
        Ok(()) => 0,
        Err(error) => match error.i32_exit_status() {
            Some(code) => i64::from(code),
            None => {
                say(format_args!("gohost: {name}: trapped: {error}"));
                EXIT_TRAP
            }
        },
    };
    flush_line(store.data_mut());
    code
}

/// The first `module` of the directory, mapped read-only for good.
fn module(directory: &Directory) -> Option<(&'static str, &'static [u8])> {
    let name = directory.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some(_), Some("module"), Some(name)) => Some(name),
            _ => None,
        }
    })?;
    let memory = directory.find("module", name)?;
    let size = usize::try_from(oceans_rt::memory_size(memory).ok()?).ok()?;
    let base = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: the object is mapped readable at `base` for the rest of the
    // process's life (never unmapped).
    let bytes = unsafe { core::slice::from_raw_parts(base, size) };
    Some((name, &bytes[..module_len(bytes)]))
}

/// The length of the WebAssembly module at the start of `bytes`: memory
/// objects are whole pages, so zeros may follow it. Sections are walked
/// until the end or a zero-sized section 0; a real custom section (id 0)
/// holds at least its name's length, so that can only be padding.
fn module_len(bytes: &[u8]) -> usize {
    let mut at = 8; // magic and version
    while at < bytes.len() {
        let id = bytes[at];
        let Some((size, used)) = leb128(&bytes[at + 1..]) else {
            break;
        };
        if id == 0 && size == 0 {
            return at;
        }
        match (at + 1 + used).checked_add(size as usize) {
            Some(end) if end <= bytes.len() => at = end,
            // Not a module we can delimit: let the parser judge all of it.
            _ => return bytes.len(),
        }
    }
    at.min(bytes.len())
}

/// An unsigned LEB128 value (at most 32 bits) and its length in bytes.
fn leb128(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut value = 0u32;
    for (i, &byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

fn flush_line(host: &mut Host) {
    if host.line.is_empty() {
        return;
    }
    if let Some(log) = host.log {
        let text = String::from_utf8_lossy(&host.line);
        let _ = oceans_rt::debug_write(log, text.trim_end());
    }
    host.line.clear();
}

/// Standard output and error: the console if granted, else the log, a line
/// at a time.
fn write_out(host: &mut Host, bytes: &[u8]) {
    if let Some(out) = &mut host.out {
        let _ = out.write_bytes(bytes);
        return;
    }
    for &byte in bytes {
        if byte == b'\n' {
            flush_line(host);
        } else if host.line.len() < 240 {
            host.line.push(byte);
        }
    }
}

// ---- Module memory ------------------------------------------------------------

fn memory(caller: &Caller<'_, Host>) -> Option<Memory> {
    caller.get_export("memory").and_then(Extern::into_memory)
}

/// A copy of `len` bytes at `ptr`, if they lie in the module's memory.
fn read(caller: &Caller<'_, Host>, ptr: u32, len: u32) -> Option<Vec<u8>> {
    let memory = memory(caller)?;
    let data = memory.data(caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    data.get(start..end).map(<[u8]>::to_vec)
}

fn write(caller: &mut Caller<'_, Host>, ptr: u32, bytes: &[u8]) -> bool {
    let Some(memory) = memory(caller) else {
        return false;
    };
    let data = memory.data_mut(caller);
    let start = ptr as usize;
    match start
        .checked_add(bytes.len())
        .and_then(|end| data.get_mut(start..end))
    {
        Some(target) => {
            target.copy_from_slice(bytes);
            true
        }
        None => false,
    }
}

fn handles_from(bytes: &[u8]) -> Vec<Handle> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&h| Handle(u64::from_le_bytes(h)))
        .collect()
}

fn handles_bytes(handles: &[Handle]) -> Vec<u8> {
    handles.iter().flat_map(|h| h.0.to_le_bytes()).collect()
}

fn code(error: Error) -> i64 {
    error.code()
}

const BAD_ADDRESS: i64 = Error::BadAddress.code();
const TOO_LARGE: i64 = Error::TooLarge.code();

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("")
}

// ---- Host functions -----------------------------------------------------------

fn define(linker: &mut Linker<Host>) -> Result<(), wasmi::Error> {
    define_oceans(linker)?;
    define_wasi(linker)
}

fn define_oceans(linker: &mut Linker<Host>) -> Result<(), wasmi::Error> {
    const M: &str = "oceans";
    linker.func_wrap(
        M,
        "handle_find",
        |caller: Caller<'_, Host>, kind: u32, kind_len: u32, name: u32, name_len: u32| -> i64 {
            let (Some(kind), Some(name)) =
                (read(&caller, kind, kind_len), read(&caller, name, name_len))
            else {
                return BAD_ADDRESS;
            };
            match caller.data().directory.find(text(&kind), text(&name)) {
                Some(handle) => handle.0 as i64,
                None => Error::NotFound.code(),
            }
        },
    )?;
    linker.func_wrap(
        M,
        "args",
        |mut caller: Caller<'_, Host>, buf: u32, cap: u32| -> i64 {
            let args = caller.data().directory.args().as_bytes();
            let len = args.len().min(cap as usize);
            let copy = args[..len].to_vec();
            if write(&mut caller, buf, &copy) {
                len as i64
            } else {
                BAD_ADDRESS
            }
        },
    )?;
    linker.func_wrap(
        M,
        "debug_write",
        |caller: Caller<'_, Host>, log: u64, ptr: u32, len: u32| -> i64 {
            match read(&caller, ptr, len.min(512)) {
                Some(bytes) => {
                    oceans_rt::debug_write(Handle(log), text(&bytes)).map_or_else(code, |()| 0)
                }
                None => BAD_ADDRESS,
            }
        },
    )?;
    linker.func_wrap(M, "close", |_: Caller<'_, Host>, handle: u64| -> i64 {
        oceans_rt::close(Handle(handle)).map_or_else(code, |()| 0)
    })?;
    linker.func_wrap(
        M,
        "duplicate",
        |_: Caller<'_, Host>, handle: u64, rights: u32| -> i64 {
            oceans_rt::duplicate(Handle(handle), rights).map_or_else(code, |h| h.0 as i64)
        },
    )?;
    linker.func_wrap(
        M,
        "ipc_call",
        |mut caller: Caller<'_, Host>,
         client: u64,
         label: u64,
         data: u32,
         data_len: u32,
         handles: u32,
         handles_len: u32,
         reply: u32,
         reply_cap: u32,
         reply_handles: u32,
         reply_handles_cap: u32,
         result: u32|
         -> i64 {
            if data_len as usize > MAX_DATA || handles_len as usize > MAX_HANDLES {
                return TOO_LARGE;
            }
            let (Some(data), Some(sent)) = (
                read(&caller, data, data_len),
                read(&caller, handles, handles_len * 8),
            ) else {
                return BAD_ADDRESS;
            };
            let sent = handles_from(&sent);
            let mut reply_data = [0u8; MAX_DATA];
            let mut received = [Handle(0); MAX_HANDLES];
            let reply_cap = (reply_cap as usize).min(MAX_DATA);
            let handles_cap = (reply_handles_cap as usize).min(MAX_HANDLES);
            let got = match oceans_rt::ipc_call_msg(
                Handle(client),
                label,
                &data,
                &sent,
                &mut reply_data[..reply_cap],
                &mut received[..handles_cap],
            ) {
                Ok(got) => got,
                Err(error) => return code(error),
            };
            let mut summary = [0u8; 16];
            summary[..8].copy_from_slice(&got.label.to_le_bytes());
            summary[8..12].copy_from_slice(&(got.data_len as u32).to_le_bytes());
            summary[12..].copy_from_slice(&(got.handles_len as u32).to_le_bytes());
            let ok = write(&mut caller, reply, &reply_data[..got.data_len])
                && write(
                    &mut caller,
                    reply_handles,
                    &handles_bytes(&received[..got.handles_len]),
                )
                && write(&mut caller, result, &summary);
            if ok { 0 } else { BAD_ADDRESS }
        },
    )?;
    linker.func_wrap(
        M,
        "ipc_receive",
        |mut caller: Caller<'_, Host>,
         server: u64,
         data: u32,
         data_cap: u32,
         handles: u32,
         handles_cap: u32,
         result: u32|
         -> i64 {
            let mut buffer = [0u8; MAX_DATA];
            let mut received = [Handle(0); MAX_HANDLES];
            let data_cap = (data_cap as usize).min(MAX_DATA);
            let handles_cap = (handles_cap as usize).min(MAX_HANDLES);
            let got = match oceans_rt::ipc_receive_msg(
                Handle(server),
                &mut buffer[..data_cap],
                &mut received[..handles_cap],
            ) {
                Ok(got) => got,
                Err(error) => return code(error),
            };
            let mut summary = [0u8; 40];
            summary[..8].copy_from_slice(&got.label.to_le_bytes());
            summary[8..12].copy_from_slice(&(got.data_len as u32).to_le_bytes());
            summary[12..16].copy_from_slice(&(got.handles_len as u32).to_le_bytes());
            summary[16..24].copy_from_slice(&got.badge.to_le_bytes());
            summary[24..32].copy_from_slice(&u64::from(got.closed).to_le_bytes());
            summary[32..].copy_from_slice(&got.signals.to_le_bytes());
            let ok = write(&mut caller, data, &buffer[..got.data_len])
                && write(
                    &mut caller,
                    handles,
                    &handles_bytes(&received[..got.handles_len]),
                )
                && write(&mut caller, result, &summary);
            if ok { 0 } else { BAD_ADDRESS }
        },
    )?;
    linker.func_wrap(
        M,
        "ipc_reply",
        |caller: Caller<'_, Host>,
         label: u64,
         data: u32,
         data_len: u32,
         handles: u32,
         handles_len: u32|
         -> i64 {
            if data_len as usize > MAX_DATA || handles_len as usize > MAX_HANDLES {
                return TOO_LARGE;
            }
            let (Some(data), Some(sent)) = (
                read(&caller, data, data_len),
                read(&caller, handles, handles_len * 8),
            ) else {
                return BAD_ADDRESS;
            };
            oceans_rt::ipc_reply_msg(label, &data, &handles_from(&sent)).map_or_else(code, |()| 0)
        },
    )?;
    linker.func_wrap(
        M,
        "endpoint_mint",
        |_: Caller<'_, Host>, server: u64, badge: u64| -> i64 {
            if badge == 0 {
                return Error::InvalidArgument.code();
            }
            oceans_rt::endpoint_mint(Handle(server), badge).map_or_else(code, |h| h.0 as i64)
        },
    )?;
    linker.func_wrap(M, "notification_create", |_: Caller<'_, Host>| -> i64 {
        oceans_rt::notification_create().map_or_else(code, |h| h.0 as i64)
    })?;
    linker.func_wrap(
        M,
        "notification_wait",
        |_: Caller<'_, Host>, notification: u64| -> i64 {
            // Bits are below 2^63 by convention, so they stay non-negative.
            oceans_rt::notification_wait(Handle(notification)).map_or_else(code, |bits| bits as i64)
        },
    )?;
    linker.func_wrap(
        M,
        "endpoint_bind",
        |_: Caller<'_, Host>, server: u64, notification: u64| -> i64 {
            oceans_rt::endpoint_bind(Handle(server), Handle(notification)).map_or_else(code, |()| 0)
        },
    )?;
    linker.func_wrap(
        M,
        "timer_set",
        |_: Caller<'_, Host>, notification: u64, bits: u64, ms: u64| -> i64 {
            oceans_rt::timer_set(Handle(notification), bits, ms).map_or_else(code, |()| 0)
        },
    )?;
    linker.func_wrap(
        M,
        "publish_text",
        |caller: Caller<'_, Host>, ptr: u32, len: u32| -> i64 {
            match read(&caller, ptr, len) {
                Some(bytes) => oceans_rt::publish_text(&bytes).map_or_else(code, |h| h.0 as i64),
                None => BAD_ADDRESS,
            }
        },
    )?;
    linker.func_wrap(
        M,
        "system_info",
        |mut caller: Caller<'_, Host>, sysinfo: u64, kind: u64, buf: u32, cap: u32| -> i64 {
            let mut buffer = alloc::vec![0u8; (cap as usize).min(64 * 1024)];
            match oceans_rt::system_info(Handle(sysinfo), kind, &mut buffer) {
                Ok(len) => {
                    if write(&mut caller, buf, &buffer[..len]) {
                        len as i64
                    } else {
                        BAD_ADDRESS
                    }
                }
                Err(error) => code(error),
            }
        },
    )?;
    linker.func_wrap(
        M,
        "read_text",
        |mut caller: Caller<'_, Host>, memory: u64, buf: u32, cap: u32| -> i64 {
            match read_text(Handle(memory), cap) {
                Ok(text) => {
                    if write(&mut caller, buf, &text) {
                        text.len() as i64
                    } else {
                        BAD_ADDRESS
                    }
                }
                Err(error) => code(error),
            }
        },
    )?;
    linker.func_wrap(
        M,
        "memory_size",
        |_: Caller<'_, Host>, memory: u64| -> i64 {
            oceans_rt::memory_size(Handle(memory))
                .map_or_else(code, |size| i64::try_from(size).unwrap_or(TOO_LARGE))
        },
    )?;
    linker.func_wrap(
        M,
        "memory_read",
        |mut caller: Caller<'_, Host>, memory: u64, offset: u64, buf: u32, cap: u32| -> i64 {
            memory_read(&mut caller, Handle(memory), offset, buf, cap)
        },
    )?;
    Ok(())
}

/// Most bytes `read_text` copies (published text is small: an app's
/// identity, its arguments).
const MAX_TEXT: usize = 64 * 1024;

/// The text in memory object `memory` (as `publish_text` makes them: up to
/// the first NUL), if it fits in `cap` bytes (at most [`MAX_TEXT`]): a
/// copy, so the module never sees the mapping. `TooLarge` if it does not
/// fit, `InvalidArgument` if it is not UTF-8; the kernel refuses handles
/// that are not memory objects or not mappable.
fn read_text(memory: Handle, cap: u32) -> Result<Vec<u8>, Error> {
    let size = usize::try_from(oceans_rt::memory_size(memory)?).map_err(|_| Error::TooLarge)?;
    let limit = (cap as usize).min(MAX_TEXT);
    let take = size.min(limit);
    let base = oceans_rt::memory_map(memory, 0, prot::READ)?;
    let mut bytes = alloc::vec![0u8; take];
    // SAFETY: the whole object (`size` >= `take` bytes) is mapped readable
    // at `base` until the unmap below; it is copied, never referenced, as
    // another process may hold the object writable.
    unsafe { core::ptr::copy_nonoverlapping(base, bytes.as_mut_ptr(), take) };
    let _ = oceans_rt::memory_unmap(base);
    match bytes.iter().position(|&b| b == 0) {
        Some(len) => bytes.truncate(len),
        None if size > take => return Err(Error::TooLarge),
        None => {}
    }
    core::str::from_utf8(&bytes).map_err(|_| Error::InvalidArgument)?;
    Ok(bytes)
}

/// Copies up to `cap` bytes of memory object `memory`, from `offset`, to
/// `buf` in the module's memory (the process needs `READ` and `MAP` on
/// it); returns how many. The object is mapped read-only only for the
/// copy.
fn memory_read(
    caller: &mut Caller<'_, Host>,
    memory: Handle,
    offset: u64,
    buf: u32,
    cap: u32,
) -> i64 {
    let size = match oceans_rt::memory_size(memory) {
        Ok(size) => size,
        Err(error) => return code(error),
    };
    let Some(available) = size.checked_sub(offset) else {
        return 0;
    };
    let len = available.min(u64::from(cap)) as usize;
    if len == 0 {
        return 0;
    }
    let base = match oceans_rt::memory_map(memory, 0, prot::READ) {
        Ok(base) => base,
        Err(error) => return code(error),
    };
    // SAFETY: the whole object (`size` bytes, `offset + len <= size`) is
    // mapped readable at `base` until the unmap below.
    let bytes = unsafe { core::slice::from_raw_parts(base.add(offset as usize), len) };
    let copied = write(caller, buf, bytes);
    let _ = oceans_rt::memory_unmap(base);
    if copied { len as i64 } else { BAD_ADDRESS }
}

fn define_wasi(linker: &mut Linker<Host>) -> Result<(), wasmi::Error> {
    const W: &str = "wasi_snapshot_preview1";
    linker.func_wrap(
        W,
        "fd_write",
        |mut caller: Caller<'_, Host>, fd: i32, iovs: u32, count: u32, written: u32| -> i32 {
            if fd != 1 && fd != 2 {
                return ERRNO_BADF;
            }
            let mut total = 0u32;
            for i in 0..count {
                let Some(iov) = read(&caller, iovs + 8 * i, 8) else {
                    return ERRNO_FAULT;
                };
                let base = u32::from_le_bytes(iov[..4].try_into().unwrap());
                let len = u32::from_le_bytes(iov[4..].try_into().unwrap());
                let Some(bytes) = read(&caller, base, len) else {
                    return ERRNO_FAULT;
                };
                write_out(caller.data_mut(), &bytes);
                total += len;
            }
            if write(&mut caller, written, &total.to_le_bytes()) {
                ERRNO_SUCCESS
            } else {
                ERRNO_FAULT
            }
        },
    )?;
    linker.func_wrap(
        W,
        "args_sizes_get",
        |mut caller: Caller<'_, Host>, argc: u32, size: u32| -> i32 {
            let (count, bytes) = argv(caller.data());
            let ok = write(&mut caller, argc, &(count as u32).to_le_bytes())
                && write(&mut caller, size, &(bytes.len() as u32).to_le_bytes());
            if ok { ERRNO_SUCCESS } else { ERRNO_FAULT }
        },
    )?;
    linker.func_wrap(
        W,
        "args_get",
        |mut caller: Caller<'_, Host>, argv_ptr: u32, buf: u32| -> i32 {
            let (_, bytes) = argv(caller.data());
            let mut pointers = Vec::new();
            let mut offset = 0u32;
            for arg in bytes.split_inclusive(|&b| b == 0) {
                pointers.extend_from_slice(&(buf + offset).to_le_bytes());
                offset += arg.len() as u32;
            }
            let ok = write(&mut caller, argv_ptr, &pointers) && write(&mut caller, buf, &bytes);
            if ok { ERRNO_SUCCESS } else { ERRNO_FAULT }
        },
    )?;
    linker.func_wrap(
        W,
        "environ_sizes_get",
        |mut caller: Caller<'_, Host>, count: u32, size: u32| -> i32 {
            let ok = write(&mut caller, count, &0u32.to_le_bytes())
                && write(&mut caller, size, &0u32.to_le_bytes());
            if ok { ERRNO_SUCCESS } else { ERRNO_FAULT }
        },
    )?;
    linker.func_wrap(
        W,
        "environ_get",
        |_: Caller<'_, Host>, _: u32, _: u32| -> i32 { ERRNO_SUCCESS },
    )?;
    linker.func_wrap(
        W,
        "clock_time_get",
        |mut caller: Caller<'_, Host>, id: u32, _precision: u64, time: u32| -> i32 {
            let ns = match id {
                // Realtime (UTC); before the clock is known, uptime.
                0 => oceans_rt::unix_time_ms().unwrap_or_else(oceans_rt::clock_ms) * 1_000_000,
                // Monotonic, process and thread CPU time: uptime.
                1..=3 => oceans_rt::clock_ms() * 1_000_000,
                _ => return ERRNO_INVAL,
            };
            if write(&mut caller, time, &ns.to_le_bytes()) {
                ERRNO_SUCCESS
            } else {
                ERRNO_FAULT
            }
        },
    )?;
    linker.func_wrap(
        W,
        "clock_res_get",
        |mut caller: Caller<'_, Host>, _id: u32, resolution: u32| -> i32 {
            // The clock ticks every 10 ms.
            if write(&mut caller, resolution, &10_000_000u64.to_le_bytes()) {
                ERRNO_SUCCESS
            } else {
                ERRNO_FAULT
            }
        },
    )?;
    linker.func_wrap(
        W,
        "random_get",
        |mut caller: Caller<'_, Host>, buf: u32, len: u32| -> i32 {
            let mut bytes = alloc::vec![0u8; len as usize];
            if oceans_rt::random(&mut bytes).is_err() {
                return ERRNO_NOSYS;
            }
            if write(&mut caller, buf, &bytes) {
                ERRNO_SUCCESS
            } else {
                ERRNO_FAULT
            }
        },
    )?;
    linker.func_wrap(
        W,
        "poll_oneoff",
        |mut caller: Caller<'_, Host>, input: u32, output: u32, count: u32, events: u32| -> i32 {
            poll_oneoff(&mut caller, input, output, count, events)
        },
    )?;
    linker.func_wrap(W, "sched_yield", |_: Caller<'_, Host>| -> i32 {
        oceans_rt::yield_now();
        ERRNO_SUCCESS
    })?;
    linker.func_wrap(
        W,
        "proc_exit",
        |mut caller: Caller<'_, Host>, status: i32| -> Result<(), wasmi::Error> {
            flush_line(caller.data_mut());
            Err(wasmi::Error::i32_exit(status))
        },
    )?;
    linker.func_wrap(W, "fd_close", |_: Caller<'_, Host>, fd: i32| -> i32 {
        if (0..=2).contains(&fd) {
            ERRNO_SUCCESS
        } else {
            ERRNO_BADF
        }
    })?;
    linker.func_wrap(
        W,
        "fd_fdstat_get",
        |mut caller: Caller<'_, Host>, fd: i32, stat: u32| -> i32 {
            if !(0..=2).contains(&fd) {
                return ERRNO_BADF;
            }
            // A character device; rights: everything.
            let mut bytes = [0u8; 24];
            bytes[0] = 2;
            bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
            bytes[16..].copy_from_slice(&u64::MAX.to_le_bytes());
            if write(&mut caller, stat, &bytes) {
                ERRNO_SUCCESS
            } else {
                ERRNO_FAULT
            }
        },
    )?;
    linker.func_wrap(
        W,
        "fd_fdstat_set_flags",
        |_: Caller<'_, Host>, fd: i32, _: u32| -> i32 {
            if (0..=2).contains(&fd) {
                ERRNO_SUCCESS
            } else {
                ERRNO_BADF
            }
        },
    )?;
    // No preopened directories: files are an Oceans service.
    linker.func_wrap(
        W,
        "fd_prestat_get",
        |_: Caller<'_, Host>, _: i32, _: u32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "fd_prestat_dir_name",
        |_: Caller<'_, Host>, _: i32, _: u32, _: u32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "fd_read",
        |_: Caller<'_, Host>, _: i32, _: u32, _: u32, _: u32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "fd_seek",
        |_: Caller<'_, Host>, _: i32, _: i64, _: u32, _: u32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "fd_filestat_get",
        |_: Caller<'_, Host>, _: i32, _: u32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "path_open",
        |_: Caller<'_, Host>,
         _: i32,
         _: u32,
         _: u32,
         _: u32,
         _: u32,
         _: u64,
         _: u64,
         _: u32,
         _: u32|
         -> i32 { ERRNO_NOTSUP },
    )?;
    // What Go's `os` package imports as well (pulled in by `net` and
    // `crypto/tls`, ADR-0054): there are no files through WASI.
    linker.func_wrap(
        W,
        "fd_readdir",
        |_: Caller<'_, Host>, _: i32, _: u32, _: u32, _: u64, _: u32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "path_filestat_get",
        |_: Caller<'_, Host>, _: i32, _: u32, _: u32, _: u32, _: u32| -> i32 { ERRNO_NOTSUP },
    )?;
    linker.func_wrap(
        W,
        "path_readlink",
        |_: Caller<'_, Host>, _: i32, _: u32, _: u32, _: u32, _: u32, _: u32| -> i32 {
            ERRNO_NOTSUP
        },
    )?;
    Ok(())
}

/// `argv` as WASI wants it: the program's name, then its arguments, each
/// NUL-terminated.
fn argv(host: &Host) -> (usize, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut count = 0;
    for word in core::iter::once(host.name).chain(host.directory.args().split_whitespace()) {
        bytes.extend_from_slice(word.as_bytes());
        bytes.push(0);
        count += 1;
    }
    (count, bytes)
}

/// `poll_oneoff`: clock subscriptions only (sleeping). The earliest clock
/// deadline is slept until, then every due clock is reported; descriptor
/// subscriptions are answered `NOTSUP` at once.
fn poll_oneoff(
    caller: &mut Caller<'_, Host>,
    input: u32,
    output: u32,
    count: u32,
    events: u32,
) -> i32 {
    const SUBSCRIPTION: u32 = 48;
    const EVENT: usize = 32;
    if count == 0 {
        return ERRNO_INVAL;
    }
    let Some(subscriptions) = read(caller, input, count * SUBSCRIPTION) else {
        return ERRNO_FAULT;
    };
    let now_ns = || oceans_rt::clock_ms() * 1_000_000;
    let start = now_ns();
    let mut clocks = Vec::new();
    let mut out = Vec::new();
    for sub in subscriptions.as_chunks::<{ SUBSCRIPTION as usize }>().0 {
        let userdata = &sub[..8];
        let tag = sub[8];
        let mut event = [0u8; EVENT];
        event[..8].copy_from_slice(userdata);
        event[10] = tag;
        if tag == 0 {
            let timeout = u64::from_le_bytes(sub[24..32].try_into().unwrap());
            let absolute = u16::from_le_bytes(sub[40..42].try_into().unwrap()) & 1 != 0;
            let deadline = if absolute {
                timeout
            } else {
                start.saturating_add(timeout)
            };
            clocks.push((deadline, event));
        } else {
            event[8..10].copy_from_slice(&(ERRNO_NOTSUP as u16).to_le_bytes());
            out.extend_from_slice(&event);
        }
    }
    if out.is_empty()
        && let Some(earliest) = clocks.iter().map(|(deadline, _)| *deadline).min()
    {
        let now = now_ns();
        if earliest > now {
            oceans_rt::sleep_ms((earliest - now).div_ceil(1_000_000));
        }
        let now = now_ns().max(earliest);
        for (deadline, event) in &clocks {
            if *deadline <= now {
                out.extend_from_slice(event);
            }
        }
    }
    let n = (out.len() / EVENT) as u32;
    if write(caller, output, &out) && write(caller, events, &n.to_le_bytes()) {
        ERRNO_SUCCESS
    } else {
        ERRNO_FAULT
    }
}
