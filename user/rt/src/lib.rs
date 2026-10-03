//! Minimal Oceans userspace runtime (ABI version 9).
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

/// What a `*_msg` call or receive delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received {
    pub label: u64,
    pub data_len: usize,
    pub handles_len: usize,
    /// Receive only: the badge of the client end the call came through
    /// (0 = unbadged), or of the end that was closed.
    pub badge: u64,
    /// Receive only (ABI 5): not a call but the close of badged end
    /// `badge`; nothing was received and nothing is to be answered.
    pub closed: bool,
    /// Receive only (ABI 8): not a call but these bits of the bound
    /// notification (non-zero); nothing is to be answered.
    pub signals: u64,
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
        badge: 0,
        closed: false,
        signals: 0,
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

/// Waits for a call on `server`, receiving its data and handles, or for
/// the close of a badged client end (`Received::closed`).
pub fn ipc_receive_msg(
    server: Handle,
    data: &mut [u8],
    handles: &mut [Handle],
) -> Result<Received, Error> {
    let mut desc = receive_desc(data, handles);
    let (kind, badge) = call(
        nr::IPC_RECEIVE_MSG,
        [server.0, &raw mut desc as u64, 0, 0, 0, 0],
    )?;
    if kind == oceans_abi::EVENT_CLOSED || kind == oceans_abi::EVENT_NOTIFICATION {
        let notification = kind == oceans_abi::EVENT_NOTIFICATION;
        return Ok(Received {
            label: 0,
            data_len: 0,
            handles_len: 0,
            badge: if notification { 0 } else { badge },
            closed: !notification,
            // `badge` carries the bits for a notification event.
            signals: if notification { badge } else { 0 },
        });
    }
    Ok(Received {
        badge,
        ..received(&desc)
    })
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

// ---- ABI 4 -----------------------------------------------------------------

/// Blocks until console input is available; returns how many raw bytes
/// were read into `buffer` (at most `oceans_abi::CONSOLE_IO_MAX`).
pub fn console_read(console: Handle, buffer: &mut [u8]) -> Result<usize, Error> {
    let len = buffer.len().min(oceans_abi::CONSOLE_IO_MAX);
    let args = [console.0, buffer.as_mut_ptr() as u64, len as u64, 0, 0, 0];
    call(nr::CONSOLE_READ, args).map(|(count, _)| count as usize)
}

/// Writes raw bytes to the console.
pub fn console_write(console: Handle, bytes: &[u8]) -> Result<(), Error> {
    for chunk in bytes.chunks(oceans_abi::CONSOLE_IO_MAX) {
        let args = [
            console.0,
            chunk.as_ptr() as u64,
            chunk.len() as u64,
            0,
            0,
            0,
        ];
        call(nr::CONSOLE_WRITE, args)?;
    }
    Ok(())
}

// ---- ABI 5 -----------------------------------------------------------------

/// A new client end of the endpoint behind `server` carrying `badge`
/// (non-zero; needs `MANAGE` on the server end).
pub fn endpoint_mint(server: Handle, badge: u64) -> Result<Handle, Error> {
    call(nr::ENDPOINT_MINT, [server.0, badge, 0, 0, 0, 0]).map(|(h, _)| Handle(h))
}

/// Size in bytes of a memory object.
pub fn memory_size(memory: Handle) -> Result<u64, Error> {
    call(nr::MEMORY_SIZE, [memory.0, 0, 0, 0, 0, 0]).map(|(size, _)| size)
}

/// The process heap (`alloc` feature): `oceans-heap` slab caches and page
/// blocks (the same allocator as the kernel's, ADR-0010), backed by memory
/// objects mapped at block-aligned addresses in a dedicated region.
#[cfg(feature = "alloc")]
mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::{self, NonNull};

    use oceans_heap::{Heap, PAGE_SIZE, PageSource};

    use super::{Handle, close, memory_create, memory_map, memory_unmap, prot};

    /// Heap blocks live here (inside the kernel's user mapping range,
    /// away from where it places kernel-chosen mappings).
    const REGION_START: u64 = 0x0000_4000_0000_0000;
    const REGION_END: u64 = 0x0000_5000_0000_0000;
    /// Live page blocks tracked at once (slabs are cached, so few).
    const MAX_BLOCKS: usize = 1024;

    struct Block {
        address: u64,
        memory: Handle,
    }

    struct MemoryObjects {
        next: u64,
        blocks: [Option<Block>; MAX_BLOCKS],
    }

    // SAFETY: blocks are fresh memory objects mapped read-write at an
    // address aligned to their size, used by nothing else until freed.
    unsafe impl PageSource for MemoryObjects {
        fn allocate(&mut self, order: u8) -> Option<NonNull<u8>> {
            let size = (PAGE_SIZE as u64) << order;
            let address = self.next.next_multiple_of(size);
            let end = address.checked_add(size)?;
            if end > REGION_END {
                return None;
            }
            let slot = self.blocks.iter().position(Option::is_none)?;
            let memory = memory_create(size).ok()?;
            if memory_map(memory, address, prot::READ | prot::WRITE).is_err() {
                let _ = close(memory);
                return None;
            }
            self.next = end;
            self.blocks[slot] = Some(Block { address, memory });
            NonNull::new(address as *mut u8)
        }

        unsafe fn free(&mut self, block: NonNull<u8>, _order: u8) {
            let address = block.as_ptr() as u64;
            let slot = self
                .blocks
                .iter()
                .position(|b| b.as_ref().is_some_and(|b| b.address == address));
            if let Some(Block { address, memory }) = slot.and_then(|i| self.blocks[i].take()) {
                let _ = memory_unmap(address as *mut u8);
                let _ = close(memory);
            }
        }
    }

    /// Processes are single-threaded (ADR-0014), so no locking is needed.
    struct ProcessHeap(UnsafeCell<Heap<MemoryObjects>>);

    // SAFETY: only one thread per process exists; there is no concurrent
    // access to the heap.
    unsafe impl Sync for ProcessHeap {}

    #[global_allocator]
    static HEAP: ProcessHeap = ProcessHeap(UnsafeCell::new(Heap::new(MemoryObjects {
        next: REGION_START,
        blocks: [const { None }; MAX_BLOCKS],
    })));

    // SAFETY: `oceans-heap` returns blocks that fit and are aligned for the
    // layout, never handed out twice; access is single-threaded.
    unsafe impl GlobalAlloc for ProcessHeap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: single-threaded (see `Sync` impl).
            let heap = unsafe { &mut *self.0.get() };
            heap.allocate(layout)
                .map_or(ptr::null_mut(), NonNull::as_ptr)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: single-threaded; `ptr` came from `alloc` with `layout`.
            unsafe {
                if let Some(ptr) = NonNull::new(ptr) {
                    (*self.0.get()).deallocate(ptr, layout);
                }
            }
        }
    }
}

// ---- ABI 6 and program conventions -----------------------------------------

/// Copies the `kind` record(s) of `oceans_abi::sysinfo` into `buffer`;
/// returns the length (needs `READ` on a system-information capability).
pub fn system_info(sysinfo: Handle, kind: u64, buffer: &mut [u8]) -> Result<usize, Error> {
    let args = [
        sysinfo.0,
        kind,
        buffer.as_mut_ptr() as u64,
        buffer.len() as u64,
        0,
        0,
    ];
    call(nr::SYSTEM_INFO, args).map(|(len, _)| len as usize)
}

// ---- ABI 7: devices (ADR-0021) ---------------------------------------------

/// Copies a `oceans_abi::device::DeviceRecord` per PCI function into
/// `buffer`; returns the length (needs `READ` on the device bus).
pub fn device_list(bus: Handle, buffer: &mut [u8]) -> Result<usize, Error> {
    let args = [
        bus.0,
        buffer.as_mut_ptr() as u64,
        buffer.len() as u64,
        0,
        0,
        0,
    ];
    call(nr::DEVICE_LIST, args).map(|(len, _)| len as usize)
}

/// Opens the `index`th PCI function with this vendor and device ID,
/// exclusively (needs `MANAGE` on the device bus).
pub fn device_open(bus: Handle, vendor: u16, device: u16, index: u64) -> Result<Handle, Error> {
    let selector = oceans_abi::device::selector(vendor, device);
    call(nr::DEVICE_OPEN, [bus.0, selector, index, 0, 0, 0]).map(|(h, _)| Handle(h))
}

/// Reads `width` (1, 2 or 4) bytes of configuration space at `offset`.
pub fn device_config_read(device: Handle, offset: u16, width: u8) -> Result<u32, Error> {
    call(
        nr::DEVICE_CONFIG_READ,
        [device.0, u64::from(offset), u64::from(width), 0, 0, 0],
    )
    .map(|(value, _)| value as u32)
}

/// Turns on memory decoding and bus mastering.
pub fn device_enable(device: Handle) -> Result<(), Error> {
    call(nr::DEVICE_ENABLE, [device.0, 0, 0, 0, 0, 0]).map(drop)
}

/// A memory object for memory BAR `bar` and its size.
pub fn device_bar(device: Handle, bar: u8) -> Result<(Handle, u64), Error> {
    call(nr::DEVICE_BAR, [device.0, u64::from(bar), 0, 0, 0, 0]).map(|(h, size)| (Handle(h), size))
}

/// Contiguous DMA memory of at least `size` bytes and the address the
/// device uses for it.
pub fn device_dma_create(device: Handle, size: u64) -> Result<(Handle, u64), Error> {
    call(nr::DEVICE_DMA_CREATE, [device.0, size, 0, 0, 0, 0])
        .map(|(h, address)| (Handle(h), address))
}

/// Delivers MSI-X vector `entry` as `bits` on `notification`.
pub fn device_irq(
    device: Handle,
    entry: u16,
    notification: Handle,
    bits: u64,
) -> Result<(), Error> {
    call(
        nr::DEVICE_IRQ,
        [device.0, u64::from(entry), notification.0, bits, 0, 0],
    )
    .map(drop)
}

// ---- ABI 8: events and time (ADR-0023) --------------------------------------

/// Binds `notification` to the endpoint behind `server`: `ipc_receive_msg`
/// then also returns when it is signalled (`Received::signals`).
pub fn endpoint_bind(server: Handle, notification: Handle) -> Result<(), Error> {
    call(nr::ENDPOINT_BIND, [server.0, notification.0, 0, 0, 0, 0]).map(drop)
}

/// Signals `bits` on `notification` after `ms` milliseconds, replacing its
/// pending timer; `ms` 0 cancels.
pub fn timer_set(notification: Handle, bits: u64, ms: u64) -> Result<(), Error> {
    call(nr::TIMER_SET, [notification.0, bits, ms, 0, 0, 0]).map(drop)
}

/// Fills `out` with cryptographically secure random bytes (ABI 9).
pub fn random(out: &mut [u8]) -> Result<(), Error> {
    for chunk in out.chunks_mut(oceans_abi::RANDOM_MAX) {
        call(
            nr::RANDOM,
            [chunk.as_mut_ptr() as u64, chunk.len() as u64, 0, 0, 0, 0],
        )?;
    }
    Ok(())
}

/// A random 64-bit value (0 if the kernel cannot provide one).
pub fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    random(&mut bytes).map_or(0, |()| u64::from_le_bytes(bytes))
}

/// Milliseconds since boot (monotonic, 10 ms resolution).
pub fn clock_ms() -> u64 {
    call(nr::CLOCK, [0; 6]).map_or(0, |(ms, _)| ms)
}

/// Unix time in milliseconds (UTC), if the machine's clock is known.
pub fn unix_time_ms() -> Option<u64> {
    call(nr::TIME, [0; 6]).ok().map(|(ms, _)| ms)
}

/// Maps a text memory object read-only for the rest of the process's life
/// and returns its contents up to the first NUL (objects are zero-padded).
pub fn map_text(memory: Handle) -> Option<&'static str> {
    let size = usize::try_from(memory_size(memory).ok()?).ok()?;
    let base = memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: the whole object (`size` bytes) is mapped readable at `base`
    // and stays mapped for the process's lifetime.
    let bytes = unsafe { core::slice::from_raw_parts(base, size) };
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..len]).ok()
}

/// A new read-only memory object holding `text` (to hand to another
/// process: a handle directory, program arguments, …).
pub fn publish_text(text: &[u8]) -> Result<Handle, Error> {
    let memory = memory_create(text.len().max(1) as u64)?;
    let result = (|| {
        let page = memory_map(memory, 0, prot::READ | prot::WRITE)?;
        // SAFETY: just mapped writable, at least `text.len()` bytes.
        unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), page, text.len()) };
        memory_unmap(page)?;
        duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER)
    })();
    let _ = close(memory);
    result
}

/// The handle directory a process receives as its last handle (from init
/// or the shell): lines `<index> <kind> <name>` describing every handle.
pub struct Directory {
    text: &'static str,
    handles: &'static [Handle],
}

impl Directory {
    pub fn from_start(start: &Start) -> Option<Self> {
        let text = map_text(*start.handles.last()?)?;
        Some(Self {
            text,
            handles: start.handles,
        })
    }

    pub fn lines(&self) -> core::str::Lines<'static> {
        self.text.lines()
    }

    fn entries(&self) -> impl Iterator<Item = (Handle, &'static str, &'static str)> + '_ {
        self.text.lines().filter_map(|line| {
            let mut words = line.split_whitespace();
            let index: usize = words.next()?.parse().ok()?;
            let kind = words.next()?;
            let name = words.next()?;
            Some((*self.handles.get(index)?, kind, name))
        })
    }

    /// The stored name of entry `kind`/`name` (a `'static` copy of `name`).
    pub fn name(&self, kind: &str, name: &str) -> Option<&'static str> {
        self.entries()
            .find(|&(_, k, n)| k == kind && n == name)
            .map(|(_, _, n)| n)
    }

    /// The handle of `kind` called `name`.
    pub fn find(&self, kind: &str, name: &str) -> Option<Handle> {
        self.entries()
            .find(|&(_, k, n)| k == kind && n == name)
            .map(|(h, ..)| h)
    }

    /// The first handle of `kind`.
    pub fn find_kind(&self, kind: &str) -> Option<Handle> {
        self.entries().find(|&(_, k, _)| k == kind).map(|(h, ..)| h)
    }

    /// The process's arguments (space-separated words), if any.
    pub fn args(&self) -> &'static str {
        self.find_kind("args").and_then(map_text).unwrap_or("")
    }
}

/// Console output as `fmt::Write`, translating `\n` to `\r\n`.
///
/// Line-buffered: a line goes to the console in one write (when it ends,
/// when the buffer fills, or when the `Out` is dropped), so kernel log
/// lines cannot land in the middle of it.
pub struct Out {
    handle: Handle,
    line: [u8; 256],
    len: usize,
}

impl Out {
    pub const fn new(console: Handle) -> Self {
        Self {
            handle: console,
            line: [0; 256],
            len: 0,
        }
    }

    /// The console capability.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Writes what is buffered.
    pub fn flush(&mut self) -> Result<(), Error> {
        let result = console_write(self.handle, &self.line[..self.len]);
        self.len = 0;
        result
    }

    /// Buffers raw bytes (no translation), flushing full lines' worth.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        for &byte in bytes {
            if self.len == self.line.len() {
                self.flush()?;
            }
            self.line[self.len] = byte;
            self.len += 1;
        }
        Ok(())
    }
}

impl fmt::Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                self.write_bytes(b"\r\n").map_err(|_| fmt::Error)?;
                self.flush().map_err(|_| fmt::Error)?;
            }
            self.write_bytes(part.as_bytes()).map_err(|_| fmt::Error)?;
        }
        Ok(())
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Declares the capabilities a program requests (master spec §41), as an
/// `.oceans.manifest` ELF section the shell reads before running it, e.g.
/// `oceans_rt::manifest!(b"grant out\ngrant sysinfo\n");`. Only low-risk
/// requests are granted implicitly; others need an explicit `run`.
#[macro_export]
macro_rules! manifest {
    ($text:literal) => {
        #[used]
        #[unsafe(link_section = ".oceans.manifest")]
        static OCEANS_MANIFEST: [u8; $text.len()] = *$text;
    };
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
    /// Appends as much of `s` as fits (cut at a character boundary); an
    /// error reports that the text was truncated.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut take = s.len().min(N - self.len);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        if take == s.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

/// Exit code of a process that panicked.
pub const PANIC_EXIT_CODE: i64 = -2;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    exit(PANIC_EXIT_CODE)
}
