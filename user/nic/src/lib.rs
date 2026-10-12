//! What Oceans' Ethernet drivers share (ADR-0123): the registers of a
//! memory BAR, waiting on the device, and serving the `netdev` protocol
//! (ADR-0023) to the stack, so that each driver is only its device.
//!
//! A driver implements [`Nic`] (send a frame, deliver received ones,
//! handle the device's interrupt causes) and calls [`serve`], which answers
//! the stack: `INFO`, one session at a time (`OPEN`), `SEND` and `RECV`.
//! Frames are copied between the session's shared buffer and the driver's
//! own DMA buffers: the stack's memory is never given to the device.
//!
//! One thread serves the stack and the device. The device's interrupt (or,
//! without one, a polling tick) is a notification bound to the driver's
//! endpoint, so a receive returns either a request or the device's signal.
//! When frames arrive the driver signals the stack's notification, never
//! calling it: neither side can block the other.

#![no_std]

use core::fmt::Write;
use core::ptr;

use oceans_net_proto::Status;
use oceans_net_proto::netdev::{BUFFER_SIZE, MAX_FRAME, RX_AREA, TX_AREA, op};
use oceans_rt::{Buffer, Handle, prot};

pub use oceans_net_proto::Mac;

/// The notification bit of the device's interrupt and of the polling tick.
pub const IRQ: u64 = 1;
/// The polling interval without an interrupt.
pub const POLL_MS: u64 = 10;
/// The smallest frame a driver passes on or sends (an Ethernet header).
pub const MIN_FRAME: usize = 14;

pub const EXIT_BAD_START: i64 = 2;
pub const EXIT_DEVICE: i64 = 3;
pub const EXIT_IPC: i64 = 4;

/// Logs `args` as `name: ...`.
pub fn say(log: Handle, name: &str, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<192>::new();
    let _ = line.write_str(name);
    let _ = line.write_str(": ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// A MAC address as `aa:bb:cc:dd:ee:ff`.
pub struct ShowMac(pub Mac);

impl core::fmt::Display for ShowMac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

/// A device's registers: one memory BAR, mapped uncached for the driver's
/// lifetime (never unmapped).
#[derive(Clone, Copy)]
pub struct Registers {
    base: *mut u8,
    len: usize,
}

impl Registers {
    /// Maps `bar` of `device`, which must span at least `span` bytes.
    pub fn map(device: Handle, bar: u8, span: usize) -> Result<Self, &'static str> {
        let (memory, size) =
            oceans_rt::device_bar(device, bar).map_err(|_| "cannot get the register BAR")?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
            .map_err(|_| "cannot map the register BAR");
        let _ = oceans_rt::close(memory);
        let base = base?;
        let len = usize::try_from(size).unwrap_or(0);
        if len < span {
            return Err("register BAR too small for this controller");
        }
        Ok(Self { base, len })
    }

    fn at<T>(self, offset: usize) -> *mut T {
        assert!(offset.is_multiple_of(size_of::<T>()) && offset + size_of::<T>() <= self.len);
        // SAFETY: in bounds and aligned (checked); `base..base + len` is
        // the BAR, mapped while the driver lives.
        unsafe { self.base.add(offset).cast::<T>() }
    }

    pub fn read(self, offset: usize) -> u32 {
        // SAFETY: `at` checks bounds and alignment; device registers are
        // read with volatile accesses.
        unsafe { ptr::read_volatile(self.at::<u32>(offset)) }
    }

    pub fn write(self, offset: usize, value: u32) {
        // SAFETY: as for `read`.
        unsafe { ptr::write_volatile(self.at::<u32>(offset), value) }
    }

    pub fn read16(self, offset: usize) -> u16 {
        // SAFETY: as for `read`.
        unsafe { ptr::read_volatile(self.at::<u16>(offset)) }
    }

    pub fn write16(self, offset: usize, value: u16) {
        // SAFETY: as for `read`.
        unsafe { ptr::write_volatile(self.at::<u16>(offset), value) }
    }

    pub fn read8(self, offset: usize) -> u8 {
        // SAFETY: as for `read`.
        unsafe { ptr::read_volatile(self.at::<u8>(offset)) }
    }

    pub fn write8(self, offset: usize, value: u8) {
        // SAFETY: as for `read`.
        unsafe { ptr::write_volatile(self.at::<u8>(offset), value) }
    }

    /// Sets the bits of `set` and clears those of `clear` (distinct bits).
    pub fn update(self, offset: usize, set: u32, clear: u32) {
        self.write(offset, (self.read(offset) | set) & !clear);
    }

    /// Gives the field `mask` the value `value` (already in place).
    pub fn replace(self, offset: usize, mask: u32, value: u32) {
        self.write(offset, (self.read(offset) & !mask) | (value & mask));
    }
}

/// Waits up to `timeout_ms` for `done`: briefly spinning (most registers
/// finish in microseconds), then sleeping a millisecond at a time.
pub fn wait_for(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..1000 {
        if done() {
            return true;
        }
        core::hint::spin_loop();
    }
    let deadline = oceans_rt::clock_ms() + timeout_ms;
    loop {
        if done() {
            return true;
        }
        if oceans_rt::clock_ms() > deadline {
            return done();
        }
        oceans_rt::sleep_ms(1);
    }
}

/// Where received frames go: the session's receive area, as `[len u16]
/// [frame]` records each padded to an even length.
pub struct Frames {
    buffer: *mut u8,
    at: usize,
    count: u16,
}

impl Frames {
    /// Room for a frame of `len` bytes, filled by `fill`; `false` (and
    /// nothing written) when the area is full: the frame stays with the
    /// device until the next `RECV`.
    pub fn push(&mut self, len: usize, fill: impl FnOnce(&mut [u8])) -> bool {
        let end = self.at + 2 + len.next_multiple_of(2);
        if end > RX_AREA.end || len > MAX_FRAME {
            return false;
        }
        // SAFETY: `at..at + 2 + len` lies in the receive area of the
        // session buffer (`end` checked), mapped read-write while the
        // session lives.
        let record = unsafe { core::slice::from_raw_parts_mut(self.buffer.add(self.at), 2 + len) };
        record[..2].copy_from_slice(&(len as u16).to_le_bytes());
        fill(&mut record[2..]);
        self.at = end;
        self.count += 1;
        true
    }

    /// Whether a frame of `len` bytes still fits.
    pub fn fits(&self, len: usize) -> bool {
        self.at + 2 + len.next_multiple_of(2) <= RX_AREA.end
    }
}

/// One controller, as [`serve`] drives it.
pub trait Nic {
    /// The station address and the MTU.
    fn info(&self) -> (Mac, u16);
    /// Handles the device's interrupt causes, reclaims sent frames and
    /// logs link changes. Fails only if the device is gone.
    fn service(&mut self) -> Result<(), &'static str>;
    /// Whether a received frame waits for the stack.
    fn frames_waiting(&self) -> bool;
    /// Queues `frame` (`MIN_FRAME..=MAX_FRAME` bytes, checked); a full
    /// ring drops it with `NoBuffers`, as a NIC does.
    fn transmit(&mut self, frame: &[u8]) -> Status;
    /// Moves waiting frames into `frames` and gives their buffers back to
    /// the device.
    fn receive(&mut self, frames: &mut Frames);
}

struct Session {
    badge: u64,
    buffer: *mut u8,
    notification: Handle,
    bits: u64,
}

/// How [`serve`] reaches the device: its notification is bound to the
/// endpoint already; without an interrupt, it is ticked every
/// [`POLL_MS`].
pub struct Events {
    pub handle: Handle,
    pub interrupts: bool,
}

/// Serves the stack on `server` until the device is gone or IPC fails.
pub fn serve(nic: &mut impl Nic, name: &str, log: Handle, server: Handle, events: Events) -> i64 {
    let mut data = [0u8; 64];
    let mut handles = [Handle(0); 4];
    let mut next_badge = 1;
    let mut session: Option<Session> = None;
    loop {
        if !events.interrupts {
            let _ = oceans_rt::timer_set(events.handle, IRQ, POLL_MS);
        }
        let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
            Ok(got) => got,
            Err(error) => {
                say(log, name, format_args!("receive failed: {error:?}"));
                return EXIT_IPC;
            }
        };
        if got.signals != 0 {
            if let Err(problem) = nic.service() {
                say(log, name, format_args!("{problem}"));
                return EXIT_DEVICE;
            }
            signal(nic, &session);
            continue;
        }
        if got.closed {
            if session.as_ref().is_some_and(|s| s.badge == got.badge)
                && let Some(old) = session.take()
            {
                let _ = oceans_rt::memory_unmap(old.buffer);
                let _ = oceans_rt::close(old.notification);
            }
            continue;
        }
        let received = &handles[..got.handles_len];
        let in_session = got.badge != 0 && session.as_ref().is_some_and(|s| s.badge == got.badge);
        let mut reply = [0u8; 8];
        let mut reply_len = 0;
        let mut reply_handle = None;
        let mut kept = false;
        let status = match got.label {
            op::INFO => {
                let (mac, mtu) = nic.info();
                reply[..6].copy_from_slice(&mac);
                reply[6..8].copy_from_slice(&mtu.to_le_bytes());
                reply_len = 8;
                Status::Ok
            }
            op::OPEN if got.badge == 0 && received.len() == 2 && got.data_len == 8 => {
                let bits = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
                match open(&session, server, received, bits, next_badge) {
                    Ok((handle, opened)) => {
                        next_badge += 1;
                        kept = true;
                        reply_handle = Some(handle);
                        session = Some(opened);
                        // Frames that arrived before the stack connected.
                        signal(nic, &session);
                        Status::Ok
                    }
                    Err(status) => status,
                }
            }
            op::SEND if in_session => match &session {
                Some(s) => send(nic, s.buffer, &data[..got.data_len]),
                None => Status::BadRequest,
            },
            op::RECV if in_session => {
                let count = match &session {
                    Some(s) => {
                        let mut frames = Frames {
                            buffer: s.buffer,
                            at: RX_AREA.start,
                            count: 0,
                        };
                        nic.receive(&mut frames);
                        frames.count
                    }
                    None => 0,
                };
                reply[..2].copy_from_slice(&count.to_le_bytes());
                reply_len = 2;
                Status::Ok
            }
            _ => Status::BadRequest,
        };
        if !kept {
            for &handle in received {
                let _ = oceans_rt::close(handle);
            }
        }
        let reply_handles: &[Handle] = match &reply_handle {
            Some(handle) => core::slice::from_ref(handle),
            None => &[],
        };
        if oceans_rt::ipc_reply_msg(status as u64, &reply[..reply_len], reply_handles).is_err()
            && let Some(handle) = reply_handle
        {
            let _ = oceans_rt::close(handle);
        }
    }
}

/// Tells the stack that frames wait, if they do.
fn signal(nic: &impl Nic, session: &Option<Session>) {
    if let Some(session) = session
        && nic.frames_waiting()
    {
        let _ = oceans_rt::notification_signal(session.notification, session.bits);
    }
}

fn open(
    session: &Option<Session>,
    server: Handle,
    received: &[Handle],
    bits: u64,
    badge: u64,
) -> Result<(Handle, Session), Status> {
    if session.is_some() {
        return Err(Status::NoBuffers);
    }
    let (memory, notification) = (received[0], received[1]);
    let size = oceans_rt::memory_size(memory).map_err(|_| Status::BadRequest)? as usize;
    if size < BUFFER_SIZE || bits == 0 {
        return Err(Status::BadRequest);
    }
    let buffer = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
        .map_err(|_| Status::BadRequest)?;
    let handle = match oceans_rt::endpoint_mint(server, badge) {
        Ok(handle) => handle,
        Err(_) => {
            let _ = oceans_rt::memory_unmap(buffer);
            return Err(Status::NoBuffers);
        }
    };
    // The mapping keeps the memory; the notification handle is kept.
    let _ = oceans_rt::close(memory);
    Ok((
        handle,
        Session {
            badge,
            buffer,
            notification,
            bits,
        },
    ))
}

/// Sends the frame at `[offset u32][len u16]` of the transmit area.
fn send(nic: &mut impl Nic, buffer: *mut u8, data: &[u8]) -> Status {
    if data.len() != 6 {
        return Status::BadRequest;
    }
    let offset = u32::from_le_bytes(data[..4].try_into().expect("4 bytes")) as usize;
    let len = usize::from(u16::from_le_bytes([data[4], data[5]]));
    let fits = offset
        .checked_add(len)
        .is_some_and(|end| offset >= TX_AREA.start && end <= TX_AREA.end);
    if !fits || !(MIN_FRAME..=MAX_FRAME).contains(&len) {
        return Status::BadRequest;
    }
    // SAFETY: `offset..offset + len` lies in the transmit area of the
    // session buffer (checked), mapped read-write while it lives.
    let frame = unsafe { core::slice::from_raw_parts(buffer.add(offset), len) };
    nic.transmit(frame)
}
