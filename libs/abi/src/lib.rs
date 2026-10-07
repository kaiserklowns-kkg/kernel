//! The Oceans system call ABI, version 9 (ADR-0014 to ADR-0026).
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
/// 3 = ADR-0016 (18–22); 4 = ADR-0017 (23–24); 5 = ADR-0019 (25–26: badges,
/// memory size); 6 = ADR-0020 (27: system information); 7 = ADR-0021
/// (28–34: devices; errors -15 and -16); 8 = ADR-0023 (35–37: bound
/// notifications, timers, clock); 9 = ADR-0026 (38: random); 10 = ADR-0031
/// (39: wall time); 11 = ADR-0032 (40: console input; class selectors for
/// `DEVICE_OPEN`); 12 = ADR-0044 (41: kill); 13 = ADR-0057 (42–44:
/// display); 14 = ADR-0059 (45–46: the keyboard to the desktop); 15 =
/// ADR-0070 (47: reading the log); 16 = ADR-0085 (48:
/// switching off and restarting). Versions only add; existing numbers keep
/// their meaning.
pub const ABI_VERSION: u64 = 16;

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
    /// `(server, message: *mut MessageDesc) -> (kind, badge)` — needs
    /// `RECEIVE`. `kind` [`EVENT_CALL`](super::EVENT_CALL): a call through the client end with
    /// `badge` (0 = unbadged), to be answered with `IPC_REPLY_MSG`.
    /// [`EVENT_CLOSED`](super::EVENT_CLOSED) (ABI 5): the badged client end `badge` was closed;
    /// the descriptor is not written and nothing is to be answered.
    pub const IPC_RECEIVE_MSG: u64 = 11;
    /// `(reply: *const MessageDesc) -> 0` — answers the pending call.
    pub const IPC_REPLY_MSG: u64 = 12;
    /// `(size) -> handle` — a zero-filled memory object (rounded to pages).
    pub const MEMORY_CREATE: u64 = 13;
    /// `(handle, addr, prot) -> addr` — maps the whole object at `addr`
    /// (page-aligned; 0 = kernel chooses). Needs `MAP`, plus `READ`,
    /// `WRITE`, `EXECUTE` for the requested [`prot`](super::prot) bits.
    pub const MEMORY_MAP: u64 = 14;
    /// `(addr) -> 0` — removes the mapping that starts at `addr`.
    pub const MEMORY_UNMAP: u64 = 15;
    /// `(image, image_len, handles: *const u64, handles_len, arg, name) ->
    /// process_handle` — starts a process from the ELF image held in the
    /// memory object `image` (needs `READ`), moving `handles` (each needs
    /// `TRANSFER`) to it as its initial capabilities. `name` (ABI 3): 0, or
    /// a pointer to a [`PROCESS_NAME_MAX`](super::PROCESS_NAME_MAX)-byte buffer holding a UTF-8 name,
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

    // ABI 5

    /// `(server, badge) -> client_handle` — a new client end of the
    /// endpoint carrying `badge` (non-zero). Needs `MANAGE` on the server
    /// end. Its calls report the badge; closing it sends the server an
    /// [`EVENT_CLOSED`](super::EVENT_CLOSED) event.
    pub const ENDPOINT_MINT: u64 = 25;
    /// `(memory) -> size` — the size in bytes of a memory object (a whole
    /// number of pages). Needs any right on it.
    pub const MEMORY_SIZE: u64 = 26;

    // ABI 6

    /// `(sysinfo, kind, ptr, capacity) -> len` — writes the [`sysinfo`](super::sysinfo)
    /// record(s) of `kind` to `ptr`. Needs `READ` on a system-information
    /// capability. `TooLarge` if `capacity` is too small.
    pub const SYSTEM_INFO: u64 = 27;

    // ABI 7

    /// `(bus, ptr, capacity) -> len` — writes a [`device::DeviceRecord`](super::device::DeviceRecord)
    /// per PCI function to `ptr`. Needs `READ` on the device bus.
    /// `TooLarge` if `capacity` is too small.
    pub const DEVICE_LIST: u64 = 28;
    /// `(bus, vendor << 16 | device_id, index) -> device` — opens the
    /// `index`th function with that ID, exclusively. Needs `MANAGE` on the
    /// device bus. `NotFound` if there is none, `Busy` if it is open.
    pub const DEVICE_OPEN: u64 = 29;
    /// `(device, offset, width) -> value` — reads configuration space
    /// (`width` 1, 2 or 4, aligned). Needs `READ`.
    pub const DEVICE_CONFIG_READ: u64 = 30;
    /// `(device) -> 0` — turns on memory decoding and bus mastering (DMA),
    /// with legacy interrupts off. Needs `MANAGE`.
    pub const DEVICE_ENABLE: u64 = 31;
    /// `(device, bar) -> (memory, size)` — a memory object for memory BAR
    /// `bar`, to map uncached (never executable). Pages holding the MSI-X
    /// table are left out: the kernel programs interrupts. Needs `MANAGE`.
    pub const DEVICE_BAR: u64 = 32;
    /// `(device, size) -> (memory, device_address)` — physically
    /// contiguous memory the device may access, and the address the device
    /// uses for it. Kept alive until DMA has been switched off when the
    /// device closes. Needs `MANAGE`.
    pub const DEVICE_DMA_CREATE: u64 = 33;
    /// `(device, entry, notification, bits) -> 0` — delivers MSI-X vector
    /// `entry` as `bits` on the notification (replacing an earlier binding).
    /// Needs `MANAGE` on the device and `SIGNAL` on the notification.
    pub const DEVICE_IRQ: u64 = 34;

    // ABI 8

    /// `(server, notification) -> 0` — binds the notification to the
    /// endpoint: `IPC_RECEIVE_MSG` also returns when it is signalled, with
    /// kind [`EVENT_NOTIFICATION`](super::EVENT_NOTIFICATION) and the bits
    /// (cleared). Needs `RECEIVE` on the server end and `WAIT` on the
    /// notification. `Busy` if the notification is bound elsewhere.
    pub const ENDPOINT_BIND: u64 = 35;
    /// `(notification, bits, milliseconds) -> 0` — signals `bits` once the
    /// delay has passed (rounded up to the 10 ms tick), replacing the
    /// notification's pending timer; 0 milliseconds cancels it. Needs
    /// `SIGNAL`.
    pub const TIMER_SET: u64 = 36;
    /// `() -> milliseconds` — monotonic time since boot (10 ms resolution).
    pub const CLOCK: u64 = 37;

    // ABI 9

    /// `(ptr, len) -> len` — fills `len` (at most
    /// [`RANDOM_MAX`](super::RANDOM_MAX)) bytes with cryptographically
    /// secure random bytes. Needs no capability: randomness grants no
    /// authority.
    pub const RANDOM: u64 = 38;

    // ABI 10

    /// `() -> milliseconds` — Unix time (UTC), from the real-time clock
    /// read at boot plus the monotonic clock. `NotFound` if the machine has
    /// no usable real-time clock. Needs no capability.
    pub const TIME: u64 = 39;

    // ABI 11

    /// `(console, ptr, len) -> len` — appends `len` (at most
    /// [`CONSOLE_IO_MAX`](super::CONSOLE_IO_MAX)) bytes to the console's
    /// input, as if typed (keyboard drivers). Needs `MANAGE`.
    pub const CONSOLE_INPUT: u64 = 40;

    // ABI 12

    /// `(process) -> ()` — ends the process (ADR-0044): its thread leaves
    /// any wait and the process exits with
    /// [`EXIT_KILLED`](super::EXIT_KILLED) before running user code again.
    /// Killing an exited process does nothing. Needs `MANAGE`.
    pub const PROCESS_KILL: u64 = 41;

    // ABI 13 (ADR-0057)

    /// `(display, ptr) -> ()` — writes the screen's
    /// [`display::Info`](super::display::Info). Needs `READ`.
    pub const DISPLAY_INFO: u64 = 42;
    /// `(display) -> (memory, size)` — the framebuffer, as device memory;
    /// the console stops drawing until the calling process is gone. `Busy`
    /// if a live process holds it. Needs `MANAGE`.
    pub const DISPLAY_CLAIM: u64 = 43;
    /// `(display, ptr, capacity) -> len` — the console's text: a
    /// [`display::TEXT_HEADER`](super::display::TEXT_HEADER)-byte header,
    /// then the cells. Needs `READ`.
    pub const DISPLAY_TEXT: u64 = 44;

    // ABI 14 (ADR-0059)

    /// `(display, notification, bits) -> ()` — keyboard input goes to the
    /// caller instead of the console: key bytes queue for `DISPLAY_KEYS`,
    /// and `bits` is signalled on `notification` as they arrive. The
    /// caller must hold the screen (`DISPLAY_CLAIM`; else `Busy`). Serial
    /// input still goes to the console, and so does the caller's own
    /// `CONSOLE_INPUT`. Ends when the caller exits. Needs `MANAGE` on the
    /// display, `SIGNAL` on the notification.
    pub const DISPLAY_KEYBOARD: u64 = 45;
    /// `(display, ptr, capacity) -> count` — takes queued key bytes
    /// without waiting (0: none). Only the keyboard's holder (else
    /// `Busy`). Needs `READ`.
    pub const DISPLAY_KEYS: u64 = 46;

    // ABI 15 (ADR-0070)

    /// `(log, from, ptr, capacity) -> (len, start)` — log text the kernel
    /// keeps (its last [`LOG_RING`](super::LOG_RING) bytes of lines at
    /// level INFO and above, services' lines included), from byte `from`
    /// of everything ever logged, or from the oldest still kept: `start`
    /// is where the copy begins, `start + len` where to continue. At most
    /// [`LOG_READ_MAX`](super::LOG_READ_MAX) per call. Needs `READ` on the
    /// log, which init grants only on request (`grant = log-read`).
    pub const LOG_READ: u64 = 47;

    // ABI 16 (ADR-0085)

    /// `(system, action) -> can` — the machine's power
    /// ([`power`](super::power)). `QUERY` answers what this machine can do
    /// (`CAN_*` bits) and needs `READ` on the system information object.
    /// `OFF` and `RESTART` need `MANAGE` on it, which only init holds;
    /// they do not return (`NotFound` if `OFF` is not possible here,
    /// before anything happens).
    pub const SYSTEM_POWER: u64 = 48;
}

/// The machine's power (ADR-0085): `SYSTEM_POWER` actions, which are also
/// the requests init answers on its `power` endpoint.
pub mod power {
    pub const QUERY: u64 = 0;
    /// Switch off (ACPI S5).
    pub const OFF: u64 = 1;
    pub const RESTART: u64 = 2;
    /// `QUERY` bits.
    pub const CAN_OFF: u64 = 1 << 0;
    pub const CAN_RESTART: u64 = 1 << 1;

    /// init's answer on the `power` endpoint (one byte): the system is
    /// stopping; this machine cannot do that; not a request.
    pub const ACCEPTED: u8 = 0;
    pub const NOT_POSSIBLE: u8 = 1;
    pub const INVALID: u8 = 2;
}

/// The display (ADR-0057).
pub mod display {
    /// Bytes of the `DISPLAY_TEXT` header: columns, rows, cursor column,
    /// cursor row (u16 each), generation (u64).
    pub const TEXT_HEADER: usize = 16;
    /// Largest `DISPLAY_TEXT` buffer.
    pub const MAX_TEXT: usize = 64 * 1024;
    /// Key bytes queued for `DISPLAY_KEYS`; more are dropped.
    pub const MAX_KEYS: usize = 1024;
    /// The byte keyboards send for Ctrl+Tab (ADR-0059): the desktop moves
    /// the keyboard focus to the next window. (ASCII RS; no other key
    /// sends it.)
    pub const KEY_NEXT_WINDOW: u8 = 0x1e;

    /// The bytes keyboards send for keys that move or edit without a
    /// character (ADR-0084), past ASCII so that what reads only text
    /// (the shell, older apps) skips them.
    pub const KEY_UP: u8 = 0x80;
    pub const KEY_DOWN: u8 = 0x81;
    pub const KEY_LEFT: u8 = 0x82;
    pub const KEY_RIGHT: u8 = 0x83;
    pub const KEY_HOME: u8 = 0x84;
    pub const KEY_END: u8 = 0x85;
    /// Delete (forward); Backspace stays DEL (0x7f) or BS (0x08).
    pub const KEY_DELETE: u8 = 0x86;
    pub const KEY_PAGE_UP: u8 = 0x87;
    pub const KEY_PAGE_DOWN: u8 = 0x88;

    /// The screen.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Info {
        pub width: u32,
        pub height: u32,
        /// Bytes per line.
        pub pitch: u32,
        pub bpp: u32,
        pub red_shift: u8,
        pub green_shift: u8,
        pub blue_shift: u8,
        /// The console's text grid.
        pub cols: u32,
        pub rows: u32,
    }

    impl Info {
        pub const SIZE: usize = 28;

        pub fn encode(&self, out: &mut [u8; Self::SIZE]) {
            for (i, value) in [self.width, self.height, self.pitch, self.bpp]
                .into_iter()
                .enumerate()
            {
                out[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
            }
            out[16] = self.red_shift;
            out[17] = self.green_shift;
            out[18] = self.blue_shift;
            out[19] = 0;
            out[20..24].copy_from_slice(&self.cols.to_le_bytes());
            out[24..28].copy_from_slice(&self.rows.to_le_bytes());
        }

        pub fn decode(bytes: &[u8]) -> Option<Self> {
            let bytes = bytes.get(..Self::SIZE)?;
            let word = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
            Some(Self {
                width: word(0),
                height: word(4),
                pitch: word(8),
                bpp: word(12),
                red_shift: bytes[16],
                green_shift: bytes[17],
                blue_shift: bytes[18],
                cols: word(20),
                rows: word(24),
            })
        }
    }
}

/// The exit code of a process ended by `PROCESS_KILL` (ADR-0044). Exit
/// codes from CPU exceptions are `-128 - vector`, so they never collide.
pub const EXIT_KILLED: i64 = -127;

/// Bytes of log the kernel keeps for `LOG_READ` (ADR-0070).
pub const LOG_RING: usize = 64 * 1024;
/// Largest single `LOG_READ`.
pub const LOG_READ_MAX: usize = 16 * 1024;

/// Largest single `RANDOM` request, in bytes.
pub const RANDOM_MAX: usize = 256;

/// `IPC_RECEIVE_MSG` result kinds.
pub const EVENT_CALL: u64 = 0;
pub const EVENT_CLOSED: u64 = 1;
/// ABI 8: the bound notification was signalled; the second result holds the
/// bits. The descriptor is not written and nothing is to be answered.
pub const EVENT_NOTIFICATION: u64 = 2;

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
    /// No such object (e.g. no device with that ID).
    NotFound = -15,
    /// The object is in exclusive use (e.g. a device another driver opened).
    Busy = -16,
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
            -15 => Self::NotFound,
            -16 => Self::Busy,
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
    /// Raised from 16 in ABI 6 (receivers use the count they are given).
    pub const MAX_INITIAL_HANDLES: usize = 32;
}

/// `SYSTEM_INFO` kinds and their records (ADR-0020): fixed-size,
/// little-endian, encoded explicitly so both sides agree byte for byte.
pub mod sysinfo {
    pub const KERNEL: u64 = 0;
    pub const MEMORY: u64 = 1;
    pub const UPTIME: u64 = 2;
    /// A sequence of [`ProcessRecord`]s.
    pub const PROCESSES: u64 = 3;

    fn put(out: &mut [u8], at: usize, value: u64) {
        out[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn get(bytes: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
    }

    fn put_text(out: &mut [u8], text: &str) {
        let len = text.len().min(out.len());
        out[..len].copy_from_slice(&text.as_bytes()[..len]);
        out[len..].fill(0);
    }

    /// Text up to the first NUL (lossy: invalid UTF-8 yields "?").
    pub fn text(bytes: &[u8]) -> &str {
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        core::str::from_utf8(&bytes[..len]).unwrap_or("?")
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KernelInfo {
        pub abi_version: u64,
        pub version: [u8; 16],
        pub arch: [u8; 16],
    }

    impl KernelInfo {
        pub const SIZE: usize = 40;

        pub fn new(abi_version: u64, version: &str, arch: &str) -> Self {
            let mut info = Self {
                abi_version,
                version: [0; 16],
                arch: [0; 16],
            };
            put_text(&mut info.version, version);
            put_text(&mut info.arch, arch);
            info
        }

        pub fn encode(&self, out: &mut [u8; Self::SIZE]) {
            put(out, 0, self.abi_version);
            out[8..24].copy_from_slice(&self.version);
            out[24..40].copy_from_slice(&self.arch);
        }

        pub fn decode(bytes: &[u8]) -> Option<Self> {
            let bytes = bytes.get(..Self::SIZE)?;
            Some(Self {
                abi_version: get(bytes, 0),
                version: bytes[8..24].try_into().ok()?,
                arch: bytes[24..40].try_into().ok()?,
            })
        }
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct MemoryInfo {
        pub page_size: u64,
        /// Frames the kernel manages (usable RAM).
        pub total_frames: u64,
        pub free_frames: u64,
        /// Kernel heap bytes in use (slab objects + page blocks).
        pub kernel_heap: u64,
    }

    impl MemoryInfo {
        pub const SIZE: usize = 32;

        pub fn encode(&self, out: &mut [u8; Self::SIZE]) {
            for (i, value) in [
                self.page_size,
                self.total_frames,
                self.free_frames,
                self.kernel_heap,
            ]
            .into_iter()
            .enumerate()
            {
                put(out, i * 8, value);
            }
        }

        pub fn decode(bytes: &[u8]) -> Option<Self> {
            let bytes = bytes.get(..Self::SIZE)?;
            Some(Self {
                page_size: get(bytes, 0),
                total_frames: get(bytes, 8),
                free_frames: get(bytes, 16),
                kernel_heap: get(bytes, 24),
            })
        }
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct UptimeInfo {
        pub ticks: u64,
        pub hz: u64,
    }

    impl UptimeInfo {
        pub const SIZE: usize = 16;

        pub fn milliseconds(&self) -> u64 {
            (self.ticks * 1000).checked_div(self.hz).unwrap_or(0)
        }

        pub fn encode(&self, out: &mut [u8; Self::SIZE]) {
            put(out, 0, self.ticks);
            put(out, 8, self.hz);
        }

        pub fn decode(bytes: &[u8]) -> Option<Self> {
            let bytes = bytes.get(..Self::SIZE)?;
            Some(Self {
                ticks: get(bytes, 0),
                hz: get(bytes, 8),
            })
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessRecord {
        pub id: u64,
        /// 0 for processes the kernel started.
        pub parent: u64,
        /// `i64::MIN` while running.
        pub exit_code: i64,
        /// Bytes of user memory mapped.
        pub memory: u64,
        pub name: [u8; 32],
    }

    impl ProcessRecord {
        pub const SIZE: usize = 64;
        pub const RUNNING: i64 = i64::MIN;

        pub fn exit(&self) -> Option<i64> {
            (self.exit_code != Self::RUNNING).then_some(self.exit_code)
        }

        pub fn encode(&self, out: &mut [u8; Self::SIZE]) {
            put(out, 0, self.id);
            put(out, 8, self.parent);
            put(out, 16, self.exit_code as u64);
            put(out, 24, self.memory);
            out[32..64].copy_from_slice(&self.name);
        }

        pub fn decode(bytes: &[u8]) -> Option<Self> {
            let bytes = bytes.get(..Self::SIZE)?;
            Some(Self {
                id: get(bytes, 0),
                parent: get(bytes, 8),
                exit_code: get(bytes, 16) as i64,
                memory: get(bytes, 24),
                name: bytes[32..64].try_into().ok()?,
            })
        }

        pub fn set_name(&mut self, name: &str) {
            put_text(&mut self.name, name);
        }
    }
}

/// `DEVICE_LIST` records (ADR-0021).
pub mod device {
    /// One PCI function. Fixed-size and little-endian.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct DeviceRecord {
        pub segment: u16,
        pub bus: u8,
        /// Device number (0–31).
        pub slot: u8,
        pub function: u8,
        pub vendor: u16,
        pub device: u16,
        pub class: u8,
        pub subclass: u8,
        pub prog_if: u8,
        pub revision: u8,
        /// MSI-X vectors (0: none; such devices cannot interrupt yet).
        pub msix_vectors: u16,
        /// Opened by a driver.
        pub open: bool,
    }

    impl DeviceRecord {
        pub const SIZE: usize = 16;

        pub fn encode(&self, out: &mut [u8; Self::SIZE]) {
            out[0..2].copy_from_slice(&self.segment.to_le_bytes());
            out[2] = self.bus;
            out[3] = self.slot;
            out[4] = self.function;
            out[5] = u8::from(self.open);
            out[6..8].copy_from_slice(&self.vendor.to_le_bytes());
            out[8..10].copy_from_slice(&self.device.to_le_bytes());
            out[10] = self.class;
            out[11] = self.subclass;
            out[12] = self.prog_if;
            out[13] = self.revision;
            out[14..16].copy_from_slice(&self.msix_vectors.to_le_bytes());
        }

        pub fn decode(bytes: &[u8]) -> Option<Self> {
            let bytes = bytes.get(..Self::SIZE)?;
            let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
            Some(Self {
                segment: u16_at(0),
                bus: bytes[2],
                slot: bytes[3],
                function: bytes[4],
                open: bytes[5] != 0,
                vendor: u16_at(6),
                device: u16_at(8),
                class: bytes[10],
                subclass: bytes[11],
                prog_if: bytes[12],
                revision: bytes[13],
                msix_vectors: u16_at(14),
            })
        }
    }

    /// The `vendor << 16 | device` selector `DEVICE_OPEN` takes.
    pub const fn selector(vendor: u16, device: u16) -> u64 {
        ((vendor as u64) << 16) | device as u64
    }

    /// Marks a class selector (ABI 11).
    pub const CLASS_SELECTOR: u64 = 1 << 40;

    /// A `DEVICE_OPEN` selector matching any function of this class,
    /// subclass and programming interface (ABI 11), for standard
    /// controller interfaces such as xHCI (`0c 03 30`).
    pub const fn class_selector(class: u8, subclass: u8, prog_if: u8) -> u64 {
        CLASS_SELECTOR | (class as u64) << 16 | (subclass as u64) << 8 | prog_if as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysinfo_records_round_trip() {
        use sysinfo::*;
        let kernel = KernelInfo::new(6, "0.1.0", "x86_64");
        let mut bytes = [0u8; KernelInfo::SIZE];
        kernel.encode(&mut bytes);
        let back = KernelInfo::decode(&bytes).unwrap();
        assert_eq!(back, kernel);
        assert_eq!((text(&back.version), text(&back.arch)), ("0.1.0", "x86_64"));

        let memory = MemoryInfo {
            page_size: 4096,
            total_frames: 9,
            free_frames: 4,
            kernel_heap: 77,
        };
        let mut bytes = [0u8; MemoryInfo::SIZE];
        memory.encode(&mut bytes);
        assert_eq!(MemoryInfo::decode(&bytes), Some(memory));

        let uptime = UptimeInfo {
            ticks: 250,
            hz: 100,
        };
        let mut bytes = [0u8; UptimeInfo::SIZE];
        uptime.encode(&mut bytes);
        assert_eq!(UptimeInfo::decode(&bytes).unwrap().milliseconds(), 2500);

        let mut process = ProcessRecord {
            id: 3,
            parent: 1,
            exit_code: -142,
            memory: 8192,
            name: [0; 32],
        };
        process.set_name("a-very-long-process-name-that-is-cut-off");
        let mut bytes = [0u8; ProcessRecord::SIZE];
        process.encode(&mut bytes);
        let back = ProcessRecord::decode(&bytes).unwrap();
        assert_eq!(back, process);
        assert_eq!(back.exit(), Some(-142));
        assert_eq!(text(&back.name).len(), 32, "names are cut at 32 bytes");
        assert_eq!(ProcessRecord::decode(&bytes[..10]), None, "short input");
    }

    #[test]
    fn device_records_round_trip() {
        use device::*;
        let record = DeviceRecord {
            segment: 1,
            bus: 2,
            slot: 31,
            function: 7,
            vendor: 0x1af4,
            device: 0x1042,
            class: 1,
            subclass: 0,
            prog_if: 0,
            revision: 1,
            msix_vectors: 2,
            open: true,
        };
        let mut bytes = [0u8; DeviceRecord::SIZE];
        record.encode(&mut bytes);
        assert_eq!(DeviceRecord::decode(&bytes), Some(record));
        assert_eq!(DeviceRecord::decode(&bytes[..15]), None);
        assert_eq!(selector(0x1af4, 0x1042), 0x1af4_1042);
        assert_eq!(class_selector(0x0c, 0x03, 0x30), (1 << 40) | 0x0c0330);
        // A vendor selector never has the class bit.
        assert_eq!(selector(0xffff, 0xffff) & CLASS_SELECTOR, 0);
    }

    #[test]
    fn error_codes_round_trip_and_are_negative() {
        for code in -16..=-1 {
            let error = Error::from_code(code).expect("defined");
            assert_eq!(error.code(), code);
            assert!(error.code() < 0);
        }
        assert_eq!(Error::from_code(0), None);
        assert_eq!(Error::from_code(-17), None);
    }
}
