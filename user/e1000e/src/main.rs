//! e1000e: the driver for Intel 82574L Gigabit Ethernet controllers (PCI
//! `8086:10d3`, QEMU's `-device e1000e`, ADR-0041).
//!
//! An ordinary service with exactly what `services.conf` grants: one device
//! capability (`grant = device:8086:10d3`), the endpoint it serves
//! (`provide = netdev`) and a log. It serves the same `netdev` protocol as
//! virtio-net (ADR-0023), so the stack (`net`) runs unchanged on top.
//!
//! - Bring-up: interrupts masked, DMA stopped, the controller reset (the
//!   firmware may have left it running), the NVM checksum checked and the
//!   MAC address read from it, the link set up (autonegotiation restarted
//!   if it is down), legacy descriptor rings programmed, then interrupts
//!   unmasked.
//! - Receive: a ring of 64 descriptors, each with its own 2048-byte DMA
//!   buffer. Completed descriptors keep their frames until the stack asks
//!   (`RECV`); only then are they re-armed, so a slow stack makes the
//!   device drop frames (counted in `MPC`) instead of the driver queueing
//!   without bound.
//! - Transmit: a ring of 32 descriptors and buffers; a frame is copied in
//!   and queued, and its descriptor reclaimed once the device sets `DONE`.
//! - Interrupts: MSI-X vector 0, a notification bound to the driver's
//!   endpoint, so one thread serves the stack and the device; every cause
//!   (receive, transmit, link changes, overruns) is routed to it. Without
//!   MSI-X the driver polls every 10 ms. When frames arrive the driver
//!   signals the stack's notification, never calling it: neither side can
//!   block the other.
//! - Errors: bad frames are dropped and counted; the counts and the
//!   device's own error statistics are logged (at most every 10 s). A
//!   device that stops answering (registers read all ones) ends the driver
//!   with an error, for init's restart policy.
//!
//! The register, descriptor and ring logic is in `oceans-e1000e`
//! (libs/e1000e), tested on the host.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use oceans_e1000e::{
    DESCRIPTOR_SIZE, Link, RxAssembler, RxDescriptor, RxOutcome, RxRing, STATUS_OFFSET,
    TxDescriptor, TxRing, mdic, nvm, reg, rx_status, tx_status,
};
use oceans_net_proto::netdev::{BUFFER_SIZE, MAX_FRAME, RX_AREA, TX_AREA, op};
use oceans_net_proto::{Mac, Status};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const RX_DESCRIPTORS: u16 = 64;
const TX_DESCRIPTORS: u16 = 32;
/// One buffer per descriptor (`RCTL.BSIZE` 2048).
const BUFFER: usize = 2048;
/// Where the rings sit in their (one page of) DMA memory.
const RX_RING: usize = 0;
const TX_RING: usize = RX_DESCRIPTORS as usize * DESCRIPTOR_SIZE;
const RINGS_SIZE: usize = TX_RING + TX_DESCRIPTORS as usize * DESCRIPTOR_SIZE;
/// Notification bits: device interrupt, polling tick.
const IRQ: u64 = 1;
const MSIX_VECTOR: u16 = 0;
/// Polling interval without MSI-X.
const POLL_MS: u64 = 10;
/// Error statistics are read, and changes logged, at most this often.
const STATISTICS_MS: u64 = 10_000;
const MTU: u16 = 1500;

/// Bring-up timeouts (Intel's 82574 initialisation uses the same bounds).
const MASTER_DISABLE_MS: u64 = 800;
const RESET_MS: u64 = 100;
const NVM_MS: u64 = 10;
const PHY_MS: u64 = 10;

const EXIT_BAD_START: i64 = 2;
const EXIT_DEVICE: i64 = 3;
const EXIT_IPC: i64 = 4;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_str("e1000e: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(device), Some(server)) = (
        directory.find("log", "log"),
        directory.find_kind("device"),
        directory.find_kind("provide"),
    ) else {
        return EXIT_BAD_START;
    };
    let mut driver = match Driver::start(log, device, server) {
        Ok(driver) => driver,
        Err(problem) => {
            say(log, format_args!("{problem}"));
            return EXIT_DEVICE;
        }
    };
    driver.serve(server)
}

/// The controller's registers: BAR 0, mapped uncached.
#[derive(Clone, Copy)]
struct Registers {
    base: *mut u8,
    len: usize,
}

impl Registers {
    fn read(self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= self.len);
        // SAFETY: `base..base + len` is BAR 0, mapped for the driver's
        // lifetime (never unmapped); the offset is aligned and in bounds
        // (checked). Device registers are read with volatile accesses.
        unsafe { ptr::read_volatile(self.base.add(offset).cast::<u32>()) }
    }

    fn write(self, offset: usize, value: u32) {
        assert!(offset.is_multiple_of(4) && offset + 4 <= self.len);
        // SAFETY: as for `read`.
        unsafe { ptr::write_volatile(self.base.add(offset).cast::<u32>(), value) }
    }

    fn update(self, offset: usize, set: u32, clear: u32) {
        self.write(offset, (self.read(offset) | set) & !clear);
    }

    /// Posted writes reach the device before a delay starts.
    fn flush(self) {
        let _ = self.read(reg::STATUS);
    }
}

/// Waits up to `timeout_ms` for `done`: briefly spinning (registers such
/// as `EERD` finish in microseconds), then sleeping a millisecond at a
/// time.
fn wait_for(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
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

struct Session {
    badge: u64,
    buffer: *mut u8,
    notification: Handle,
    bits: u64,
}

/// Errors counted since start: the driver's own, and the device's
/// statistics registers (which clear when read).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Counters {
    /// Frames the device flagged, runts and oversized ones.
    bad_frames: u64,
    /// Frames spanning several buffers (impossible as configured).
    fragments: u64,
    /// Frames lost for want of a free descriptor or FIFO space.
    missed: u64,
    overruns: u64,
    crc_errors: u64,
    length_errors: u64,
    /// Frames the device reports it could not send.
    tx_failed: u64,
}

struct Driver {
    log: Handle,
    regs: Registers,
    mac: Mac,
    /// Both descriptor rings.
    rings: Dma,
    rx_buffers: Dma,
    tx_buffers: Dma,
    rx: RxRing,
    tx: TxRing,
    assembler: RxAssembler,
    link: Link,
    interrupts: bool,
    events: Handle,
    session: Option<Session>,
    counters: Counters,
    reported: Counters,
    next_statistics: u64,
}

impl Driver {
    fn start(log: Handle, device: Handle, server: Handle) -> Result<Self, &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let (memory, size) =
            oceans_rt::device_bar(device, 0).map_err(|_| "cannot get the register BAR")?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
            .map_err(|_| "cannot map the register BAR");
        let _ = oceans_rt::close(memory);
        let base = base?;
        let size = usize::try_from(size).unwrap_or(0);
        if size < reg::SPAN {
            return Err("register BAR too small for an 82574");
        }
        let regs = Registers { base, len: size };
        let setup = Self::setup(log, device, server, regs);
        if setup.is_err() {
            // Leave it quiet; closing the device capability at exit then
            // turns off its DMA (ADR-0021).
            regs.write(reg::IMC, reg::int::ALL);
            regs.write(reg::RCTL, 0);
            regs.write(reg::TCTL, 0);
        }
        setup
    }

    fn setup(
        log: Handle,
        device: Handle,
        server: Handle,
        regs: Registers,
    ) -> Result<Self, &'static str> {
        if regs.read(reg::STATUS) == u32::MAX {
            return Err("device does not respond (registers read all ones)");
        }
        reset(log, regs)?;
        initialize_hardware_bits(regs);
        let mac = read_mac(regs)?;
        let (low, high) = oceans_e1000e::receive_address(&mac);
        regs.write(reg::RAL0, low);
        regs.write(reg::RAH0, high);
        for index in 0..reg::MTA_LEN {
            regs.write(reg::MTA + 4 * index, 0);
        }
        // Link: let the MAC follow the PHY's autonegotiated speed and
        // duplex (copper), with nothing forced and nothing in reset.
        regs.update(
            reg::CTRL,
            reg::ctrl::SET_LINK_UP,
            reg::ctrl::FORCE_SPEED
                | reg::ctrl::FORCE_DUPLEX
                | reg::ctrl::LINK_RESET
                | reg::ctrl::PHY_RESET
                | reg::ctrl::INVERT_LOSS_OF_SIGNAL,
        );

        let rings = Dma::new(device, RINGS_SIZE)?;
        let rx_buffers = Dma::new(device, usize::from(RX_DESCRIPTORS) * BUFFER)?;
        let tx_buffers = Dma::new(device, usize::from(TX_DESCRIPTORS) * BUFFER)?;
        let rx = RxRing::new(RX_DESCRIPTORS);
        let tx = TxRing::new(TX_DESCRIPTORS);
        for index in 0..usize::from(RX_DESCRIPTORS) {
            let descriptor = RxDescriptor::posted(rx_buffers.device + (index * BUFFER) as u64);
            rings.copy_in(RX_RING + index * DESCRIPTOR_SIZE, &descriptor.encode());
        }
        rings.copy_in(TX_RING, &[0; TX_DESCRIPTORS as usize * DESCRIPTOR_SIZE]);
        fence(Ordering::SeqCst);

        let events = oceans_rt::notification_create().map_err(|_| "no notification")?;
        oceans_rt::endpoint_bind(server, events).map_err(|_| "cannot bind events")?;
        let interrupts = oceans_rt::device_irq(device, MSIX_VECTOR, events, IRQ).is_ok();
        if interrupts {
            regs.write(reg::IVAR, oceans_e1000e::ivar(MSIX_VECTOR as u8));
            regs.update(reg::CTRL_EXT, reg::ctrl_ext::PBA_SUPPORT, 0);
            // Causes stay in ICR until the driver clears them.
            regs.write(reg::EIAC, 0);
        }

        // Receive: legacy descriptors, 2048-byte buffers, no checksum
        // offload (the stack checks its own).
        let split = |address: u64| (address as u32, (address >> 32) as u32);
        let (low, high) = split(rings.device + RX_RING as u64);
        regs.write(reg::RDBAL, low);
        regs.write(reg::RDBAH, high);
        regs.write(
            reg::RDLEN,
            (usize::from(RX_DESCRIPTORS) * DESCRIPTOR_SIZE) as u32,
        );
        regs.write(reg::RDH, 0);
        regs.write(reg::RDT, 0);
        regs.write(reg::RXCSUM, 0);
        regs.write(reg::RCTL, oceans_e1000e::receive_control());
        regs.write(reg::RDT, u32::from(rx.initial_tail()));

        // Transmit.
        let (low, high) = split(rings.device + TX_RING as u64);
        regs.write(reg::TDBAL, low);
        regs.write(reg::TDBAH, high);
        regs.write(
            reg::TDLEN,
            (usize::from(TX_DESCRIPTORS) * DESCRIPTOR_SIZE) as u32,
        );
        regs.write(reg::TDH, 0);
        regs.write(reg::TDT, 0);
        regs.update(
            reg::TXDCTL,
            reg::txdctl::WRITE_BACK_EACH | reg::txdctl::COUNT_DESCRIPTORS,
            reg::txdctl::WTHRESH_MASK,
        );
        regs.write(reg::TIPG, oceans_e1000e::TRANSMIT_IPG);
        regs.write(reg::TCTL, oceans_e1000e::transmit_control());

        // Interrupts: whatever is pending from bring-up goes, then the
        // causes the driver handles are unmasked.
        let stale = regs.read(reg::ICR);
        regs.write(reg::ICR, stale);
        regs.write(reg::IMS, oceans_e1000e::interrupt_mask(interrupts));

        let mut driver = Self {
            log,
            regs,
            mac,
            rings,
            rx_buffers,
            tx_buffers,
            rx,
            tx,
            assembler: RxAssembler::default(),
            link: Link::Down,
            interrupts,
            events,
            session: None,
            counters: Counters::default(),
            reported: Counters::default(),
            next_statistics: oceans_rt::clock_ms() + STATISTICS_MS,
        };
        let [a, b, c, d, e, f] = mac;
        say(
            log,
            format_args!(
                "MAC {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}, 82574L, {}",
                if interrupts { "MSI-X" } else { "polling" }
            ),
        );
        driver.link = Link::from_status(regs.read(reg::STATUS));
        match driver.link {
            Link::Up { .. } => say(log, format_args!("link {}", driver.link)),
            Link::Down => {
                // The PHY may have been left idle: negotiate afresh. The
                // link-change interrupt reports the result.
                match restart_autonegotiation(regs) {
                    Ok(()) => say(log, format_args!("link down, autonegotiating")),
                    Err(problem) => say(log, format_args!("link down; {problem}")),
                }
            }
        }
        Ok(driver)
    }

    /// Handles the device's interrupt causes and reclaims sent frames.
    /// Fails only if the device is gone.
    fn service(&mut self) -> Result<(), &'static str> {
        let causes = self.regs.read(reg::ICR);
        if causes == u32::MAX && self.regs.read(reg::STATUS) == u32::MAX {
            return Err("device stopped responding (registers read all ones)");
        }
        if causes != 0 {
            // Write-1-to-clear (with MSI-X, reading ICR does not clear
            // it). Frames that arrive after this raise a new interrupt;
            // those before it are in the ring, read below.
            self.regs.write(reg::ICR, causes);
        }
        if causes & reg::int::RX_OVERRUN != 0 {
            self.counters.overruns += 1;
        }
        let link = Link::from_status(self.regs.read(reg::STATUS));
        if link != self.link {
            self.link = link;
            say(self.log, format_args!("link {link}"));
        }
        self.reclaim_transmitted();
        let now = oceans_rt::clock_ms();
        if now >= self.next_statistics {
            self.next_statistics = now + STATISTICS_MS;
            self.report_errors();
        }
        Ok(())
    }

    /// Accumulates the device's error statistics and logs what changed.
    fn report_errors(&mut self) {
        self.counters.missed += u64::from(self.regs.read(reg::MPC));
        self.counters.crc_errors += u64::from(self.regs.read(reg::CRCERRS));
        self.counters.length_errors += u64::from(self.regs.read(reg::RLEC));
        let c = self.counters;
        if c != self.reported {
            self.reported = c;
            say(
                self.log,
                format_args!(
                    "errors so far: receive {} bad, {} fragments, {} missed, {} overruns, \
                     {} CRC, {} length; transmit {} failed",
                    c.bad_frames,
                    c.fragments,
                    c.missed,
                    c.overruns,
                    c.crc_errors,
                    c.length_errors,
                    c.tx_failed
                ),
            );
        }
    }

    /// Reclaims transmit descriptors the device is done with.
    fn reclaim_transmitted(&mut self) {
        while let Some(index) = self.tx.oldest() {
            let at = TX_RING + usize::from(index) * DESCRIPTOR_SIZE;
            let status: u8 = self.rings.read(at + STATUS_OFFSET);
            if status & tx_status::DONE == 0 {
                break;
            }
            if TxDescriptor::failed(status) {
                self.counters.tx_failed += 1;
            }
            self.tx.complete();
        }
    }

    /// Whether a completed receive descriptor waits for the stack.
    fn frames_waiting(&self) -> bool {
        let at = RX_RING + usize::from(self.rx.next()) * DESCRIPTOR_SIZE;
        self.rings.read::<u8>(at + STATUS_OFFSET) & rx_status::DONE != 0
    }

    fn signal_session(&self) {
        if let Some(session) = &self.session
            && self.frames_waiting()
        {
            let _ = oceans_rt::notification_signal(session.notification, session.bits);
        }
    }

    fn serve(&mut self, server: Handle) -> i64 {
        let mut data = [0u8; 64];
        let mut handles = [Handle(0); 4];
        let mut next_badge = 1;
        loop {
            if !self.interrupts {
                let _ = oceans_rt::timer_set(self.events, IRQ, POLL_MS);
            }
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                Err(error) => {
                    say(self.log, format_args!("receive failed: {error:?}"));
                    return EXIT_IPC;
                }
            };
            if got.signals != 0 {
                if let Err(problem) = self.service() {
                    say(self.log, format_args!("{problem}"));
                    return EXIT_DEVICE;
                }
                self.signal_session();
                continue;
            }
            if got.closed {
                if self.session.as_ref().is_some_and(|s| s.badge == got.badge)
                    && let Some(session) = self.session.take()
                {
                    let _ = oceans_rt::memory_unmap(session.buffer);
                    let _ = oceans_rt::close(session.notification);
                }
                continue;
            }
            let received = &handles[..got.handles_len];
            let in_session =
                got.badge != 0 && self.session.as_ref().is_some_and(|s| s.badge == got.badge);
            let mut reply = [0u8; 8];
            let mut reply_len = 0;
            let mut reply_handle = None;
            let mut kept = false;
            let status = match got.label {
                op::INFO => {
                    reply[..6].copy_from_slice(&self.mac);
                    reply[6..8].copy_from_slice(&MTU.to_le_bytes());
                    reply_len = 8;
                    Status::Ok
                }
                op::OPEN if got.badge == 0 && received.len() == 2 && got.data_len == 8 => {
                    let bits = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
                    match self.open(server, received, bits, next_badge) {
                        Ok(handle) => {
                            next_badge += 1;
                            kept = true;
                            reply_handle = Some(handle);
                            Status::Ok
                        }
                        Err(status) => status,
                    }
                }
                op::SEND if in_session => self.send(&data[..got.data_len]),
                op::RECV if in_session => {
                    let count = self.receive();
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

    fn open(
        &mut self,
        server: Handle,
        received: &[Handle],
        bits: u64,
        badge: u64,
    ) -> Result<Handle, Status> {
        if self.session.is_some() {
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
        self.session = Some(Session {
            badge,
            buffer,
            notification,
            bits,
        });
        // Frames that arrived before the stack connected.
        self.signal_session();
        Ok(handle)
    }

    /// Queues the frame at `[offset u32][len u16]` of the transmit area.
    fn send(&mut self, data: &[u8]) -> Status {
        let Some(session) = &self.session else {
            return Status::BadRequest;
        };
        if data.len() != 6 {
            return Status::BadRequest;
        }
        let offset = u32::from_le_bytes(data[..4].try_into().expect("4 bytes")) as usize;
        let len = usize::from(u16::from_le_bytes([data[4], data[5]]));
        let fits = offset
            .checked_add(len)
            .is_some_and(|end| offset >= TX_AREA.start && end <= TX_AREA.end);
        if !fits || !(14..=MAX_FRAME).contains(&len) {
            return Status::BadRequest;
        }
        let buffer = session.buffer;
        self.reclaim_transmitted();
        // A full ring drops the frame, as a NIC does.
        let Some(index) = self.tx.claim() else {
            return Status::NoBuffers;
        };
        let at = usize::from(index) * BUFFER;
        // SAFETY: `offset..offset + len` lies in the transmit area of the
        // session buffer (checked), mapped read-write while it lives.
        let frame = unsafe { core::slice::from_raw_parts(buffer.add(offset), len) };
        self.tx_buffers.copy_in(at, frame);
        let [low, high] =
            TxDescriptor::frame(self.tx_buffers.device + at as u64, len as u16).words();
        let descriptor = TX_RING + usize::from(index) * DESCRIPTOR_SIZE;
        self.rings.write(descriptor, low);
        self.rings.write(descriptor + 8, high);
        // The frame and its descriptor before the tail the device reads.
        fence(Ordering::SeqCst);
        self.regs.write(reg::TDT, u32::from(self.tx.tail()));
        Status::Ok
    }

    /// Copies waiting frames into the receive area as `[len u16][frame]`
    /// records and gives their buffers back to the device.
    fn receive(&mut self) -> u16 {
        let Some(session) = &self.session else {
            return 0;
        };
        let buffer = session.buffer;
        let mut at = RX_AREA.start;
        let mut count = 0u16;
        let mut tail = None;
        // At most one pass over the ring: consumed descriptors are re-armed
        // with a clear status, so the walk ends there at the latest.
        for _ in 0..self.rx.size() {
            let index = usize::from(self.rx.next());
            let place = RX_RING + index * DESCRIPTOR_SIZE;
            if self.rings.read::<u8>(place + STATUS_OFFSET) & rx_status::DONE == 0 {
                break;
            }
            // The rest of the write-back only after seeing `DONE`.
            fence(Ordering::SeqCst);
            let mut bytes = [0u8; DESCRIPTOR_SIZE];
            bytes[..8].copy_from_slice(&self.rings.read::<u64>(place).to_le_bytes());
            bytes[8..].copy_from_slice(&self.rings.read::<u64>(place + 8).to_le_bytes());
            let descriptor = RxDescriptor::decode(&bytes);
            match self.assembler.accept(&descriptor, BUFFER) {
                Ok(len) => {
                    let end = at + 2 + len.next_multiple_of(2);
                    if end > RX_AREA.end {
                        // The area is full: it waits for the next RECV.
                        break;
                    }
                    // SAFETY: `at..at + 2 + len` lies in the receive area
                    // of the session buffer (`end` checked), mapped
                    // read-write while it lives.
                    let record =
                        unsafe { core::slice::from_raw_parts_mut(buffer.add(at), 2 + len) };
                    record[..2].copy_from_slice(&(len as u16).to_le_bytes());
                    self.rx_buffers.copy_out(index * BUFFER, &mut record[2..]);
                    at = end;
                    count += 1;
                }
                Err(RxOutcome::Fragment) => self.counters.fragments += 1,
                Err(_) => self.counters.bad_frames += 1,
            }
            // Re-arm: our buffer's address (whatever the device wrote
            // back), status cleared.
            let rearmed =
                RxDescriptor::posted(self.rx_buffers.device + (index * BUFFER) as u64).encode();
            self.rings.copy_in(place, &rearmed);
            tail = Some(self.rx.consume());
        }
        if let Some(tail) = tail {
            // Re-armed descriptors before the tail that hands them over.
            fence(Ordering::SeqCst);
            self.regs.write(reg::RDT, u32::from(tail));
        }
        count
    }
}

/// Masks interrupts, stops DMA and resets the controller (§4.6.1): the
/// firmware may have left it receiving into memory it no longer owns.
fn reset(log: Handle, regs: Registers) -> Result<(), &'static str> {
    regs.write(reg::IMC, reg::int::ALL);
    regs.write(reg::RCTL, 0);
    regs.write(reg::TCTL, reg::tctl::PAD_SHORT_PACKETS);
    regs.flush();
    oceans_rt::sleep_ms(10);
    // No new bus-master requests, and wait for those in flight (§5.2.3).
    regs.update(reg::CTRL, reg::ctrl::GIO_MASTER_DISABLE, 0);
    if !wait_for(MASTER_DISABLE_MS, || {
        regs.read(reg::STATUS) & reg::status::GIO_MASTER_ENABLE == 0
    }) {
        // The reset below stops them anyway; worth knowing about.
        say(
            log,
            format_args!("bus-master requests still pending; resetting anyway"),
        );
    }
    regs.update(reg::CTRL, reg::ctrl::RESET, 0);
    oceans_rt::sleep_ms(10);
    if !wait_for(RESET_MS, || regs.read(reg::CTRL) & reg::ctrl::RESET == 0) {
        return Err("device did not come out of reset");
    }
    if !wait_for(RESET_MS, || {
        regs.read(reg::EECD) & reg::eecd::AUTO_READ_DONE != 0
    }) {
        return Err("NVM auto-read did not finish after reset");
    }
    regs.write(reg::IMC, reg::int::ALL);
    let _ = regs.read(reg::ICR);
    Ok(())
}

/// Settings Intel's 82574 initialisation applies after every reset
/// (reserved bits with required values, and a PCIe completion erratum
/// workaround that otherwise causes transmit timeouts).
fn initialize_hardware_bits(regs: Registers) {
    regs.update(reg::TXDCTL, reg::txdctl::COUNT_DESCRIPTORS, 0);
    regs.update(reg::TARC0, 1 << 26, 0xf << 27);
    regs.update(reg::CTRL, 0, reg::ctrl::RESERVED_29);
    regs.update(
        reg::CTRL_EXT,
        reg::ctrl_ext::RESERVED_22,
        reg::ctrl_ext::RESERVED_23,
    );
    regs.update(reg::GCR, 1 << 22, 0);
    regs.update(reg::GCR2, 1, 0);
}

fn read_nvm(regs: Registers, word: u16) -> Result<u16, &'static str> {
    regs.write(reg::EERD, nvm::read_request(word));
    let mut value = None;
    wait_for(NVM_MS, || {
        value = nvm::read_result(regs.read(reg::EERD));
        value.is_some()
    });
    value.ok_or("NVM read timed out")
}

/// The station address, from an NVM whose checksum holds (a bad checksum
/// means the device's configuration cannot be trusted). If the NVM has no
/// usable address, the one the device loaded (or firmware set) in receive
/// address 0 is used.
fn read_mac(regs: Registers) -> Result<Mac, &'static str> {
    let mut words = [0u16; nvm::CHECKSUM_WORDS];
    for (index, word) in words.iter_mut().enumerate() {
        *word = read_nvm(regs, index as u16)?;
    }
    if !nvm::checksum_valid(&words) {
        return Err("NVM checksum is invalid");
    }
    nvm::mac(&words)
        .or_else(|| {
            oceans_e1000e::mac_from_receive_address(regs.read(reg::RAL0), regs.read(reg::RAH0))
        })
        .ok_or("no valid MAC address in the NVM")
}

fn phy_command(regs: Registers, command: u32) -> Result<u16, &'static str> {
    regs.write(reg::MDIC, command);
    let mut state = mdic::State::Busy;
    wait_for(PHY_MS, || {
        state = mdic::state(regs.read(reg::MDIC));
        state != mdic::State::Busy
    });
    match state {
        mdic::State::Done(value) => Ok(value),
        mdic::State::Failed => Err("PHY access failed"),
        mdic::State::Busy => Err("PHY access timed out"),
    }
}

/// Restarts autonegotiation in the PHY.
fn restart_autonegotiation(regs: Registers) -> Result<(), &'static str> {
    let control = phy_command(regs, mdic::read(mdic::PHY_CONTROL))?;
    let control = control | mdic::AUTONEG_ENABLE | mdic::RESTART_AUTONEG;
    phy_command(regs, mdic::write(mdic::PHY_CONTROL, control)).map(drop)
}
