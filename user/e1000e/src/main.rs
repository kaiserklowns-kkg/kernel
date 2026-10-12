//! e1000e: the driver for Intel 82574L Gigabit Ethernet controllers (PCI
//! `8086:10d3`, QEMU's `-device e1000e`, ADR-0041).
//!
//! An ordinary service with exactly what `services.conf` grants: one device
//! capability (`grant = device:8086:10d3`), the endpoint it serves
//! (`provide = netdev`) and a log. It serves the same `netdev` protocol as
//! virtio-net (ADR-0023), through `oceans-nic` (ADR-0123), so the stack
//! (`net`) runs unchanged on top.
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
//!   endpoint; every cause (receive, transmit, link changes, overruns) is
//!   routed to it. Without MSI-X the driver polls every 10 ms.
//! - Errors: bad frames are dropped and counted; the counts and the
//!   device's own error statistics are logged (at most every 10 s). A
//!   device that stops answering (registers read all ones) ends the driver
//!   with an error, for init's restart policy.
//!
//! The register, descriptor and ring logic is in `oceans-e1000e`
//! (libs/e1000e), tested on the host.

#![no_std]
#![no_main]

mod pch;

use core::sync::atomic::{Ordering, fence};

use oceans_e1000e::{
    DESCRIPTOR_SIZE, Link, RxAssembler, RxDescriptor, RxOutcome, RxRing, STATUS_OFFSET,
    TxDescriptor, TxRing, mdic, nvm, reg, rx_status, tx_status,
};
use oceans_net_proto::{Mac, Status};
use oceans_nic::{EXIT_BAD_START, EXIT_DEVICE, Events, Frames, IRQ, Nic, Registers, ShowMac};
use oceans_rt::{Directory, Handle, Start};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const NAME: &str = "e1000e";
const RX_DESCRIPTORS: u16 = 64;
const TX_DESCRIPTORS: u16 = 32;
/// One buffer per descriptor (`RCTL.BSIZE` 2048).
const BUFFER: usize = 2048;
/// Where the rings sit in their (one page of) DMA memory.
const RX_RING: usize = 0;
const TX_RING: usize = RX_DESCRIPTORS as usize * DESCRIPTOR_SIZE;
const RINGS_SIZE: usize = TX_RING + TX_DESCRIPTORS as usize * DESCRIPTOR_SIZE;
const MSIX_VECTOR: u16 = 0;
/// Error statistics are read, and changes logged, at most this often.
const STATISTICS_MS: u64 = 10_000;
const MTU: u16 = 1500;

/// Bring-up timeouts (Intel's 82574 initialisation uses the same bounds).
const MASTER_DISABLE_MS: u64 = 800;
const RESET_MS: u64 = 100;
const NVM_MS: u64 = 10;
const PHY_MS: u64 = 10;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    oceans_nic::say(log, NAME, args);
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
    let (mut driver, events) = match Driver::start(log, device, server) {
        Ok(started) => started,
        Err(problem) => {
            say(log, format_args!("{problem}"));
            return EXIT_DEVICE;
        }
    };
    oceans_nic::serve(&mut driver, NAME, log, server, events)
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
    /// An I217, I218 or I219 (ADR-0123); none for the 82574L.
    pch: Option<pch::Pch>,
    counters: Counters,
    reported: Counters,
    next_statistics: u64,
}

impl Driver {
    fn start(log: Handle, device: Handle, server: Handle) -> Result<(Self, Events), &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        // The PCI device ID: the 82574L, or a PCH LAN (ADR-0123).
        let id = oceans_rt::device_config_read(device, 2, 2)
            .map_err(|_| "cannot read the device ID")? as u16;
        let generation = oceans_e1000e::pch::generation(id);
        let span = if generation.is_some() {
            oceans_e1000e::pch::SPAN
        } else {
            reg::SPAN
        };
        let regs = Registers::map(device, 0, span)?;
        let pch = generation.map(|g| pch::Pch::new(regs, device, id, g, log));
        let setup = Self::setup(log, device, server, regs, pch);
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
        mut pch: Option<pch::Pch>,
    ) -> Result<(Self, Events), &'static str> {
        if regs.read(reg::STATUS) == u32::MAX {
            return Err("device does not respond (registers read all ones)");
        }
        let rings = Dma::new(device, RINGS_SIZE)?;
        let mac = match &mut pch {
            Some(pch) => pch.bring_up(&rings)?,
            None => {
                reset(log, regs)?;
                initialize_hardware_bits(regs);
                read_mac(regs)?
            }
        };
        let (low, high) = oceans_e1000e::receive_address(&mac);
        regs.write(reg::RAL0, low);
        regs.write(reg::RAH0, high);
        // The PCH LAN has 32 multicast table entries, the 82574 128.
        let table = if pch.is_some() { 32 } else { reg::MTA_LEN };
        for index in 0..table {
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
        // The PCH LAN has no MSI-X: it is polled.
        let interrupts =
            pch.is_none() && oceans_rt::device_irq(device, MSIX_VECTOR, events, IRQ).is_ok();
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
            pch,
            counters: Counters::default(),
            reported: Counters::default(),
            next_statistics: oceans_rt::clock_ms() + STATISTICS_MS,
        };
        say(
            log,
            format_args!(
                "MAC {}, {}, {}",
                ShowMac(mac),
                match &driver.pch {
                    Some(pch) => pch.name(),
                    None => "82574L",
                },
                if interrupts { "MSI-X" } else { "polling" }
            ),
        );
        driver.link = Link::from_status(regs.read(reg::STATUS));
        match driver.link {
            Link::Up { .. } => say(log, format_args!("link {}", driver.link)),
            Link::Down => {
                // The PHY may have been left idle: negotiate afresh. The
                // link-change interrupt reports the result.
                let restarted = match &driver.pch {
                    Some(pch) => pch.restart_autonegotiation(),
                    None => restart_autonegotiation(regs),
                };
                match restarted {
                    Ok(()) => say(log, format_args!("link down, autonegotiating")),
                    Err(problem) => say(log, format_args!("link down; {problem}")),
                }
            }
        }
        Ok((
            driver,
            Events {
                handle: events,
                interrupts,
            },
        ))
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
}

impl Nic for Driver {
    fn info(&self) -> (Mac, u16) {
        (self.mac, MTU)
    }

    fn service(&mut self) -> Result<(), &'static str> {
        let causes = self.regs.read(reg::ICR);
        if causes == u32::MAX && self.regs.read(reg::STATUS) == u32::MAX {
            return Err("device stopped responding (registers read all ones)");
        }
        if causes != 0 {
            // Write-1-to-clear (with MSI-X, reading ICR does not clear
            // it). Frames that arrive after this raise a new interrupt;
            // those before it are in the ring, read by `RECV`.
            self.regs.write(reg::ICR, causes);
        }
        if causes & reg::int::RX_OVERRUN != 0 {
            self.counters.overruns += 1;
        }
        let link = Link::from_status(self.regs.read(reg::STATUS));
        if link != self.link {
            self.link = link;
            say(self.log, format_args!("link {link}"));
            if let (Some(pch), Link::Up { mbps, full_duplex }) = (&self.pch, link)
                && let Err(problem) = pch.link_up(mbps, full_duplex)
            {
                say(self.log, format_args!("tuning for the link: {problem}"));
            }
        }
        self.reclaim_transmitted();
        let now = oceans_rt::clock_ms();
        if now >= self.next_statistics {
            self.next_statistics = now + STATISTICS_MS;
            self.report_errors();
        }
        Ok(())
    }

    fn frames_waiting(&self) -> bool {
        let at = RX_RING + usize::from(self.rx.next()) * DESCRIPTOR_SIZE;
        self.rings.read::<u8>(at + STATUS_OFFSET) & rx_status::DONE != 0
    }

    fn transmit(&mut self, frame: &[u8]) -> Status {
        self.reclaim_transmitted();
        let Some(index) = self.tx.claim() else {
            return Status::NoBuffers;
        };
        let at = usize::from(index) * BUFFER;
        self.tx_buffers.copy_in(at, frame);
        let [low, high] =
            TxDescriptor::frame(self.tx_buffers.device + at as u64, frame.len() as u16).words();
        let descriptor = TX_RING + usize::from(index) * DESCRIPTOR_SIZE;
        self.rings.write(descriptor, low);
        self.rings.write(descriptor + 8, high);
        // The frame and its descriptor before the tail the device reads.
        fence(Ordering::SeqCst);
        self.regs.write(reg::TDT, u32::from(self.tx.tail()));
        Status::Ok
    }

    fn receive(&mut self, frames: &mut Frames) {
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
                    let buffers = self.rx_buffers;
                    if !frames.push(len, |out| buffers.copy_out(index * BUFFER, out)) {
                        // The area is full: it waits for the next RECV.
                        break;
                    }
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
    }
}

/// Masks interrupts, stops DMA and resets the controller (§4.6.1): the
/// firmware may have left it receiving into memory it no longer owns.
fn reset(log: Handle, regs: Registers) -> Result<(), &'static str> {
    regs.write(reg::IMC, reg::int::ALL);
    regs.write(reg::RCTL, 0);
    regs.write(reg::TCTL, reg::tctl::PAD_SHORT_PACKETS);
    // Posted writes reach the device before the delay starts.
    let _ = regs.read(reg::STATUS);
    oceans_rt::sleep_ms(10);
    // No new bus-master requests, and wait for those in flight (§5.2.3).
    regs.update(reg::CTRL, reg::ctrl::GIO_MASTER_DISABLE, 0);
    if !oceans_nic::wait_for(MASTER_DISABLE_MS, || {
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
    if !oceans_nic::wait_for(RESET_MS, || regs.read(reg::CTRL) & reg::ctrl::RESET == 0) {
        return Err("device did not come out of reset");
    }
    if !oceans_nic::wait_for(RESET_MS, || {
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
    oceans_nic::wait_for(NVM_MS, || {
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
    oceans_nic::wait_for(PHY_MS, || {
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
