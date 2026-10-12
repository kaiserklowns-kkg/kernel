//! rtl8168: the driver for Realtek's RTL8111/8168 Gigabit Ethernet
//! controllers (ADR-0123), on most PC boards whose Ethernet is not
//! Intel's.
//!
//! An ordinary service with what `services.conf` grants: the first of its
//! controllers present (`grant = device:10ec:8168,...`), the endpoint it
//! serves (`provide = netdev`) and a log. It serves the netdev protocol
//! through `oceans-nic`, so the stack runs unchanged on top.
//!
//! - Bring-up: the revision told from `TxConfig` (unknown ones refused),
//!   the controller reset, the station address from its ID registers, the
//!   PHY reset and autonegotiating, the rings handed over. No microcode is
//!   loaded: the path BSD's drivers take.
//! - Receive: 64 descriptors, each with its own 2048-byte buffer, given
//!   back to the device (`OWN`) only once the stack has taken their frames.
//! - Transmit: 32 descriptors and buffers. PCIe parts can miss a transmit
//!   kick, so the driver kicks again whenever frames are still queued.
//! - Interrupts: MSI-X vector 0 where the part has it, else MSI (ADR-0124).
//!   Without either the driver polls every 10 ms.
//!
//! QEMU has no model of this chip: the logic in `oceans-rtl8168`
//! (libs/rtl8168) is tested on the host, this driver on real machines.

#![no_std]
#![no_main]

use core::sync::atomic::{Ordering, fence};

use oceans_net_proto::{Mac, Status};
use oceans_nic::{EXIT_BAD_START, EXIT_DEVICE, Events, Frames, IRQ, Nic, Registers, ShowMac};
use oceans_rt::{Directory, Handle, Start};
use oceans_rtl8168::{DESCRIPTOR_SIZE, Link, Revision, RxOutcome, desc, phyar, reg};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const NAME: &str = "rtl8168";
const RX_DESCRIPTORS: usize = 64;
const TX_DESCRIPTORS: usize = 32;
const BUFFER: usize = 2048;
const RX_RING: usize = 0;
/// 256-byte aligned, as both rings must be.
const TX_RING: usize = RX_DESCRIPTORS * DESCRIPTOR_SIZE;
const RINGS_SIZE: usize = TX_RING + TX_DESCRIPTORS * DESCRIPTOR_SIZE;
const MSIX_VECTOR: u16 = 0;
const STATISTICS_MS: u64 = 10_000;
const MTU: u16 = 1500;

const RESET_MS: u64 = 10;
const PHY_MS: u64 = 50;
const PHY_RESET_MS: u64 = 100;

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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Counters {
    bad_frames: u64,
    missed: u64,
    overflows: u64,
    tx_failed: u64,
}

struct Driver {
    log: Handle,
    regs: Registers,
    mac: Mac,
    rings: Dma,
    rx_buffers: Dma,
    tx_buffers: Dma,
    /// The next receive descriptor to look at.
    rx_next: usize,
    /// The next transmit descriptor to fill, and the oldest in flight.
    tx_next: usize,
    tx_oldest: usize,
    tx_in_flight: usize,
    link: Link,
    counters: Counters,
    reported: Counters,
    next_statistics: u64,
}

impl Driver {
    fn start(log: Handle, device: Handle, server: Handle) -> Result<(Self, Events), &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let regs = Registers::map(device, oceans_rtl8168::REGISTER_BAR, oceans_rtl8168::SPAN)
            .or_else(|_| {
                Registers::map(
                    device,
                    oceans_rtl8168::REGISTER_BAR_FALLBACK,
                    oceans_rtl8168::SPAN,
                )
            })?;
        let setup = Self::setup(log, device, server, regs);
        if setup.is_err() {
            regs.write16(reg::INTR_MASK, 0);
            regs.write8(reg::CHIP_CMD, 0);
        }
        setup
    }

    fn setup(
        log: Handle,
        device: Handle,
        server: Handle,
        regs: Registers,
    ) -> Result<(Self, Events), &'static str> {
        let tx_config = regs.read(reg::TX_CONFIG);
        if tx_config == u32::MAX {
            return Err("device does not respond (registers read all ones)");
        }
        let revision = Revision::from_tx_config(tx_config)
            .ok_or("an RTL8168 revision this driver does not know")?;

        // Quiet, then reset.
        regs.write16(reg::INTR_MASK, 0);
        regs.write16(reg::INTR_STATUS, reg::int::ALL);
        regs.write8(reg::CHIP_CMD, reg::chip_cmd::RESET);
        if !oceans_nic::wait_for(RESET_MS, || {
            regs.read8(reg::CHIP_CMD) & reg::chip_cmd::RESET == 0
        }) {
            return Err("device did not come out of reset");
        }
        if revision.phy_wake_pm() {
            regs.write8(reg::PMCH, regs.read8(reg::PMCH) | reg::PMCH_PHY_WAKE);
        }
        if revision.phy_wake() {
            phy_write(regs, phyar::PAGE_SELECT, 0)?;
            phy_write(regs, phyar::WAKE, 0)?;
        }
        let mac = oceans_rtl8168::station(regs.read(reg::IDR0), regs.read(reg::IDR4))
            .ok_or("no valid MAC address in the ID registers")?;

        let rings = Dma::new(device, RINGS_SIZE)?;
        let rx_buffers = Dma::new(device, RX_DESCRIPTORS * BUFFER)?;
        let tx_buffers = Dma::new(device, TX_DESCRIPTORS * BUFFER)?;
        for index in 0..RX_DESCRIPTORS {
            let [command, address] = oceans_rtl8168::rx_posted(
                rx_buffers.device + (index * BUFFER) as u64,
                BUFFER,
                index + 1 == RX_DESCRIPTORS,
            );
            rings.write(RX_RING + index * DESCRIPTOR_SIZE + 8, address);
            rings.write(RX_RING + index * DESCRIPTOR_SIZE, command);
        }
        for index in 0..TX_DESCRIPTORS {
            let [command, address] = oceans_rtl8168::tx_empty(index + 1 == TX_DESCRIPTORS);
            rings.write(TX_RING + index * DESCRIPTOR_SIZE + 8, address);
            rings.write(TX_RING + index * DESCRIPTOR_SIZE, command);
        }
        fence(Ordering::SeqCst);

        // C+ first, then the rest, as Realtek's parts want.
        regs.write16(reg::CPLUS_CMD, oceans_rtl8168::CPLUS);
        let (low, high) = oceans_rtl8168::station_registers(&mac);
        regs.write8(reg::CFG9346, reg::cfg9346::UNLOCK);
        regs.write(reg::IDR0, low);
        regs.write(reg::IDR4, high);
        regs.write8(reg::CFG9346, reg::cfg9346::LOCK);
        let split = |address: u64| (address as u32, (address >> 32) as u32);
        let (low, high) = split(rings.device + RX_RING as u64);
        regs.write(reg::RX_DESC_HIGH, high);
        regs.write(reg::RX_DESC_LOW, low);
        let (low, high) = split(rings.device + TX_RING as u64);
        regs.write(reg::TX_DESC_HIGH, high);
        regs.write(reg::TX_DESC_LOW, low);
        if revision.gated() {
            regs.update(reg::MISC, 0, reg::MISC_RXDV_GATE);
        }
        regs.write(reg::TX_CONFIG, oceans_rtl8168::TX_CONFIG);
        regs.write8(reg::MAX_TX_PACKET, oceans_rtl8168::MAX_TX_PACKET);
        // All multicast (IPv6; and the 8168F's filter is broken anyway).
        regs.write(reg::MAR0, !0);
        regs.write(reg::MAR4, !0);
        regs.write(reg::RX_CONFIG, oceans_rtl8168::rx_config(revision));
        regs.write16(reg::INTR_MITIGATE, 0);
        regs.write16(reg::RX_MAX_SIZE, BUFFER as u16);
        regs.write8(
            reg::CHIP_CMD,
            reg::chip_cmd::TX_ENABLE | reg::chip_cmd::RX_ENABLE,
        );
        regs.write(reg::MISSED, 0);
        regs.write(reg::TIMER_INT, 0);
        regs.write8(reg::CFG9346, reg::cfg9346::UNLOCK);
        regs.write8(
            reg::CONFIG1,
            regs.read8(reg::CONFIG1) | reg::CONFIG1_DRIVER_LOADED,
        );
        regs.write8(reg::CFG9346, reg::cfg9346::LOCK);

        let events = oceans_rt::notification_create().map_err(|_| "no notification")?;
        oceans_rt::endpoint_bind(server, events).map_err(|_| "cannot bind events")?;
        // MSI-X where the part has it, else MSI (ADR-0124); Config2 says
        // which the chip signals.
        let kind = oceans_rt::device_irq(device, MSIX_VECTOR, events, IRQ).ok();
        let interrupts = kind.is_some();
        let msi = kind == Some(oceans_rt::interrupt::MSI);
        regs.write8(reg::CFG9346, reg::cfg9346::UNLOCK);
        let config2 = regs.read8(reg::CONFIG2);
        regs.write8(
            reg::CONFIG2,
            if msi {
                config2 | reg::CONFIG2_MSI
            } else {
                config2 & !reg::CONFIG2_MSI
            },
        );
        regs.write8(reg::CFG9346, reg::cfg9346::LOCK);
        regs.write16(reg::INTR_STATUS, reg::int::ALL);
        regs.write16(
            reg::INTR_MASK,
            if interrupts {
                oceans_rtl8168::INTERRUPTS
            } else {
                0
            },
        );

        let driver = Self {
            log,
            regs,
            mac,
            rings,
            rx_buffers,
            tx_buffers,
            rx_next: 0,
            tx_next: 0,
            tx_oldest: 0,
            tx_in_flight: 0,
            link: Link::Down,
            counters: Counters::default(),
            reported: Counters::default(),
            next_statistics: oceans_rt::clock_ms() + STATISTICS_MS,
        };
        say(
            log,
            format_args!(
                "MAC {}, {}, {}",
                ShowMac(mac),
                revision.name(),
                match kind {
                    Some(oceans_rt::interrupt::MSI) => "MSI",
                    Some(_) => "MSI-X",
                    None => "polling",
                }
            ),
        );
        match autonegotiate(regs) {
            Ok(()) => say(log, format_args!("autonegotiating")),
            Err(problem) => say(log, format_args!("link: {problem}")),
        }
        Ok((
            driver,
            Events {
                handle: events,
                interrupts,
            },
        ))
    }

    fn report_errors(&mut self) {
        self.counters.missed += u64::from(self.regs.read(reg::MISSED) & 0x00ff_ffff);
        self.regs.write(reg::MISSED, 0);
        let c = self.counters;
        if c != self.reported {
            self.reported = c;
            say(
                self.log,
                format_args!(
                    "errors so far: receive {} bad, {} missed, {} overflows; transmit {} failed",
                    c.bad_frames, c.missed, c.overflows, c.tx_failed
                ),
            );
        }
    }

    fn reclaim_transmitted(&mut self) {
        while self.tx_in_flight > 0 {
            let at = TX_RING + self.tx_oldest * DESCRIPTOR_SIZE;
            let status: u32 = self.rings.read(at);
            if status & desc::OWN != 0 {
                break;
            }
            if status & desc::TX_ERROR != 0 {
                self.counters.tx_failed += 1;
            }
            self.tx_oldest = (self.tx_oldest + 1) % TX_DESCRIPTORS;
            self.tx_in_flight -= 1;
        }
    }

    fn rx_status(&self, index: usize) -> u32 {
        self.rings.read(RX_RING + index * DESCRIPTOR_SIZE)
    }
}

impl Nic for Driver {
    fn info(&self) -> (Mac, u16) {
        (self.mac, MTU)
    }

    fn service(&mut self) -> Result<(), &'static str> {
        let causes = self.regs.read16(reg::INTR_STATUS);
        if causes == u16::MAX && self.regs.read(reg::TX_CONFIG) == u32::MAX {
            return Err("device stopped responding (registers read all ones)");
        }
        if causes != 0 {
            self.regs.write16(reg::INTR_STATUS, causes);
        }
        if causes & reg::int::SYSTEM_ERROR != 0 {
            // A PCI error: init's restart brings it up afresh.
            return Err("the controller reported a system error");
        }
        if causes & reg::int::RX_FIFO_OVERFLOW != 0 {
            self.counters.overflows += 1;
        }
        let link = Link::from_phy_status(self.regs.read8(reg::PHY_STATUS));
        if link != self.link {
            self.link = link;
            say(self.log, format_args!("link {link}"));
        }
        self.reclaim_transmitted();
        if self.tx_in_flight > 0 {
            // A kick may have been missed (PCIe parts).
            self.regs.write8(reg::TX_POLL, reg::TX_POLL_NORMAL);
        }
        let now = oceans_rt::clock_ms();
        if now >= self.next_statistics {
            self.next_statistics = now + STATISTICS_MS;
            self.report_errors();
        }
        Ok(())
    }

    fn frames_waiting(&self) -> bool {
        self.rx_status(self.rx_next) & desc::OWN == 0
    }

    fn transmit(&mut self, frame: &[u8]) -> Status {
        self.reclaim_transmitted();
        if self.tx_in_flight == TX_DESCRIPTORS {
            return Status::NoBuffers;
        }
        let index = self.tx_next;
        let at = index * BUFFER;
        self.tx_buffers.copy_in(at, frame);
        let [command, address] = oceans_rtl8168::tx_frame(
            self.tx_buffers.device + at as u64,
            frame.len() as u16,
            index + 1 == TX_DESCRIPTORS,
        );
        let place = TX_RING + index * DESCRIPTOR_SIZE;
        self.rings.write(place + 8, address);
        // The address before `OWN` hands the descriptor over.
        fence(Ordering::SeqCst);
        self.rings.write(place, command);
        fence(Ordering::SeqCst);
        self.tx_next = (index + 1) % TX_DESCRIPTORS;
        self.tx_in_flight += 1;
        self.regs.write8(reg::TX_POLL, reg::TX_POLL_NORMAL);
        Status::Ok
    }

    fn receive(&mut self, frames: &mut Frames) {
        for _ in 0..RX_DESCRIPTORS {
            let index = self.rx_next;
            let status = self.rx_status(index);
            match oceans_rtl8168::rx_outcome(status, BUFFER) {
                RxOutcome::Pending => break,
                RxOutcome::Frame(len) => {
                    fence(Ordering::SeqCst);
                    let buffers = self.rx_buffers;
                    if !frames.push(len, |out| buffers.copy_out(index * BUFFER, out)) {
                        break;
                    }
                }
                RxOutcome::Bad => self.counters.bad_frames += 1,
            }
            // Back to the device: the address first, `OWN` last.
            let place = RX_RING + index * DESCRIPTOR_SIZE;
            let [command, address] = oceans_rtl8168::rx_posted(
                self.rx_buffers.device + (index * BUFFER) as u64,
                BUFFER,
                index + 1 == RX_DESCRIPTORS,
            );
            self.rings.write(place + 8, address);
            fence(Ordering::SeqCst);
            self.rings.write(place, command);
            self.rx_next = (index + 1) % RX_DESCRIPTORS;
        }
    }
}

/// A little time between PHY accesses (Realtek's drivers wait 20 µs).
fn settle() {
    for _ in 0..20_000 {
        core::hint::spin_loop();
    }
}

fn phy_write(regs: Registers, register: u8, value: u16) -> Result<(), &'static str> {
    regs.write(reg::PHYAR, phyar::write(register, value));
    let done = oceans_nic::wait_for(PHY_MS, || regs.read(reg::PHYAR) & phyar::FLAG == 0);
    settle();
    if done {
        Ok(())
    } else {
        Err("PHY write timed out")
    }
}

fn phy_read(regs: Registers, register: u8) -> Result<u16, &'static str> {
    regs.write(reg::PHYAR, phyar::read(register));
    let mut value = 0;
    let done = oceans_nic::wait_for(PHY_MS, || {
        value = regs.read(reg::PHYAR);
        value & phyar::FLAG != 0
    });
    settle();
    if done {
        Ok(value as u16)
    } else {
        Err("PHY read timed out")
    }
}

/// Resets the PHY and starts autonegotiation, advertising everything up to
/// 1000 Mb/s full duplex.
fn autonegotiate(regs: Registers) -> Result<(), &'static str> {
    phy_write(regs, phyar::CONTROL, phyar::RESET)?;
    let mut reset_done = false;
    oceans_nic::wait_for(PHY_RESET_MS, || {
        reset_done = phy_read(regs, phyar::CONTROL).is_ok_and(|v| v & phyar::RESET == 0);
        reset_done
    });
    if !reset_done {
        return Err("the PHY did not come out of reset");
    }
    phy_write(regs, phyar::ADVERTISE, phyar::ADVERTISE_ALL)?;
    phy_write(regs, phyar::GIGABIT_CONTROL, phyar::ADVERTISE_GIGABIT)?;
    phy_write(regs, phyar::CONTROL, phyar::AUTONEG)
}
