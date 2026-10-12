//! igc: the driver for Intel's 82576, I210/I211 and I225/I226 Ethernet
//! controllers (ADR-0123): the I225-V and I226-V (2.5 Gb/s) of many
//! desktop and mini-PC boards, and QEMU's `-device igb` (an 82576), on
//! which it is tested.
//!
//! An ordinary service with what `services.conf` grants: the first of its
//! controllers present (`grant = device:8086:15f3,...`), the endpoint it
//! serves (`provide = netdev`) and a log. It serves the netdev protocol
//! through `oceans-nic`, so the stack runs unchanged on top.
//!
//! - Bring-up: interrupts masked, DMA stopped, the controller reset (`RST`,
//!   or `DEV_RST` on the I225), the address taken from receive address 0,
//!   the link autonegotiated (and on the I225 its power saving off), one
//!   receive and one transmit queue with advanced descriptors.
//! - Receive: 64 descriptors, each with its own 2048-byte buffer, re-armed
//!   only when the stack has taken their frames.
//! - Transmit: 32 descriptors and buffers, reclaimed once written back.
//! - Interrupts: MSI-X vector 0 for both queues and link changes; without
//!   MSI-X the driver polls every 10 ms.
//!
//! What differs between the families is in `oceans-igc` (libs/igc),
//! tested on the host.

#![no_std]
#![no_main]

use core::sync::atomic::{Ordering, fence};

use oceans_igc::{
    DESCRIPTOR_SIZE, Family, Link, RxAssembler, RxOutcome, RxRing, RxWriteBack, TX_DONE,
    TX_STATUS_OFFSET, TxRing, mdic, reg,
};
use oceans_net_proto::{Mac, Status};
use oceans_nic::{EXIT_BAD_START, EXIT_DEVICE, Events, Frames, IRQ, Nic, Registers, ShowMac};
use oceans_rt::{Directory, Handle, Start};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const NAME: &str = "igc";
const RX_DESCRIPTORS: u16 = 64;
const TX_DESCRIPTORS: u16 = 32;
const BUFFER: usize = 2048;
const RX_RING: usize = 0;
const TX_RING: usize = RX_DESCRIPTORS as usize * DESCRIPTOR_SIZE;
const RINGS_SIZE: usize = TX_RING + TX_DESCRIPTORS as usize * DESCRIPTOR_SIZE;
const MSIX_VECTOR: u16 = 0;
const STATISTICS_MS: u64 = 10_000;
const MTU: u16 = 1500;

const MASTER_DISABLE_MS: u64 = 80;
const AUTO_READ_MS: u64 = 10;
const QUEUE_ENABLE_MS: u64 = 10;
const PHY_MS: u64 = 100;

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
    fragments: u64,
    missed: u64,
    overruns: u64,
    crc_errors: u64,
}

struct Driver {
    log: Handle,
    regs: Registers,
    family: Family,
    mac: Mac,
    rings: Dma,
    rx_buffers: Dma,
    tx_buffers: Dma,
    rx: RxRing,
    tx: TxRing,
    assembler: RxAssembler,
    link: Link,
    msix: bool,
    counters: Counters,
    reported: Counters,
    next_statistics: u64,
}

impl Driver {
    fn start(log: Handle, device: Handle, server: Handle) -> Result<(Self, Events), &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let id = oceans_rt::device_config_read(device, 2, 2)
            .map_err(|_| "cannot read the device ID")? as u16;
        let family = oceans_igc::family(id).ok_or("not a controller this driver knows")?;
        let regs = Registers::map(device, 0, reg::SPAN)?;
        let setup = Self::setup(log, device, server, regs, id, family);
        if setup.is_err() {
            regs.write(family.imc(), reg::int::ALL);
            regs.write(reg::EIMC, !0);
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
        id: u16,
        family: Family,
    ) -> Result<(Self, Events), &'static str> {
        if regs.read(reg::STATUS) == u32::MAX {
            return Err("device does not respond (registers read all ones)");
        }
        reset(log, regs, family);
        regs.update(reg::CTRL_EXT, reg::ctrl_ext::DRIVER_LOADED, 0);
        regs.write(reg::WUC, 0);
        let mac = match oceans_igc::mac_from_receive_address(
            regs.read(reg::RAL0),
            regs.read(reg::RAH0),
        ) {
            Some(mac) => mac,
            None if oceans_igc::blank_nvm(id) => {
                // No NVM: an address of our own, locally administered.
                let mut mac = [0u8; 6];
                oceans_rt::random(&mut mac).map_err(|_| "no randomness for an address")?;
                mac[0] = (mac[0] & !1) | 2;
                say(
                    log,
                    format_args!("no address in the NVM; using a local one"),
                );
                mac
            }
            None => return Err("no valid MAC address in receive address 0"),
        };
        let (low, high) = oceans_igc::receive_address(&mac);
        regs.write(reg::RAL0, low);
        regs.write(reg::RAH0, high);
        for index in 0..reg::MTA_LEN {
            regs.write(reg::MTA + 4 * index, 0);
        }

        // Link: follow the PHY's autonegotiation, nothing forced; on the
        // I225 neither its own link-down saving nor EEE.
        regs.update(
            reg::CTRL,
            reg::ctrl::SET_LINK_UP,
            reg::ctrl::FORCE_SPEED | reg::ctrl::FORCE_DUPLEX,
        );
        if family == Family::I225 {
            regs.update(reg::PHPM, 0, reg::phpm::GO_LINK_DOWN);
            regs.update(reg::IPCNFG, 0, reg::IPCNFG_EEE);
            regs.update(reg::EEER, 0, reg::EEER_LPI);
        }

        let rings = Dma::new(device, RINGS_SIZE)?;
        let rx_buffers = Dma::new(device, usize::from(RX_DESCRIPTORS) * BUFFER)?;
        let tx_buffers = Dma::new(device, usize::from(TX_DESCRIPTORS) * BUFFER)?;
        let rx = RxRing::new(RX_DESCRIPTORS);
        let tx = TxRing::new(TX_DESCRIPTORS);
        for index in 0..usize::from(RX_DESCRIPTORS) {
            let [address, header] =
                oceans_igc::rx_posted(rx_buffers.device + (index * BUFFER) as u64);
            rings.write(RX_RING + index * DESCRIPTOR_SIZE, address);
            rings.write(RX_RING + index * DESCRIPTOR_SIZE + 8, header);
        }
        rings.copy_in(TX_RING, &[0; TX_DESCRIPTORS as usize * DESCRIPTOR_SIZE]);
        fence(Ordering::SeqCst);

        let split = |address: u64| (address as u32, (address >> 32) as u32);
        // Receive: the queue off while it is programmed.
        regs.write(reg::RCTL, 0);
        regs.write(reg::RXDCTL, 0);
        let (low, high) = split(rings.device + RX_RING as u64);
        regs.write(reg::RDBAL, low);
        regs.write(reg::RDBAH, high);
        regs.write(
            reg::RDLEN,
            (usize::from(RX_DESCRIPTORS) * DESCRIPTOR_SIZE) as u32,
        );
        regs.write(reg::SRRCTL, oceans_igc::split_receive_control(BUFFER));
        regs.write(reg::RDH, 0);
        regs.write(reg::RDT, 0);
        regs.write(
            reg::RXDCTL,
            oceans_igc::receive_queue(regs.read(reg::RXDCTL)),
        );
        if !oceans_nic::wait_for(QUEUE_ENABLE_MS, || {
            regs.read(reg::RXDCTL) & oceans_igc::QUEUE_ENABLE != 0
        }) {
            return Err("the receive queue did not start");
        }
        regs.write(reg::RXCSUM, 0);
        regs.write(reg::RCTL, oceans_igc::receive_control());
        regs.write(reg::RDT, u32::from(rx.initial_tail()));

        // Transmit.
        regs.write(reg::TXDCTL, 0);
        let (low, high) = split(rings.device + TX_RING as u64);
        regs.write(reg::TDBAL, low);
        regs.write(reg::TDBAH, high);
        regs.write(
            reg::TDLEN,
            (usize::from(TX_DESCRIPTORS) * DESCRIPTOR_SIZE) as u32,
        );
        regs.write(reg::TDH, 0);
        regs.write(reg::TDT, 0);
        regs.write(reg::TXDCTL, oceans_igc::transmit_queue());
        regs.write(reg::TCTL, oceans_igc::transmit_control());

        // Interrupts: MSI-X vector 0 for the queues and the link, or
        // polled.
        let events = oceans_rt::notification_create().map_err(|_| "no notification")?;
        oceans_rt::endpoint_bind(server, events).map_err(|_| "cannot bind events")?;
        let msix = oceans_rt::device_irq(device, MSIX_VECTOR, events, IRQ).is_ok();
        let _ = regs.read(family.icr());
        if msix {
            regs.write(reg::GPIE, oceans_igc::GPIE_MSIX);
            regs.write(reg::IVAR0, oceans_igc::IVAR0_VECTOR0);
            regs.write(reg::IVAR_MISC, oceans_igc::IVAR_MISC_VECTOR0);
            regs.write(reg::EITR0, 0);
            regs.write(reg::EIAC, oceans_igc::VECTOR0);
            regs.write(reg::EIAM, oceans_igc::VECTOR0);
            regs.write(reg::EIMS, oceans_igc::VECTOR0);
            regs.write(
                family.ims(),
                reg::int::LINK_STATUS_CHANGE | reg::int::RX_OVERRUN,
            );
        } else {
            regs.write(family.ims(), oceans_igc::interrupt_mask());
        }

        let mut driver = Self {
            log,
            regs,
            family,
            mac,
            rings,
            rx_buffers,
            tx_buffers,
            rx,
            tx,
            assembler: RxAssembler::default(),
            link: Link::Down,
            msix,
            counters: Counters::default(),
            reported: Counters::default(),
            next_statistics: oceans_rt::clock_ms() + STATISTICS_MS,
        };
        say(
            log,
            format_args!(
                "MAC {}, {}, {}",
                ShowMac(mac),
                family.name(),
                if msix { "MSI-X" } else { "polling" }
            ),
        );
        driver.link = Link::from_status(regs.read(reg::STATUS));
        match driver.link {
            Link::Up { .. } => say(log, format_args!("link {}", driver.link)),
            Link::Down => match restart_autonegotiation(regs, family) {
                Ok(()) => say(log, format_args!("link down, autonegotiating")),
                Err(problem) => say(log, format_args!("link down; {problem}")),
            },
        }
        Ok((
            driver,
            Events {
                handle: events,
                interrupts: msix,
            },
        ))
    }

    fn report_errors(&mut self) {
        self.counters.missed += u64::from(self.regs.read(reg::MPC));
        self.counters.crc_errors += u64::from(self.regs.read(reg::CRCERRS));
        let c = self.counters;
        if c != self.reported {
            self.reported = c;
            say(
                self.log,
                format_args!(
                    "errors so far: receive {} bad, {} fragments, {} missed, {} overruns, {} CRC",
                    c.bad_frames, c.fragments, c.missed, c.overruns, c.crc_errors
                ),
            );
        }
    }

    fn reclaim_transmitted(&mut self) {
        while let Some(index) = self.tx.oldest() {
            let at = TX_RING + usize::from(index) * DESCRIPTOR_SIZE;
            let status: u32 = self.rings.read(at + TX_STATUS_OFFSET);
            if status & TX_DONE == 0 {
                break;
            }
            self.tx.complete();
        }
    }

    fn written_back(&self, index: usize) -> RxWriteBack {
        RxWriteBack::decode(
            self.rings
                .read::<u64>(RX_RING + index * DESCRIPTOR_SIZE + 8),
        )
    }
}

impl Nic for Driver {
    fn info(&self) -> (Mac, u16) {
        (self.mac, MTU)
    }

    fn service(&mut self) -> Result<(), &'static str> {
        let causes = self.regs.read(self.family.icr());
        if causes == u32::MAX && self.regs.read(reg::STATUS) == u32::MAX {
            return Err("device stopped responding (registers read all ones)");
        }
        if causes & reg::int::RX_OVERRUN != 0 {
            self.counters.overruns += 1;
        }
        if self.msix {
            // Vector 0 was masked when it fired (`EIAM`): unmask it again.
            let _ = self.regs.read(reg::EICR);
            self.regs.write(reg::EIMS, oceans_igc::VECTOR0);
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

    fn frames_waiting(&self) -> bool {
        self.written_back(usize::from(self.rx.next())).done()
    }

    fn transmit(&mut self, frame: &[u8]) -> Status {
        self.reclaim_transmitted();
        let Some(index) = self.tx.claim() else {
            return Status::NoBuffers;
        };
        let at = usize::from(index) * BUFFER;
        self.tx_buffers.copy_in(at, frame);
        let [address, words] =
            oceans_igc::tx_frame(self.tx_buffers.device + at as u64, frame.len() as u16);
        let descriptor = TX_RING + usize::from(index) * DESCRIPTOR_SIZE;
        self.rings.write(descriptor, address);
        self.rings.write(descriptor + 8, words);
        fence(Ordering::SeqCst);
        self.regs.write(reg::TDT, u32::from(self.tx.tail()));
        Status::Ok
    }

    fn receive(&mut self, frames: &mut Frames) {
        let mut tail = None;
        for _ in 0..self.rx.size() {
            let index = usize::from(self.rx.next());
            let written = self.written_back(index);
            if !written.done() {
                break;
            }
            fence(Ordering::SeqCst);
            match self.assembler.accept(written, BUFFER) {
                RxOutcome::Frame(len) => {
                    let buffers = self.rx_buffers;
                    if !frames.push(len, |out| buffers.copy_out(index * BUFFER, out)) {
                        break;
                    }
                }
                RxOutcome::Fragment => self.counters.fragments += 1,
                RxOutcome::Bad => self.counters.bad_frames += 1,
            }
            let place = RX_RING + index * DESCRIPTOR_SIZE;
            let [address, header] =
                oceans_igc::rx_posted(self.rx_buffers.device + (index * BUFFER) as u64);
            self.rings.write(place, address);
            self.rings.write(place + 8, header);
            tail = Some(self.rx.consume());
        }
        if let Some(tail) = tail {
            fence(Ordering::SeqCst);
            self.regs.write(reg::RDT, u32::from(tail));
        }
    }
}

/// Masks interrupts, stops DMA and resets the controller: the firmware may
/// have left it receiving into memory it no longer owns.
fn reset(log: Handle, regs: Registers, family: Family) {
    regs.update(reg::CTRL, reg::ctrl::GIO_MASTER_DISABLE, 0);
    if !oceans_nic::wait_for(MASTER_DISABLE_MS, || {
        regs.read(reg::STATUS) & reg::status::GIO_MASTER_ENABLE == 0
    }) {
        say(
            log,
            format_args!("bus-master requests still pending; resetting anyway"),
        );
    }
    regs.write(reg::EIMC, !0);
    regs.write(reg::EIAC, 0);
    regs.write(family.imc(), reg::int::ALL);
    regs.write(reg::RCTL, 0);
    regs.write(reg::TCTL, 1 << 3);
    let _ = regs.read(reg::STATUS);
    oceans_rt::sleep_ms(10);
    regs.update(reg::CTRL, family.reset_bit(), 0);
    oceans_rt::sleep_ms(10);
    // The NVM's settings loaded; not every controller says so (QEMU's
    // never does), and Intel's drivers go on regardless.
    oceans_nic::wait_for(AUTO_READ_MS, || {
        regs.read(reg::EECD) & reg::eecd::AUTO_READ_DONE != 0
    });
    regs.write(reg::EIMC, !0);
    regs.write(family.imc(), reg::int::ALL);
    let _ = regs.read(family.icr());
}

fn phy_command(regs: Registers, command: u32, register: u8) -> Result<u16, &'static str> {
    regs.write(reg::MDIC, command);
    let mut result = None;
    oceans_nic::wait_for(PHY_MS, || {
        result = mdic::result(regs.read(reg::MDIC), register);
        result.is_some()
    });
    match result {
        Some(Ok(value)) => Ok(value),
        Some(Err(())) => Err("PHY access failed"),
        None => Err("PHY access timed out"),
    }
}

/// Powers the PHY up and restarts autonegotiation.
fn restart_autonegotiation(regs: Registers, family: Family) -> Result<(), &'static str> {
    let address = family.phy_address();
    let control = phy_command(regs, mdic::read(address, mdic::CONTROL), mdic::CONTROL)?;
    let control = (control | mdic::AUTONEG_ENABLE | mdic::RESTART_AUTONEG) & !mdic::POWER_DOWN;
    phy_command(
        regs,
        mdic::write(address, mdic::CONTROL, control),
        mdic::CONTROL,
    )
    .map(drop)
}
