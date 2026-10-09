//! xhci: the USB host controller driver (xHCI 1.2, ADR-0032).
//!
//! An ordinary service holding exactly what `services.conf` grants: the
//! first xHCI controller (`grant = device-class:0c0330`), the console's
//! input and nothing else of it (`grant = console-input`), the endpoint it
//! serves (`provide = usb`) and a log.
//!
//! - Brings the controller up: takes it from the firmware (legacy
//!   handoff), resets it, and sets up the device context array,
//!   scratchpad buffers, the command ring and one event ring.
//! - Enumerates devices on the root ports and behind hubs (ADR-0033), at
//!   start and when plugged in: port reset, slot, address, descriptors,
//!   product string. Hubs are configured, their ports powered and watched
//!   through their status-change endpoint; low and full speed devices
//!   behind high-speed hubs go through the hub's transaction translator.
//! - Drives boot keyboards: configures the interrupt IN endpoint, keeps
//!   reports queued, and types their keys into the console (the same
//!   bytes as the PS/2 keyboard).
//! - Answers `LIST` on `usb` (for `lsusb`).
//! - Hands interfaces to class drivers (`CLAIM`, ADR-0034): mass storage,
//!   whose bulk transfers it moves, and pointers (ADR-0042), whose
//!   interrupt reports it queues for the class driver (`REPORTS`). It
//!   recognises a pointer by its boot mouse interface or by parsing the
//!   interface's report descriptor; it never interprets the reports.
//!
//! Its interrupt (MSI-X) is a notification bound to its endpoint, so one
//! thread serves clients and the controller. Other classes are enumerated
//! and listed but not driven here.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, compiler_fence};

use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_usb::Speed;
use oceans_usb::consumer;
use oceans_usb::descriptor::{self, Bulk, Configuration, Device};
use oceans_usb::hid::Keyboard;
use oceans_usb::hub;
use oceans_usb::pointer::{self, Layout};
use oceans_usb::request::Setup;
use oceans_usb::service::{self, Claim, Kind, Path, Record, ReportWriter};
use oceans_usb::storage;
use oceans_usb::xhci::{
    self, Consumer, Event, HubSlot, InputContext, Params, Producer, Slot, TRB_SIZE, Trb, cap,
    completion, endpoint_type, interrupter, op, port,
};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const PAGE: usize = 4096;
/// TRBs per ring: one page.
const RING_TRBS: u16 = (PAGE / TRB_SIZE) as u16;
/// Pages for the controller's own structures (4) and every device's
/// ([`Pages`]: 10).
const POOL_PAGES: usize = 4 + 12 * MAX_DEVICES;
const MAX_PORTS: usize = 32;
const MAX_DEVICES: usize = 16;
/// Interrupt reports kept queued per keyboard or pointer, and per hub.
const QUEUED_REPORTS: usize = 4;
const HUB_REPORTS: usize = 2;
/// Bytes per hub status-change transfer (15 ports need 2).
const HUB_REPORT_LEN: usize = 8;
/// Spacing of report buffers in a report page; the longest report.
const REPORT_STRIDE: usize = pointer::MAX_REPORT;
/// Pointers polled at once, and the reports kept for each between its
/// class driver's `REPORTS` calls (queues live in the controller, not in
/// every device: the stack is 64 KiB).
const MAX_POINTERS: usize = 4;
const POINTER_QUEUE: usize = 8;
/// The longest report descriptor read to identify a pointer.
const MAX_REPORT_DESCRIPTOR: usize = 1024;
/// HID interfaces looked at per device.
const MAX_HID_INTERFACES: usize = 4;

/// Notification bit: controller interrupt or polling tick.
const IRQ: u64 = 1;
const POLL_MS: u64 = 10;
const COMMAND_MS: u64 = 1000;
/// A bulk transfer; slow media may take seconds.
const BULK_MS: u64 = 10_000;
/// After a short packet, how long to wait for the transfer's last event.
const SHORT_TAIL_MS: u64 = 50;
/// Class drivers waiting for an interface.
const MAX_WATCHERS: usize = 4;
const TRANSFER_MS: u64 = 1000;

/// A HID interface whose report descriptor describes no pointer: not an
/// error worth logging.
const NOT_A_POINTER: &str = "not a pointer";

const EXIT_BAD_START: i64 = 2;
const EXIT_DEVICE: i64 = 3;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<200>::new();
    let _ = line.write_str("xhci: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// Memory-mapped registers.
#[derive(Clone, Copy)]
struct Registers(*mut u8);

// SAFETY (all accessors): `Registers` point into the mapped BAR, at offsets
// the controller's own capability registers give, checked against its size.
impl Registers {
    fn at(self, offset: usize) -> Self {
        Self(unsafe { self.0.add(offset) })
    }

    fn read8(self, offset: usize) -> u8 {
        unsafe { ptr::read_volatile(self.0.add(offset)) }
    }

    fn read32(self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile(self.0.add(offset).cast()) }
    }

    fn write32(self, offset: usize, value: u32) {
        unsafe { ptr::write_volatile(self.0.add(offset).cast(), value) }
    }

    fn write64(self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}

/// One page of DMA memory.
#[derive(Clone, Copy)]
struct Page {
    virt: *mut u8,
    phys: u64,
}

impl Page {
    fn zero(self) {
        // SAFETY: a page of the driver's DMA memory.
        unsafe { ptr::write_bytes(self.virt, 0, PAGE) }
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: a page of DMA memory owned by this driver; the controller
        // only touches it while a command or transfer that names it runs,
        // and the driver is single-threaded.
        unsafe { core::slice::from_raw_parts_mut(self.virt, PAGE) }
    }

    fn write64(&self, offset: usize, value: u64) {
        assert!(offset + 8 <= PAGE);
        // SAFETY: in bounds (checked).
        unsafe { ptr::write_volatile(self.virt.add(offset).cast::<u64>(), value) }
    }

    /// Writes a TRB: the dword holding the cycle bit last, so the
    /// controller never sees a half-written TRB as valid.
    fn write_trb(&self, index: u16, trb: Trb) {
        let base = usize::from(index) * TRB_SIZE;
        assert!(base + TRB_SIZE <= PAGE);
        // SAFETY: in bounds (checked); DMA memory, hence volatile.
        unsafe {
            let at = self.virt.add(base).cast::<u32>();
            for (i, dword) in trb.0.iter().enumerate().take(3) {
                ptr::write_volatile(at.add(i), *dword);
            }
            compiler_fence(Ordering::Release);
            ptr::write_volatile(at.add(3), trb.0[3]);
        }
    }

    /// Reads a TRB: the cycle dword first.
    fn read_trb(&self, index: u16) -> Trb {
        let base = usize::from(index) * TRB_SIZE;
        assert!(base + TRB_SIZE <= PAGE);
        // SAFETY: as for `write_trb`.
        unsafe {
            let at = self.virt.add(base).cast::<u32>();
            let control = ptr::read_volatile(at.add(3));
            compiler_fence(Ordering::Acquire);
            Trb([
                ptr::read_volatile(at),
                ptr::read_volatile(at.add(1)),
                ptr::read_volatile(at.add(2)),
                control,
            ])
        }
    }
}

/// A bump allocator over one DMA region.
struct Pool {
    dma: Dma,
    used: usize,
}

impl Pool {
    fn page(&mut self) -> Result<Page, &'static str> {
        if (self.used + 1) * PAGE > self.dma.len {
            return Err("out of DMA pages");
        }
        let offset = self.used * PAGE;
        self.used += 1;
        // SAFETY: inside the region (checked).
        let page = Page {
            virt: unsafe { self.dma.virt.add(offset) },
            phys: self.dma.device + offset as u64,
        };
        page.zero();
        Ok(page)
    }
}

/// A command or transfer ring of one page.
#[derive(Clone, Copy)]
struct Ring {
    page: Page,
    producer: Producer,
}

impl Ring {
    fn new(page: Page) -> Self {
        Self {
            page,
            producer: Producer::new(RING_TRBS),
        }
    }

    /// Queues `trb`; returns its device address.
    fn push(&mut self, trb: Trb) -> u64 {
        let page = self.page;
        let index = self
            .producer
            .push(trb, Trb::link(page.phys), |i, t| page.write_trb(i, t));
        page.phys + u64::from(index) * TRB_SIZE as u64
    }

    /// The TRB index of a device address inside this ring.
    fn index_of(&self, address: u64) -> Option<usize> {
        let offset = address.checked_sub(self.page.phys)? as usize;
        (offset < PAGE && offset.is_multiple_of(TRB_SIZE)).then_some(offset / TRB_SIZE)
    }
}

/// The DMA pages a device uses; kept for the next device in the same table
/// entry after an unplug.
#[derive(Clone, Copy)]
struct Pages {
    output: Page,
    input: Page,
    control: Page,
    buffer: Page,
    reports: Page,
    interrupt: Page,
    bulk_in: Page,
    bulk_out: Page,
    /// A pointer's interrupt ring and its report buffers (ADR-0042).
    pointer: Page,
    pointer_reports: Page,
    /// Media keys' interrupt ring and report buffers (ADR-0102).
    media: Page,
    media_reports: Page,
}

impl Pages {
    fn all(&self) -> [Page; 12] {
        [
            self.output,
            self.input,
            self.control,
            self.buffer,
            self.reports,
            self.interrupt,
            self.bulk_in,
            self.bulk_out,
            self.pointer,
            self.pointer_reports,
            self.media,
            self.media_reports,
        ]
    }
}

/// A configured bulk interface's transfer rings.
#[derive(Clone, Copy)]
struct BulkRings {
    in_dci: u8,
    out_dci: u8,
    in_ring: Ring,
    out_ring: Ring,
}

/// An interface handed to a class driver (ADR-0034).
struct ClaimState {
    badge: u64,
    /// The class driver's buffer, mapped here.
    buffer: *mut u8,
    size: usize,
    notification: Handle,
    bits: u64,
    /// The device's pointer interface was claimed, not its storage.
    pointer: bool,
}

/// A HID interface recognised as a pointer (ADR-0042).
#[derive(Clone, Copy)]
struct PointerInterface {
    hid: descriptor::HidInterface,
    /// Positions (a tablet), not motion (a mouse).
    absolute: bool,
}

/// Reports a pointer sent that its class driver has not fetched yet.
struct ReportQueue {
    reports: [(u64, u8, [u8; pointer::MAX_REPORT]); POINTER_QUEUE],
    head: usize,
    len: usize,
    /// Dropped because the queue was full, since the last fetch.
    lost: u8,
}

impl ReportQueue {
    const fn new() -> Self {
        Self {
            reports: [(0, 0, [0; pointer::MAX_REPORT]); POINTER_QUEUE],
            head: 0,
            len: 0,
            lost: 0,
        }
    }

    /// Keeps a report; a full queue drops its oldest.
    fn push(&mut self, time_ms: u64, report: &[u8]) {
        if self.len == POINTER_QUEUE {
            self.head = (self.head + 1) % POINTER_QUEUE;
            self.len -= 1;
            self.lost = self.lost.saturating_add(1);
        }
        let entry = &mut self.reports[(self.head + self.len) % POINTER_QUEUE];
        let len = report.len().min(pointer::MAX_REPORT);
        entry.0 = time_ms;
        entry.1 = len as u8;
        entry.2[..len].copy_from_slice(&report[..len]);
        self.len += 1;
    }

    /// Moves reports into a `REPORTS` reply, oldest first, as many as fit.
    fn drain(&mut self, reply: &mut [u8]) -> usize {
        let mut writer = ReportWriter::new(reply, self.lost);
        self.lost = 0;
        while self.len > 0 {
            let (time, len, bytes) = &self.reports[self.head];
            if !writer.push(*time, &bytes[..usize::from(*len)]) {
                break;
            }
            self.head = (self.head + 1) % POINTER_QUEUE;
            self.len -= 1;
        }
        writer.len()
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.lost = 0;
    }
}

/// A pointer's interrupt endpoint, polled from the first `REPORTS` until
/// the device goes.
/// A media-keys interface's interrupt endpoint (ADR-0102).
struct MediaRing {
    interrupt: Interrupt,
    packet: usize,
    keys: consumer::Keys,
}

struct PointerRing {
    interrupt: Interrupt,
    /// Bytes per report transfer: the endpoint's packet size, at most
    /// [`REPORT_STRIDE`].
    packet: usize,
    /// Its queue in [`Controller::pointer_queues`].
    queue: usize,
}

/// A class driver waiting for an interface of `class`.
struct Watcher {
    class: (u8, u8, u8),
    notification: Handle,
    bits: u64,
}

/// An interrupt IN endpoint with reports kept queued.
struct Interrupt {
    dci: u8,
    ring: Ring,
}

enum Driver {
    None,
    /// A boot keyboard typing into the console.
    Keyboard {
        interrupt: Interrupt,
        hid: Keyboard,
    },
    /// A hub: its status-change endpoint and port count.
    Hub {
        interrupt: Interrupt,
        ports: u8,
        usb3: bool,
    },
}

struct UsbDevice {
    root_port: u8,
    route: u32,
    /// Hubs between the root port and the device.
    depth: u8,
    /// The hub (device index) and port the device is on.
    parent: Option<(usize, u8)>,
    slot: u8,
    speed: Speed,
    tt: Option<(u8, u8)>,
    hub_slot: Option<HubSlot>,
    descriptor: Device,
    name: [u8; service::MAX_NAME],
    name_len: usize,
    kind: Kind,
    control: Ring,
    max_packet0: u16,
    driver: Driver,
    configuration: u8,
    /// `SET_CONFIGURATION` was sent.
    configured: bool,
    /// A mass storage (BOT) interface, if the device has one.
    storage: Option<(descriptor::Interface, Bulk, Bulk)>,
    bulk: Option<BulkRings>,
    /// A pointer interface (ADR-0042), if the device has one.
    pointer: Option<PointerInterface>,
    pointer_ring: Option<PointerRing>,
    /// Media keys (HID consumer controls, ADR-0102), read here like the
    /// keyboard's.
    media: Option<MediaRing>,
    claim: Option<ClaimState>,
}

impl UsbDevice {
    fn path(&self) -> Path {
        Path {
            port: self.root_port,
            route: self.route,
        }
    }

    /// The slot context's Context Entries when `adding` is configured too:
    /// the highest DCI in use (xHCI §6.2.2).
    fn context_entries(&self, adding: u8) -> u8 {
        let driver = match &self.driver {
            Driver::Keyboard { interrupt, .. } | Driver::Hub { interrupt, .. } => interrupt.dci,
            Driver::None => 1,
        };
        let bulk = self.bulk.map_or(1, |b| b.in_dci.max(b.out_dci));
        let pointer = self.pointer_ring.as_ref().map_or(1, |p| p.interrupt.dci);
        adding.max(driver).max(bulk).max(pointer)
    }

    fn slot_context(&self, last_dci: u8) -> Slot {
        Slot {
            speed: self.speed,
            root_port: self.root_port,
            route: self.route,
            last_dci,
            tt: self.tt,
            multi_tt: false,
            hub: self.hub_slot,
        }
    }
}

/// Where a new device is, before it has a slot.
struct Location {
    root_port: u8,
    route: u32,
    depth: u8,
    parent: Option<(usize, u8)>,
    speed: Speed,
    tt: Option<(u8, u8)>,
}

struct Controller {
    log: Handle,
    console: Option<Handle>,
    operational: Registers,
    runtime: Registers,
    doorbells: Registers,
    params: Params,
    pool: Pool,
    dcbaa: Page,
    commands: Ring,
    event_page: Page,
    events: Consumer,
    pages: [Option<Pages>; MAX_DEVICES],
    devices: [Option<UsbDevice>; MAX_DEVICES],
    /// Root ports whose status changed while the driver was busy.
    pending_ports: u32,
    /// Per hub (device index): ports whose status changed.
    pending_hubs: [u32; MAX_DEVICES],
    notification: Handle,
    interrupts: bool,
    /// The controller's device capability (for more DMA memory).
    device: Handle,
    /// The `usb` endpoint (session handles are minted from it).
    server: Handle,
    next_badge: u64,
    /// DMA memory bulk transfers pass through.
    bounce: Option<Dma>,
    watchers: [Option<Watcher>; MAX_WATCHERS],
    pointer_queues: [ReportQueue; MAX_POINTERS],
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
    let console = directory.find_kind("console-input");
    if console.is_none() {
        say(
            log,
            format_args!("no console-input grant: keyboards will be listed, not used"),
        );
    }
    let mut controller = match Controller::start(log, device, console) {
        Ok(controller) => controller,
        Err(problem) => {
            say(log, format_args!("{problem}"));
            return EXIT_DEVICE;
        }
    };
    say(
        log,
        format_args!(
            "{} ports, {} slots, {}",
            controller.params.max_ports,
            controller.params.max_slots,
            if controller.interrupts {
                "MSI-X"
            } else {
                "polling"
            }
        ),
    );
    for number in 1..=controller.params.max_ports {
        controller.port_changed(number);
    }
    controller.service_events();
    controller.serve(server)
}

fn wait(mut done: impl FnMut() -> bool, timeout_ms: u64) -> bool {
    let deadline = oceans_rt::clock_ms() + timeout_ms;
    loop {
        for _ in 0..1000 {
            if done() {
                return true;
            }
            core::hint::spin_loop();
        }
        if oceans_rt::clock_ms() > deadline {
            return done();
        }
        oceans_rt::sleep_ms(1);
    }
}

impl Controller {
    fn start(log: Handle, device: Handle, console: Option<Handle>) -> Result<Self, &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the controller")?;
        let (memory, size) = oceans_rt::device_bar(device, 0).map_err(|_| "cannot get BAR 0")?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
            .map_err(|_| "cannot map BAR 0")?;
        let _ = oceans_rt::close(memory);
        let capability = Registers(base);
        let size = size as usize;
        let cap_length = usize::from(capability.read8(cap::CAPLENGTH));
        let runtime_offset = (capability.read32(cap::RTSOFF) & !0x1f) as usize;
        let doorbell_offset = (capability.read32(cap::DBOFF) & !0x3) as usize;
        let params = Params::decode(
            capability.read32(cap::HCSPARAMS1),
            capability.read32(cap::HCSPARAMS2),
            capability.read32(cap::HCCPARAMS1),
        );
        let ac64 = capability.read32(cap::HCCPARAMS1) & 1 != 0;
        let ports = usize::from(params.max_ports).min(MAX_PORTS);
        let in_bar =
            |offset: usize, len: usize| offset.checked_add(len).is_some_and(|end| end <= size);
        if !in_bar(cap_length, op::PORTS + 0x10 * ports)
            || !in_bar(runtime_offset, 0x40)
            || !in_bar(doorbell_offset, 4 * (usize::from(params.max_slots) + 1))
            || params.max_slots == 0
        {
            return Err("register layout outside BAR 0");
        }
        let operational = capability.at(cap_length);
        legacy_handoff(log, capability, params.extended_capabilities, size);

        // Halt, then reset.
        let usbcmd = operational.read32(op::USBCMD);
        operational.write32(op::USBCMD, usbcmd & !op::CMD_RUN);
        if !wait(|| operational.read32(op::USBSTS) & op::STS_HALTED != 0, 100) {
            return Err("the controller does not halt");
        }
        operational.write32(op::USBCMD, op::CMD_RESET);
        if !wait(
            || {
                operational.read32(op::USBCMD) & op::CMD_RESET == 0
                    && operational.read32(op::USBSTS) & op::STS_NOT_READY == 0
            },
            1000,
        ) {
            return Err("the controller does not finish its reset");
        }
        if operational.read32(op::PAGESIZE) & 1 == 0 {
            return Err("the controller does not support 4 KiB pages");
        }

        let dma = Dma::new(device, POOL_PAGES * PAGE)?;
        if !ac64 && dma.device + dma.len as u64 > 1 << 32 {
            return Err("DMA memory above 4 GiB for a 32-bit controller");
        }
        let mut pool = Pool { dma, used: 0 };
        let dcbaa = pool.page()?;
        if params.scratchpads > 0 {
            let count = usize::from(params.scratchpads);
            let scratch = Dma::new(device, (count + 1) * PAGE)?;
            let array = Page {
                virt: scratch.virt,
                phys: scratch.device,
            };
            array.zero();
            for i in 0..count.min(PAGE / 8) {
                array.write64(i * 8, scratch.device + ((i + 1) * PAGE) as u64);
            }
            dcbaa.write64(0, array.phys);
        }
        operational.write32(op::CONFIG, u32::from(params.max_slots));
        operational.write64(op::DCBAAP, dcbaa.phys);

        let commands = Ring::new(pool.page()?);
        operational.write64(op::CRCR, commands.page.phys | 1);

        // One event ring segment, described by a one-entry table.
        let event_page = pool.page()?;
        let table = pool.page()?;
        table.write64(0, event_page.phys);
        table.write64(8, u64::from(RING_TRBS));
        let primary = Registers(base).at(runtime_offset + 0x20);
        primary.write32(interrupter::ERSTSZ, 1);
        primary.write64(interrupter::ERDP, event_page.phys);
        primary.write64(interrupter::ERSTBA, table.phys);
        // At most one interrupt per 250 µs (in 250 ns units).
        primary.write32(interrupter::IMOD, 1000);
        primary.write32(
            interrupter::IMAN,
            interrupter::IMAN_ENABLE | interrupter::IMAN_PENDING,
        );

        let notification =
            oceans_rt::notification_create().map_err(|_| "cannot create a notification")?;
        let interrupts = oceans_rt::device_irq(device, 0, notification, IRQ).is_ok();

        operational.write32(op::USBCMD, op::CMD_RUN | op::CMD_INTERRUPTS);
        if !wait(|| operational.read32(op::USBSTS) & op::STS_HALTED == 0, 100) {
            return Err("the controller does not start");
        }
        Ok(Self {
            log,
            console,
            operational,
            runtime: Registers(base).at(runtime_offset),
            doorbells: Registers(base).at(doorbell_offset),
            params,
            pool,
            dcbaa,
            commands,
            event_page,
            events: Consumer::new(RING_TRBS),
            pages: [None; MAX_DEVICES],
            devices: [const { None }; MAX_DEVICES],
            pending_ports: 0,
            pending_hubs: [0; MAX_DEVICES],
            notification,
            interrupts,
            device,
            server: Handle(0),
            next_badge: 1,
            bounce: None,
            watchers: [const { None }; MAX_WATCHERS],
            pointer_queues: [const { ReportQueue::new() }; MAX_POINTERS],
        })
    }

    // ---- Events ----------------------------------------------------------

    fn primary(&self) -> Registers {
        self.runtime.at(0x20)
    }

    fn next_event(&mut self) -> Option<Event> {
        let page = self.event_page;
        let trb = self.events.pop(|i| page.read_trb(i))?;
        let dequeue = page.phys + u64::from(self.events.index()) * TRB_SIZE as u64;
        self.primary()
            .write64(interrupter::ERDP, dequeue | interrupter::ERDP_BUSY);
        Some(Event::decode(&trb))
    }

    /// Waits for the event `matches` accepts; other events are handled
    /// (keyboard reports) or remembered (port changes) meanwhile.
    fn wait_event(
        &mut self,
        mut matches: impl FnMut(&Event) -> bool,
        timeout_ms: u64,
    ) -> Option<Event> {
        let deadline = oceans_rt::clock_ms() + timeout_ms;
        loop {
            for _ in 0..1000 {
                while let Some(event) = self.next_event() {
                    if matches(&event) {
                        return Some(event);
                    }
                    self.handle_event(event);
                }
                core::hint::spin_loop();
            }
            if oceans_rt::clock_ms() > deadline {
                return None;
            }
            oceans_rt::sleep_ms(1);
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Transfer {
                slot,
                endpoint,
                trb,
                code,
                residue,
            } => self.interrupt_report(slot, endpoint, trb, code, residue),
            Event::PortStatus { port } if (1..=32).contains(&port) => {
                self.pending_ports |= 1 << (port - 1);
            }
            Event::HostController { code } => {
                say(self.log, format_args!("host controller event, code {code}"));
            }
            _ => {}
        }
    }

    /// Everything the controller has reported since the last call, and the
    /// port changes it implies (on root ports and on hubs).
    fn service_events(&mut self) {
        let primary = self.primary();
        primary.write32(
            interrupter::IMAN,
            interrupter::IMAN_ENABLE | interrupter::IMAN_PENDING,
        );
        self.operational.write32(op::USBSTS, op::STS_EVENT);
        while let Some(event) = self.next_event() {
            self.handle_event(event);
        }
        if self.operational.read32(op::USBSTS) & op::STS_FATAL != 0 {
            say(
                self.log,
                format_args!("host system error: the controller stopped"),
            );
        }
        loop {
            if self.pending_ports != 0 {
                let index = self.pending_ports.trailing_zeros();
                self.pending_ports &= !(1 << index);
                self.port_changed(index as u8 + 1);
            } else if let Some(hub) = self.pending_hubs.iter().position(|&ports| ports != 0) {
                let port = self.pending_hubs[hub].trailing_zeros();
                self.pending_hubs[hub] &= !(1 << port);
                self.hub_port_changed(hub, port as u8);
            } else {
                break;
            }
        }
    }

    // ---- Commands and transfers -----------------------------------------

    fn command(&mut self, trb: Trb) -> Result<u8, &'static str> {
        let address = self.commands.push(trb);
        self.doorbells.write32(0, 0);
        match self.wait_event(
            |e| matches!(e, Event::Command { trb, .. } if *trb == address),
            COMMAND_MS,
        ) {
            Some(Event::Command {
                code: completion::SUCCESS,
                slot,
                ..
            }) => Ok(slot),
            Some(_) => Err("command failed"),
            None => Err("command timed out"),
        }
    }

    /// A control transfer on a device's endpoint 0 through its buffer page.
    /// Returns the bytes transferred.
    fn control(
        &mut self,
        index: usize,
        setup: Setup,
        out: Option<&mut [u8]>,
    ) -> Result<usize, &'static str> {
        let mut pages = self.pages[index].ok_or("no pages")?;
        let device = self.devices[index].as_mut().ok_or("no such device")?;
        let length = usize::from(setup.length).min(PAGE);
        let input = setup.is_in();
        let has_data = length > 0;
        if has_data && !input {
            return Err("OUT data stages are not used");
        }
        device
            .control
            .push(Trb::setup(setup.to_bytes(), has_data.then_some(input)));
        let data = has_data.then(|| {
            device
                .control
                .push(Trb::data(pages.buffer.phys, length as u32, input))
        });
        let status = device.control.push(Trb::status(!(has_data && input)));
        let slot = device.slot;
        self.doorbells.write32(4 * usize::from(slot), 1);
        let mut received = length;
        loop {
            let event = self.wait_event(
                |e| matches!(e, Event::Transfer { slot: s, endpoint: 1, .. } if *s == slot),
                TRANSFER_MS,
            );
            match event {
                Some(Event::Transfer {
                    trb, code, residue, ..
                }) => {
                    let ok = code == completion::SUCCESS || code == completion::SHORT_PACKET;
                    if !ok {
                        return Err(if code == completion::STALL {
                            "the device stalled a request"
                        } else {
                            "control transfer failed"
                        });
                    }
                    if Some(trb) == data {
                        received = length.saturating_sub(residue as usize);
                    }
                    if trb == status {
                        break;
                    }
                }
                _ => return Err("control transfer timed out"),
            }
        }
        if let Some(out) = out {
            let len = received.min(out.len());
            out[..len].copy_from_slice(&pages.buffer.bytes()[..len]);
            return Ok(len);
        }
        Ok(received)
    }

    // ---- Root ports ------------------------------------------------------

    fn portsc(&self, number: u8) -> u32 {
        self.operational
            .read32(op::PORTS + 0x10 * usize::from(number - 1))
    }

    fn set_portsc(&self, number: u8, value: u32) {
        self.operational
            .write32(op::PORTS + 0x10 * usize::from(number - 1), value);
    }

    fn child_of(&self, parent: Option<(usize, u8)>, root_port: u8) -> Option<usize> {
        self.devices.iter().position(|d| {
            d.as_ref().is_some_and(|d| {
                d.parent == parent && (parent.is_some() || d.root_port == root_port)
            })
        })
    }

    /// Reconciles a root port with what the driver knows: attaches a newly
    /// connected device, forgets a removed one.
    fn port_changed(&mut self, number: u8) {
        if number == 0 || usize::from(number) > MAX_PORTS || number > self.params.max_ports {
            return;
        }
        let status = self.portsc(number);
        // Acknowledge every change reported so far.
        self.set_portsc(number, port::write_value(status, status & port::CHANGES));
        let connected = status & port::CONNECTED != 0;
        match (connected, self.child_of(None, number)) {
            (true, None) => {
                if let Err(problem) = self.attach_root(number) {
                    say(self.log, format_args!("port {number}: {problem}"));
                }
            }
            (false, Some(index)) => self.detach(index, true),
            _ => {}
        }
    }

    fn attach_root(&mut self, number: u8) -> Result<(), &'static str> {
        // USB 2 ports enable only after a reset; USB 3 ports train by
        // themselves.
        let mut status = self.portsc(number);
        if status & port::ENABLED == 0 {
            self.set_portsc(number, port::write_value(status, port::RESET));
            if !wait(|| self.portsc(number) & port::RESET_CHANGE != 0, 500) {
                return Err("port reset timed out");
            }
            status = self.portsc(number);
            self.set_portsc(number, port::write_value(status, status & port::CHANGES));
            if status & port::ENABLED == 0 {
                return Err("the port did not enable");
            }
        }
        let speed = Speed::from_id(port::speed(status)).ok_or("unknown port speed")?;
        self.attach(Location {
            root_port: number,
            route: 0,
            depth: 0,
            parent: None,
            speed,
            tt: None,
        })
    }

    // ---- Hub ports -------------------------------------------------------

    /// Reconciles port `port` of the hub at `hub` with what the driver
    /// knows (ADR-0033).
    fn hub_port_changed(&mut self, hub: usize, port: u8) {
        let Some(Driver::Hub { ports, usb3, .. }) = self.devices[hub].as_ref().map(|d| &d.driver)
        else {
            return;
        };
        let (ports, usb3) = (*ports, *usb3);
        if port == 0 || port > ports {
            return;
        }
        let result = (|| {
            let status = self.hub_port_status(hub, port)?;
            for feature in status.changes(usb3) {
                self.control(hub, hub::clear_port_feature(port, feature), None)?;
            }
            match (status.connected(), self.child_of(Some((hub, port)), 0)) {
                (true, None) => self.attach_on_hub(hub, port, usb3),
                (false, Some(child)) => {
                    self.detach(child, true);
                    Ok(())
                }
                _ => Ok(()),
            }
        })();
        if let Err(problem) = result {
            let path = self.devices[hub].as_ref().map(UsbDevice::path);
            if let Some(path) = path {
                say(self.log, format_args!("hub {path}, port {port}: {problem}"));
            }
        }
    }

    fn hub_port_status(&mut self, hub: usize, port: u8) -> Result<hub::PortStatus, &'static str> {
        let mut bytes = [0u8; 4];
        let len = self.control(hub, hub::get_port_status(port), Some(&mut bytes))?;
        hub::PortStatus::parse(&bytes[..len]).ok_or("short port status")
    }

    fn attach_on_hub(&mut self, hub: usize, port: u8, usb3: bool) -> Result<(), &'static str> {
        self.control(
            hub,
            hub::set_port_feature(port, hub::feature::PORT_RESET),
            None,
        )?;
        let mut status = self.hub_port_status(hub, port)?;
        for _ in 0..50 {
            if status.reset_done() {
                break;
            }
            oceans_rt::sleep_ms(10);
            status = self.hub_port_status(hub, port)?;
        }
        if !status.reset_done() {
            return Err("port reset timed out");
        }
        for feature in status.changes(usb3) {
            self.control(hub, hub::clear_port_feature(port, feature), None)?;
        }
        if !status.enabled() {
            return Err("the port did not enable");
        }
        // Reset recovery (USB 2.0 §7.1.7.5).
        oceans_rt::sleep_ms(10);
        let speed = status.speed(usb3);
        let parent = self.devices[hub].as_ref().ok_or("hub gone")?;
        let route =
            hub::child_route(parent.route, parent.depth, port).ok_or("hubs nested too deep")?;
        // Low and full speed devices below a high-speed hub use its
        // transaction translator; deeper ones inherit it.
        let tt = match speed {
            Speed::Low | Speed::Full if parent.speed == Speed::High => Some((parent.slot, port)),
            Speed::Low | Speed::Full => parent.tt,
            _ => None,
        };
        let location = Location {
            root_port: parent.root_port,
            route,
            depth: parent.depth + 1,
            parent: Some((hub, port)),
            speed,
            tt,
        };
        self.attach(location)
    }

    // ---- Devices ---------------------------------------------------------

    /// Forgets a device and, first, everything behind it; `announce`
    /// logs it (not for a device that failed to enumerate).
    fn detach(&mut self, index: usize, announce: bool) {
        while let Some(child) = self.devices.iter().position(|d| {
            d.as_ref()
                .is_some_and(|d| d.parent.is_some_and(|(hub, _)| hub == index))
        }) {
            self.detach(child, announce);
        }
        self.release(index, true);
        if let Some(device) = self.devices[index].take() {
            let _ = self.command(Trb::disable_slot(device.slot));
            self.dcbaa.write64(usize::from(device.slot) * 8, 0);
            self.pending_hubs[index] = 0;
            if announce {
                say(
                    self.log,
                    format_args!("port {}: device removed", device.path()),
                );
            }
        }
    }

    fn attach(&mut self, location: Location) -> Result<(), &'static str> {
        let free = self
            .devices
            .iter()
            .position(Option::is_none)
            .ok_or("too many devices")?;
        let pages = match self.pages[free] {
            Some(pages) => pages,
            None => {
                let pages = Pages {
                    output: self.pool.page()?,
                    input: self.pool.page()?,
                    control: self.pool.page()?,
                    buffer: self.pool.page()?,
                    reports: self.pool.page()?,
                    interrupt: self.pool.page()?,
                    bulk_in: self.pool.page()?,
                    bulk_out: self.pool.page()?,
                    pointer: self.pool.page()?,
                    pointer_reports: self.pool.page()?,
                    media: self.pool.page()?,
                    media_reports: self.pool.page()?,
                };
                self.pages[free] = Some(pages);
                pages
            }
        };
        for page in pages.all() {
            page.zero();
        }
        let slot = self.command(Trb::enable_slot())?;
        if slot == 0 || slot > self.params.max_slots {
            return Err("the controller gave an invalid slot");
        }
        self.dcbaa.write64(usize::from(slot) * 8, pages.output.phys);
        self.pending_hubs[free] = 0;
        self.devices[free] = Some(UsbDevice {
            root_port: location.root_port,
            route: location.route,
            depth: location.depth,
            parent: location.parent,
            slot,
            speed: location.speed,
            tt: location.tt,
            hub_slot: None,
            descriptor: Device::default(),
            name: [0; service::MAX_NAME],
            name_len: 0,
            kind: Kind::Other,
            control: Ring::new(pages.control),
            max_packet0: location.speed.default_max_packet0(),
            driver: Driver::None,
            configuration: 0,
            configured: false,
            storage: None,
            bulk: None,
            pointer: None,
            pointer_ring: None,
            media: None,
            claim: None,
        });
        let result = self.enumerate(free, pages);
        match result {
            Ok(()) => self.announce(free),
            // Also forgets anything a half-configured hub found.
            Err(_) => self.detach(free, false),
        }
        result
    }

    fn enumerate(&mut self, index: usize, mut pages: Pages) -> Result<(), &'static str> {
        let size = self.params.context_size;
        let (slot, ring, max_packet0, context) = {
            let d = self.devices[index].as_ref().ok_or("gone")?;
            (d.slot, d.control, d.max_packet0, d.slot_context(1))
        };
        {
            let mut input = InputContext::new(pages.input.bytes(), size);
            input.add(0b11);
            input.slot(&context);
            input.endpoint(
                1,
                endpoint_type::CONTROL,
                max_packet0,
                0,
                ring.page.phys,
                true,
            );
        }
        self.command(Trb::address_device(pages.input.phys, slot))?;

        // The first 8 bytes give endpoint 0's real packet size.
        let mut head = [0u8; 8];
        self.control(
            index,
            Setup::get_descriptor(descriptor::DEVICE, 0, 0, 8),
            Some(&mut head),
        )?;
        let mut bytes = [0u8; Device::SIZE];
        bytes[..8].copy_from_slice(&head);
        bytes[0] = Device::SIZE as u8;
        let real = Device::parse(&bytes)
            .and_then(|d| d.max_packet0_bytes())
            .ok_or("bad device descriptor")?;
        if real != max_packet0 {
            {
                let mut input = InputContext::new(pages.input.bytes(), size);
                input.add(0b10);
                input.endpoint(1, endpoint_type::CONTROL, real, 0, ring.page.phys, true);
            }
            self.command(Trb::evaluate_context(pages.input.phys, slot))?;
            if let Some(d) = self.devices[index].as_mut() {
                d.max_packet0 = real;
            }
        }
        let len = self.control(
            index,
            Setup::get_descriptor(descriptor::DEVICE, 0, 0, Device::SIZE as u16),
            Some(&mut bytes),
        )?;
        let device = Device::parse(&bytes[..len]).ok_or("bad device descriptor")?;

        // The product name, if the device has one (failures are harmless).
        let mut name = [0u8; service::MAX_NAME];
        let mut name_len = 0;
        if device.product_name != 0 {
            let mut buffer = [0u8; 255];
            if let Ok(len) = self.control(
                index,
                Setup::get_descriptor(descriptor::STRING, 0, 0, 255),
                Some(&mut buffer),
            ) && let Some(language) = descriptor::first_language(&buffer[..len])
                && let Ok(len) = self.control(
                    index,
                    Setup::get_descriptor(descriptor::STRING, device.product_name, language, 255),
                    Some(&mut buffer),
                )
            {
                name_len = descriptor::string(&buffer[..len], &mut name).unwrap_or(0);
            }
        }

        // The first configuration.
        let mut header = [0u8; Configuration::SIZE];
        let len = self.control(
            index,
            Setup::get_descriptor(descriptor::CONFIGURATION, 0, 0, Configuration::SIZE as u16),
            Some(&mut header),
        )?;
        let configuration =
            Configuration::parse(&header[..len]).ok_or("bad configuration descriptor")?;
        let total = usize::from(configuration.total_length).min(1024);
        let mut full = [0u8; 1024];
        let len = self.control(
            index,
            Setup::get_descriptor(descriptor::CONFIGURATION, 0, 0, total as u16),
            Some(&mut full[..total]),
        )?;
        let full = &full[..len];

        let keyboard = descriptor::find_boot_interface(full, descriptor::HID_PROTOCOL_KEYBOARD);
        let hub_endpoint = find_hub_endpoint(full);
        if let Some(d) = self.devices[index].as_mut() {
            d.configuration = configuration.value;
        }
        let pointer = self.find_pointer(index, full);
        let kind = if keyboard.is_some() {
            Kind::Keyboard
        } else if let Some(pointer) = pointer {
            if pointer.absolute {
                Kind::Tablet
            } else {
                Kind::Mouse
            }
        } else if device.class == descriptor::CLASS_HUB && hub_endpoint.is_some() {
            Kind::Hub
        } else if descriptor::Items::new(full).any(|item| {
            matches!(item, descriptor::Item::Interface(i) if i.class == descriptor::CLASS_MASS_STORAGE)
        }) {
            Kind::Storage
        } else {
            Kind::Other
        };
        let path = {
            let d = self.devices[index].as_mut().ok_or("gone")?;
            d.descriptor = device;
            d.name = name;
            d.name_len = name_len;
            d.kind = kind;
            d.pointer = pointer;
            d.storage = descriptor::find_bulk_interface(
                full,
                storage::CLASS,
                storage::SUBCLASS_SCSI,
                storage::PROTOCOL_BOT,
            );
            d.path()
        };
        let speed = self.devices[index]
            .as_ref()
            .map_or(Speed::Full, |d| d.speed);
        let text = core::str::from_utf8(&name[..name_len]).unwrap_or("");
        let role = match kind {
            Kind::Keyboard if self.console.is_none() => "keyboard (no console-input grant)",
            other => other.describe(),
        };
        say(
            self.log,
            format_args!(
                "port {path}: {:04x}:{:04x} {text} ({}), {role}",
                device.vendor,
                device.product,
                speed.name(),
            ),
        );

        if let Some((interface, endpoint)) = keyboard
            && self.console.is_some()
        {
            self.configure_device(index)?;
            self.control(index, Setup::hid_set_boot_protocol(interface.number), None)?;
            // Optional for keyboards: some stall it.
            let _ = self.control(index, Setup::hid_set_idle(interface.number), None);
            let interrupt = self.configure_interrupt(
                index,
                endpoint,
                pages.interrupt,
                pages.reports,
                oceans_usb::hid::REPORT_SIZE,
                QUEUED_REPORTS,
            )?;
            if let Some(d) = self.devices[index].as_mut() {
                d.driver = Driver::Keyboard {
                    interrupt,
                    hid: Keyboard::new(),
                };
            }
        } else if kind == Kind::Hub
            && let Some(endpoint) = hub_endpoint
        {
            self.configure_hub(index, endpoint, pages)?;
        }
        // Media keys (ADR-0102): a keyboard's own interface for them, or a
        // device of nothing else. A failure leaves the rest working.
        if self.console.is_some()
            && kind != Kind::Hub
            && let Some((hid, layout)) =
                self.find_media(index, full, pointer.map(|p| p.hid.interface.number))
            && let Err(problem) = self.start_media(index, hid, layout)
        {
            say(self.log, format_args!("port {path}: media keys: {problem}"));
        }
        Ok(())
    }

    /// Selects the device's configuration, once.
    fn configure_device(&mut self, index: usize) -> Result<(), &'static str> {
        let d = self.devices[index].as_ref().ok_or("gone")?;
        if d.configured {
            return Ok(());
        }
        let value = d.configuration;
        self.control(index, Setup::set_configuration(value), None)?;
        if let Some(d) = self.devices[index].as_mut() {
            d.configured = true;
        }
        Ok(())
    }

    /// Configures an interrupt IN endpoint on `ring_page` and queues
    /// `queued` transfers of `report_len` bytes into `reports`.
    fn configure_interrupt(
        &mut self,
        index: usize,
        endpoint: descriptor::Endpoint,
        ring_page: Page,
        reports: Page,
        report_len: usize,
        queued: usize,
    ) -> Result<Interrupt, &'static str> {
        let mut pages = self.pages[index].ok_or("no pages")?;
        let (slot, speed, context) = {
            let d = self.devices[index].as_ref().ok_or("gone")?;
            (
                d.slot,
                d.speed,
                d.slot_context(d.context_entries(endpoint.dci())),
            )
        };
        let dci = endpoint.dci();
        let mut ring = Ring::new(ring_page);
        {
            let mut input = InputContext::new(pages.input.bytes(), self.params.context_size);
            input.add(1 | 1 << dci);
            input.slot(&context);
            input.endpoint(
                dci,
                endpoint_type::INTERRUPT_IN,
                endpoint.packet_size(),
                xhci::interrupt_interval(speed, endpoint.interval),
                ring.page.phys,
                true,
            );
        }
        self.command(Trb::configure_endpoint(pages.input.phys, slot))?;
        for _ in 0..queued {
            queue_report(&mut ring, reports, report_len);
        }
        self.doorbells
            .write32(4 * usize::from(slot), u32::from(dci));
        Ok(Interrupt { dci, ring })
    }

    /// Makes a device a hub (ADR-0033): reads its hub descriptor, tells
    /// the controller, powers its ports and looks at each.
    fn configure_hub(
        &mut self,
        index: usize,
        endpoint: descriptor::Endpoint,
        pages: Pages,
    ) -> Result<(), &'static str> {
        let (speed, depth) = {
            let d = self.devices[index].as_ref().ok_or("gone")?;
            (d.speed, d.depth)
        };
        let usb3 = matches!(speed, Speed::Super | Speed::SuperPlus);
        let mut bytes = [0u8; 71];
        let len = self.control(index, hub::get_descriptor(usb3), Some(&mut bytes))?;
        let descriptor = hub::Descriptor::parse(&bytes[..len], usb3).ok_or("bad hub descriptor")?;
        // Route strings name ports 1–15; the 32-bit change mask holds 31.
        let ports = descriptor.ports.min(15);
        self.configure_device(index)?;
        if usb3 {
            self.control(index, hub::set_hub_depth(depth), None)?;
        }
        if let Some(d) = self.devices[index].as_mut() {
            d.hub_slot = Some(HubSlot {
                ports,
                think_time: descriptor.think_time,
            });
        }
        let interrupt = self.configure_interrupt(
            index,
            endpoint,
            pages.interrupt,
            pages.reports,
            HUB_REPORT_LEN,
            HUB_REPORTS,
        )?;
        if let Some(d) = self.devices[index].as_mut() {
            d.driver = Driver::Hub {
                interrupt,
                ports,
                usb3,
            };
        }
        for port in 1..=ports {
            self.control(
                index,
                hub::set_port_feature(port, hub::feature::PORT_POWER),
                None,
            )?;
        }
        oceans_rt::sleep_ms(u64::from(descriptor.power_good_ms).max(100));
        for port in 1..=ports {
            self.hub_port_changed(index, port);
        }
        Ok(())
    }

    /// A completed interrupt transfer: keyboard keys go to the console, a
    /// hub's changed ports are noted, a pointer's report is queued for its
    /// class driver; the TRB is queued again.
    fn interrupt_report(&mut self, slot: u8, endpoint: u8, trb: u64, code: u8, residue: u32) {
        let Some(index) = self
            .devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.slot == slot))
        else {
            return;
        };
        let Some(pages) = self.pages[index] else {
            return;
        };
        let console = self.console;
        let Some(device) = self.devices[index].as_mut() else {
            return;
        };
        // Media keys (ADR-0102): their bytes go where the keyboard's do.
        if let Some(media) = device.media.as_mut()
            && media.interrupt.dci == endpoint
        {
            let Some(at) = media.interrupt.ring.index_of(trb) else {
                return;
            };
            let mut reports = pages.media_reports;
            let offset = at * REPORT_STRIDE % PAGE;
            let len = media.packet.saturating_sub(residue as usize);
            let mut report = [0u8; REPORT_STRIDE];
            report[..len].copy_from_slice(&reports.bytes()[offset..offset + len]);
            queue_report(&mut media.interrupt.ring, reports, media.packet);
            self.doorbells
                .write32(4 * usize::from(slot), u32::from(media.interrupt.dci));
            if code == completion::SUCCESS || code == completion::SHORT_PACKET {
                let mut pressed = [0u8; 8];
                let mut count = 0;
                media.keys.report(&report[..len], |byte| {
                    if count < pressed.len() {
                        pressed[count] = byte;
                        count += 1;
                    }
                });
                if count > 0
                    && let Some(console) = console
                {
                    let _ = oceans_rt::console_input(console, &pressed[..count]);
                }
            }
            return;
        }
        let (interrupt, mut reports, report_len) =
            match (&mut device.driver, &mut device.pointer_ring) {
                (_, Some(ring)) if ring.interrupt.dci == endpoint => {
                    (&mut ring.interrupt, pages.pointer_reports, ring.packet)
                }
                (Driver::Keyboard { interrupt, .. }, _) if interrupt.dci == endpoint => {
                    (interrupt, pages.reports, oceans_usb::hid::REPORT_SIZE)
                }
                (Driver::Hub { interrupt, .. }, _) if interrupt.dci == endpoint => {
                    (interrupt, pages.reports, HUB_REPORT_LEN)
                }
                _ => return,
            };
        let Some(at) = interrupt.ring.index_of(trb) else {
            return;
        };
        let offset = at * REPORT_STRIDE % PAGE;
        let ok = code == completion::SUCCESS || code == completion::SHORT_PACKET;
        let len = report_len.saturating_sub(residue as usize);
        let mut report = [0u8; REPORT_STRIDE];
        report[..len].copy_from_slice(&reports.bytes()[offset..offset + len]);
        queue_report(&mut interrupt.ring, reports, report_len);
        let dci = interrupt.dci;
        self.doorbells
            .write32(4 * usize::from(slot), u32::from(dci));
        if let Some(ring) = device.pointer_ring.as_ref()
            && ring.interrupt.dci == endpoint
        {
            if !ok {
                let path = device.path();
                say(
                    self.log,
                    format_args!("port {path}: pointer report failed, code {code}"),
                );
                return;
            }
            let queue = ring.queue;
            self.pointer_queues[queue].push(oceans_rt::clock_ms(), &report[..len]);
            if let Some(claim) = device.claim.as_ref().filter(|c| c.pointer) {
                let _ = oceans_rt::notification_signal(claim.notification, claim.bits);
            }
            return;
        }
        if !ok {
            return;
        }
        match &mut device.driver {
            Driver::Keyboard { hid, .. } => {
                let mut typed = [0u8; 6];
                let mut count = 0;
                hid.report(&report[..len], |byte| {
                    if count < typed.len() {
                        typed[count] = byte;
                        count += 1;
                    }
                });
                if count > 0
                    && let Some(console) = console
                {
                    let _ = oceans_rt::console_input(console, &typed[..count]);
                }
            }
            Driver::Hub { ports, .. } => {
                self.pending_hubs[index] |= hub::changed_ports(&report[..len], *ports);
            }
            Driver::None => {}
        }
    }

    /// The first HID interface of a configuration that is a pointer: a
    /// boot mouse, or an interface whose report descriptor describes a
    /// mouse or pointer (ADR-0042). Boot keyboards stay with this driver.
    fn find_pointer(&mut self, index: usize, configuration: &[u8]) -> Option<PointerInterface> {
        let mut hids = [descriptor::HidInterface::default(); MAX_HID_INTERFACES];
        let count = descriptor::find_hid_interfaces(configuration, &mut hids);
        for hid in hids.into_iter().take(count) {
            if hid.is_boot(descriptor::HID_PROTOCOL_KEYBOARD) {
                continue;
            }
            if hid.is_boot(descriptor::HID_PROTOCOL_MOUSE) {
                return Some(PointerInterface {
                    hid,
                    absolute: false,
                });
            }
            if hid.report_length == 0 {
                continue;
            }
            match self.report_layout(index, hid) {
                Ok(layout) => {
                    return Some(PointerInterface {
                        hid,
                        absolute: layout.absolute,
                    });
                }
                Err(problem) => {
                    let path = self.devices[index].as_ref().map(UsbDevice::path);
                    if let Some(path) = path
                        && problem != NOT_A_POINTER
                    {
                        say(
                            self.log,
                            format_args!(
                                "port {path}: interface {}: {problem}",
                                hid.interface.number
                            ),
                        );
                    }
                }
            }
        }
        None
    }

    /// Reads and parses a HID interface's report descriptor; the device
    /// is configured first (devices need not answer before).
    fn report_layout(
        &mut self,
        index: usize,
        hid: descriptor::HidInterface,
    ) -> Result<Layout, &'static str> {
        self.configure_device(index)?;
        let length = usize::from(hid.report_length).min(MAX_REPORT_DESCRIPTOR);
        let mut bytes = [0u8; MAX_REPORT_DESCRIPTOR];
        let len = self.control(
            index,
            Setup::hid_get_report_descriptor(hid.interface.number, length as u16),
            Some(&mut bytes[..length]),
        )?;
        Layout::parse(&bytes[..len]).map_err(|error| match error {
            pointer::Error::Malformed => "malformed report descriptor",
            pointer::Error::NotAPointer => NOT_A_POINTER,
        })
    }

    /// The first HID interface of a configuration with media keys (HID
    /// consumer controls, ADR-0102): not a boot keyboard or mouse, not the
    /// pointer, with a Consumer Control collection this driver knows keys
    /// of.
    fn find_media(
        &mut self,
        index: usize,
        configuration: &[u8],
        pointer: Option<u8>,
    ) -> Option<(descriptor::HidInterface, consumer::Layout)> {
        let mut hids = [descriptor::HidInterface::default(); MAX_HID_INTERFACES];
        let count = descriptor::find_hid_interfaces(configuration, &mut hids);
        for hid in hids.into_iter().take(count) {
            if hid.is_boot(descriptor::HID_PROTOCOL_KEYBOARD)
                || hid.is_boot(descriptor::HID_PROTOCOL_MOUSE)
                || hid.report_length == 0
                || Some(hid.interface.number) == pointer
            {
                continue;
            }
            if self.configure_device(index).is_err() {
                return None;
            }
            let length = usize::from(hid.report_length).min(MAX_REPORT_DESCRIPTOR);
            let mut bytes = [0u8; MAX_REPORT_DESCRIPTOR];
            let Ok(len) = self.control(
                index,
                Setup::hid_get_report_descriptor(hid.interface.number, length as u16),
                Some(&mut bytes[..length]),
            ) else {
                continue;
            };
            if let Ok(layout) = consumer::Layout::parse(&bytes[..len]) {
                return Some((hid, layout));
            }
        }
        None
    }

    /// Reads a device's media keys from now on (ADR-0102).
    fn start_media(
        &mut self,
        index: usize,
        hid: descriptor::HidInterface,
        layout: consumer::Layout,
    ) -> Result<(), &'static str> {
        let pages = self.pages[index].ok_or("no pages")?;
        // Optional (HID 1.11 §7.2.4): some devices stall it.
        let _ = self.control(index, Setup::hid_set_idle(hid.interface.number), None);
        let packet = usize::from(hid.endpoint.packet_size()).min(REPORT_STRIDE);
        let interrupt = self.configure_interrupt(
            index,
            hid.endpoint,
            pages.media,
            pages.media_reports,
            packet,
            QUEUED_REPORTS,
        )?;
        let device = self.devices[index].as_mut().ok_or("gone")?;
        device.media = Some(MediaRing {
            interrupt,
            packet,
            keys: consumer::Keys::new(layout),
        });
        let path = device.path();
        say(
            self.log,
            format_args!(
                "port {path}: interface {}: media keys",
                hid.interface.number
            ),
        );
        Ok(())
    }

    // ---- Class drivers (ADR-0034) ----------------------------------------

    /// Tells a waiting class driver about a newly enumerated interface it
    /// wants.
    fn announce(&mut self, index: usize) {
        let Some(device) = self.devices[index].as_ref() else {
            return;
        };
        let storage = device
            .storage
            .map(|(i, _, _)| (i.class, i.subclass, i.protocol));
        let pointer = device.pointer.map(|_| service::POINTER);
        for watcher in self.watchers.iter().flatten() {
            if Some(watcher.class) == storage || Some(watcher.class) == pointer {
                let _ = oceans_rt::notification_signal(watcher.notification, watcher.bits);
            }
        }
    }

    /// `CLAIM`: hands the first unclaimed matching interface to the
    /// caller, or remembers the caller's notification for when one comes.
    fn claim(
        &mut self,
        data: &[u8],
        received: &[Handle],
        kept: &mut [bool],
    ) -> (u64, [u8; Claim::SIZE], Option<Handle>) {
        let refuse = (service::BAD_REQUEST, [0u8; Claim::SIZE], None);
        if data.len() < 12 || received.len() != 2 {
            return refuse;
        }
        let class = (data[0], data[1], data[2]);
        let bits = u64::from_le_bytes(data[4..12].try_into().expect("8 bytes"));
        let pointer = class == service::POINTER;
        let storage = class
            == (
                storage::CLASS,
                storage::SUBCLASS_SCSI,
                storage::PROTOCOL_BOT,
            );
        if !(pointer || storage) || bits == 0 {
            return refuse;
        }
        let Ok(size) = oceans_rt::memory_size(received[0]).map(|s| s as usize) else {
            return refuse;
        };
        let smallest = if pointer {
            service::MIN_POINTER_BUFFER
        } else {
            service::MIN_BUFFER
        };
        if !(smallest..=service::MAX_BUFFER).contains(&size) {
            return refuse;
        }
        let notification = received[1];
        let found = self.devices.iter().position(|d| {
            d.as_ref().is_some_and(|d| {
                d.claim.is_none()
                    && if pointer {
                        d.pointer.is_some()
                    } else {
                        d.storage.is_some()
                    }
            })
        });
        let Some(index) = found else {
            // Keep the notification: one watcher per class driver.
            let slot = self
                .watchers
                .iter()
                .position(|w| w.as_ref().is_some_and(|w| w.class == class))
                .or_else(|| self.watchers.iter().position(Option::is_none));
            let Some(slot) = slot else {
                return refuse;
            };
            if let Some(old) = self.watchers[slot].replace(Watcher {
                class,
                notification,
                bits,
            }) {
                let _ = oceans_rt::close(old.notification);
            }
            kept[1] = true;
            return (service::NOT_FOUND, [0u8; Claim::SIZE], None);
        };
        let prepared = if pointer {
            self.configure_device(index)
        } else if self.devices[index]
            .as_ref()
            .is_some_and(|d| d.bulk.is_none())
        {
            self.configure_bulk(index)
        } else {
            Ok(())
        };
        if let Err(problem) = prepared {
            let path = self.devices[index].as_ref().map(UsbDevice::path);
            if let Some(path) = path {
                say(self.log, format_args!("port {path}: {problem}"));
            }
            return (service::IO_ERROR, [0u8; Claim::SIZE], None);
        }
        let Ok(base) = oceans_rt::memory_map(received[0], 0, prot::READ | prot::WRITE) else {
            return refuse;
        };
        let badge = self.next_badge;
        let Ok(session) = oceans_rt::endpoint_mint(self.server, badge) else {
            let _ = oceans_rt::memory_unmap(base);
            return refuse;
        };
        // A new claimant starts from an empty report queue.
        let queue = self.devices[index]
            .as_ref()
            .and_then(|d| d.pointer_ring.as_ref())
            .map(|ring| ring.queue);
        if pointer && let Some(queue) = queue {
            self.pointer_queues[queue].clear();
        }
        let Some(device) = self.devices[index].as_mut() else {
            let _ = oceans_rt::memory_unmap(base);
            let _ = oceans_rt::close(session);
            return refuse;
        };
        let mut claim = Claim {
            port: device.root_port,
            route: device.route,
            vendor: device.descriptor.vendor,
            product: device.descriptor.product,
            ..Claim::default()
        };
        let interface = match (pointer, device.pointer, device.storage) {
            (true, Some(found), _) => {
                claim.interrupt_in = found.hid.endpoint.address;
                claim.report_length = found.hid.report_length;
                found.hid.interface
            }
            (false, _, Some((interface, bulk_in, bulk_out))) => {
                claim.bulk_in = bulk_in.endpoint.address;
                claim.bulk_out = bulk_out.endpoint.address;
                interface
            }
            _ => {
                let _ = oceans_rt::memory_unmap(base);
                let _ = oceans_rt::close(session);
                return refuse;
            }
        };
        claim.interface = interface.number;
        claim.subclass = interface.subclass;
        claim.protocol = interface.protocol;
        self.next_badge += 1;
        kept[1] = true;
        device.claim = Some(ClaimState {
            badge,
            buffer: base,
            size,
            notification,
            bits,
            pointer,
        });
        say(
            self.log,
            format_args!(
                "port {}: interface {} handed to a class driver",
                device.path(),
                interface.number
            ),
        );
        (service::OK, claim.encode(), Some(session))
    }

    /// Selects the configuration and sets up the claimed interface's bulk
    /// endpoints.
    fn configure_bulk(&mut self, index: usize) -> Result<(), &'static str> {
        let mut pages = self.pages[index].ok_or("no pages")?;
        self.configure_device(index)?;
        let (slot, storage, context) = {
            let d = self.devices[index].as_ref().ok_or("gone")?;
            let (_, bulk_in, bulk_out) = d.storage.ok_or("no bulk interface")?;
            let last = bulk_in.endpoint.dci().max(bulk_out.endpoint.dci());
            (
                d.slot,
                d.storage.ok_or("no bulk interface")?,
                d.slot_context(d.context_entries(last)),
            )
        };
        let (_, bulk_in, bulk_out) = storage;
        let rings = BulkRings {
            in_dci: bulk_in.endpoint.dci(),
            out_dci: bulk_out.endpoint.dci(),
            in_ring: Ring::new(pages.bulk_in),
            out_ring: Ring::new(pages.bulk_out),
        };
        {
            let mut input = InputContext::new(pages.input.bytes(), self.params.context_size);
            input.add(1 | 1 << rings.in_dci | 1 << rings.out_dci);
            input.slot(&context);
            for (bulk, dci, ring, kind) in [
                (bulk_in, rings.in_dci, rings.in_ring, endpoint_type::BULK_IN),
                (
                    bulk_out,
                    rings.out_dci,
                    rings.out_ring,
                    endpoint_type::BULK_OUT,
                ),
            ] {
                input.endpoint(
                    dci,
                    kind,
                    bulk.endpoint.packet_size(),
                    0,
                    ring.page.phys,
                    true,
                );
                input.max_burst(dci, bulk.max_burst);
            }
        }
        self.command(Trb::configure_endpoint(pages.input.phys, slot))?;
        if let Some(d) = self.devices[index].as_mut() {
            d.bulk = Some(rings);
        }
        Ok(())
    }

    fn bounce(&mut self) -> Result<Dma, &'static str> {
        if let Some(dma) = self.bounce {
            return Ok(dma);
        }
        let dma = Dma::new(self.device, service::MAX_TRANSFER)?;
        self.bounce = Some(dma);
        Ok(dma)
    }

    /// The claimed device of a session badge.
    fn session(&self, badge: u64) -> Option<usize> {
        self.devices.iter().position(|d| {
            d.as_ref()
                .is_some_and(|d| d.claim.as_ref().is_some_and(|c| c.badge == badge))
        })
    }

    /// `BULK`: one transfer through the bounce buffer.
    fn bulk(&mut self, index: usize, address: u8, offset: usize, len: usize) -> Result<usize, u64> {
        let bounce = self.bounce().map_err(|_| service::IO_ERROR)?;
        let device = self.devices[index].as_mut().ok_or(service::GONE)?;
        let claim = device.claim.as_ref().ok_or(service::GONE)?;
        if claim.pointer {
            return Err(service::BAD_REQUEST);
        }
        let rings = device.bulk.as_mut().ok_or(service::BAD_REQUEST)?;
        let input = address & 0x80 != 0;
        let (dci, ring) = match device.storage {
            Some((_, i, _)) if input && i.endpoint.address == address => {
                (rings.in_dci, &mut rings.in_ring)
            }
            Some((_, _, o)) if !input && o.endpoint.address == address => {
                (rings.out_dci, &mut rings.out_ring)
            }
            _ => return Err(service::BAD_REQUEST),
        };
        if len == 0
            || len > service::MAX_TRANSFER
            || offset.checked_add(len).is_none_or(|end| end > claim.size)
        {
            return Err(service::BAD_REQUEST);
        }
        if !input {
            // SAFETY: `offset..offset + len` lies in the claimant's mapped
            // buffer (checked) and in the bounce buffer (len ≤ its size).
            unsafe { ptr::copy_nonoverlapping(claim.buffer.add(offset), bounce.virt, len) };
        }
        // One TRB per page, chained; only the last interrupts.
        let mut trbs = [(0u64, 0usize, 0usize); service::MAX_TRANSFER / PAGE];
        let count = len.div_ceil(PAGE);
        for (i, entry) in trbs.iter_mut().enumerate().take(count) {
            let start = i * PAGE;
            let chunk = (len - start).min(PAGE);
            let trb = Trb::normal(bounce.device + start as u64, chunk as u32);
            let trb = if i + 1 < count { trb.chained() } else { trb };
            *entry = (ring.push(trb), start, chunk);
        }
        let slot = device.slot;
        self.doorbells
            .write32(4 * usize::from(slot), u32::from(dci));
        let first = trbs[0].0;
        let last = trbs[count - 1].0;
        let in_transfer = |trb: u64| trbs[..count].iter().position(|t| t.0 == trb);
        let mut transferred = None;
        loop {
            let timeout = if transferred.is_some() {
                SHORT_TAIL_MS
            } else {
                BULK_MS
            };
            let event = self.wait_event(
                |e| matches!(e, Event::Transfer { slot: s, endpoint, trb, .. }
                    if *s == slot && *endpoint == dci && (in_transfer(*trb).is_some() || *trb == first)),
                timeout,
            );
            match event {
                Some(Event::Transfer {
                    trb, code, residue, ..
                }) => {
                    let at = in_transfer(trb).unwrap_or(0);
                    match code {
                        completion::SUCCESS | completion::SHORT_PACKET => {
                            let (_, start, chunk) = trbs[at];
                            let done = start + chunk.saturating_sub(residue as usize);
                            // The first short packet ends the data; the
                            // last TRB's event (if any) only confirms.
                            let total = *transferred.get_or_insert(done);
                            if trb == last {
                                transferred = Some(total);
                                break;
                            }
                        }
                        completion::STALL => {
                            self.recover(index, dci, true);
                            return Err(service::STALL);
                        }
                        _ => {
                            self.recover(index, dci, false);
                            return Err(service::IO_ERROR);
                        }
                    }
                }
                None if transferred.is_some() => break,
                None => {
                    self.recover(index, dci, false);
                    return Err(service::IO_ERROR);
                }
                Some(_) => {}
            }
        }
        let transferred = transferred.unwrap_or(0).min(len);
        if input {
            let claim = self.devices[index]
                .as_ref()
                .and_then(|d| d.claim.as_ref())
                .ok_or(service::GONE)?;
            // SAFETY: as above.
            unsafe { ptr::copy_nonoverlapping(bounce.virt, claim.buffer.add(offset), transferred) };
        }
        Ok(transferred)
    }

    /// Puts a bulk endpoint back in order after an error: a halted one is
    /// reset, a running one stopped, and either way the controller skips
    /// whatever is still queued.
    fn recover(&mut self, index: usize, dci: u8, halted: bool) {
        let Some(device) = self.devices[index].as_ref() else {
            return;
        };
        let slot = device.slot;
        let Some(rings) = device.bulk else {
            return;
        };
        let ring = if dci == rings.in_dci {
            rings.in_ring
        } else {
            rings.out_ring
        };
        let _ = self.command(if halted {
            Trb::reset_endpoint(slot, dci)
        } else {
            Trb::stop_endpoint(slot, dci)
        });
        let dequeue = ring.page.phys + u64::from(ring.producer.index()) * TRB_SIZE as u64;
        let _ = self.command(Trb::set_tr_dequeue(
            slot,
            dci,
            dequeue,
            ring.producer.cycle(),
        ));
    }

    /// `CLEAR_HALT`: the device's halt (CLEAR_FEATURE) and the
    /// controller's.
    fn clear_halt(&mut self, index: usize, address: u8) -> Result<(), u64> {
        let device = self.devices[index].as_ref().ok_or(service::GONE)?;
        if device.claim.as_ref().is_none_or(|c| c.pointer) {
            return Err(service::BAD_REQUEST);
        }
        let (_, bulk_in, bulk_out) = device.storage.ok_or(service::BAD_REQUEST)?;
        let rings = device.bulk.ok_or(service::BAD_REQUEST)?;
        let dci = if address == bulk_in.endpoint.address {
            rings.in_dci
        } else if address == bulk_out.endpoint.address {
            rings.out_dci
        } else {
            return Err(service::BAD_REQUEST);
        };
        let clear = Setup {
            request_type: 0x02,
            request: 1,
            value: 0,
            index: u16::from(address),
            length: 0,
        };
        self.control(index, clear, None)
            .map_err(|_| service::IO_ERROR)?;
        // Reset fails harmlessly on an endpoint that is not halted.
        self.recover(index, dci, true);
        Ok(())
    }

    /// `CONTROL`: a class request to the claimed interface only, or a
    /// pointer's report descriptor.
    fn session_control(&mut self, index: usize, data: &[u8]) -> Result<usize, u64> {
        if data.len() != 12 {
            return Err(service::BAD_REQUEST);
        }
        let setup = Setup {
            request_type: data[0],
            request: data[1],
            value: u16::from_le_bytes([data[2], data[3]]),
            index: u16::from_le_bytes([data[4], data[5]]),
            length: u16::from_le_bytes([data[6], data[7]]),
        };
        let offset = u32::from_le_bytes(data[8..12].try_into().expect("4 bytes")) as usize;
        let device = self.devices[index].as_ref().ok_or(service::GONE)?;
        let claim = device.claim.as_ref().ok_or(service::GONE)?;
        let interface = if claim.pointer {
            device.pointer.map(|p| p.hid.interface)
        } else {
            device.storage.map(|(interface, _, _)| interface)
        }
        .ok_or(service::BAD_REQUEST)?;
        // Class requests (type 1) to an interface (recipient 1): ours.
        let class_to_interface = setup.request_type & 0x7f == 0x21;
        let report_descriptor =
            claim.pointer && setup.is_report_descriptor_request(interface.number);
        let length = usize::from(setup.length);
        if !(class_to_interface || report_descriptor)
            || setup.index & 0xff != u16::from(interface.number)
            || (length > 0 && !setup.is_in())
            || length > PAGE
            || offset
                .checked_add(length)
                .is_none_or(|end| end > claim.size)
        {
            return Err(service::BAD_REQUEST);
        }
        let mut bytes = [0u8; PAGE];
        let len = match self.control(index, setup, Some(&mut bytes[..length])) {
            Ok(len) => len,
            Err("the device stalled a request") => return Err(service::STALL),
            Err(_) => return Err(service::IO_ERROR),
        };
        let claim = self.devices[index]
            .as_ref()
            .and_then(|d| d.claim.as_ref())
            .ok_or(service::GONE)?;
        // SAFETY: in the claimant's buffer (checked).
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), claim.buffer.add(offset), len) };
        Ok(len)
    }

    /// `REPORTS`: the pointer's queued reports. The first call starts
    /// polling its interrupt endpoint.
    fn reports(&mut self, index: usize, reply: &mut [u8]) -> Result<usize, u64> {
        let device = self.devices[index].as_ref().ok_or(service::GONE)?;
        if !device.claim.as_ref().is_some_and(|c| c.pointer) {
            return Err(service::BAD_REQUEST);
        }
        let found = device.pointer.ok_or(service::BAD_REQUEST)?;
        let queue = match device.pointer_ring.as_ref() {
            Some(ring) => ring.queue,
            None => self.start_pointer(index, found).map_err(|problem| {
                let path = self.devices[index].as_ref().map(UsbDevice::path);
                if let Some(path) = path {
                    say(self.log, format_args!("port {path}: {problem}"));
                }
                service::IO_ERROR
            })?,
        };
        let len = reply.len().min(service::MAX_REPORTS_REPLY);
        Ok(self.pointer_queues[queue].drain(&mut reply[..len]))
    }

    /// Configures a pointer's interrupt endpoint, gives it a report queue,
    /// and keeps reports coming.
    fn start_pointer(
        &mut self,
        index: usize,
        found: PointerInterface,
    ) -> Result<usize, &'static str> {
        let pages = self.pages[index].ok_or("no pages")?;
        let queue = (0..MAX_POINTERS)
            .find(|&queue| {
                !self.devices.iter().flatten().any(|d| {
                    d.pointer_ring
                        .as_ref()
                        .is_some_and(|ring| ring.queue == queue)
                })
            })
            .ok_or("too many pointers")?;
        let packet = usize::from(found.hid.endpoint.packet_size()).min(REPORT_STRIDE);
        let interrupt = self.configure_interrupt(
            index,
            found.hid.endpoint,
            pages.pointer,
            pages.pointer_reports,
            packet,
            QUEUED_REPORTS,
        )?;
        self.pointer_queues[queue].clear();
        let device = self.devices[index].as_mut().ok_or("gone")?;
        device.pointer_ring = Some(PointerRing {
            interrupt,
            packet,
            queue,
        });
        Ok(queue)
    }

    /// Ends a claim: the class driver closed its session or the device
    /// left (then the driver hears of it).
    fn release(&mut self, index: usize, gone: bool) {
        let Some(claim) = self.devices[index].as_mut().and_then(|d| d.claim.take()) else {
            return;
        };
        let _ = oceans_rt::memory_unmap(claim.buffer);
        if gone {
            let _ = oceans_rt::notification_signal(claim.notification, claim.bits);
        }
        let _ = oceans_rt::close(claim.notification);
    }

    // ---- Clients ---------------------------------------------------------

    fn serve(&mut self, server: Handle) -> i64 {
        if oceans_rt::endpoint_bind(server, self.notification).is_err() {
            say(
                self.log,
                format_args!("cannot bind the interrupt to the endpoint"),
            );
            return EXIT_DEVICE;
        }
        self.server = server;
        let mut data = [0u8; 16];
        let mut handles = [Handle(0); 4];
        loop {
            if !self.interrupts {
                let _ = oceans_rt::timer_set(self.notification, IRQ, POLL_MS);
            }
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                // The system is shutting down: init has gone.
                Err(oceans_rt::Error::PeerClosed) => return 0,
                Err(error) => {
                    say(self.log, format_args!("receive failed: {error:?}"));
                    return 4;
                }
            };
            let received = &handles[..got.handles_len];
            let mut kept = [false; 4];
            let mut reply = [0u8; service::MAX_RECORDS * service::RECORD_SIZE];
            let mut reply_handle = None;
            let (label, len) = if got.signals != 0 {
                self.service_events();
                (u64::MAX, 0)
            } else if got.closed {
                if let Some(index) = self.session(got.badge) {
                    self.release(index, false);
                }
                (u64::MAX, 0)
            } else if got.badge != 0 {
                self.session_request(got.badge, got.label, &data[..got.data_len], &mut reply)
            } else if got.label == service::CLAIM {
                let (label, claim, session) =
                    self.claim(&data[..got.data_len], received, &mut kept);
                reply[..Claim::SIZE].copy_from_slice(&claim);
                reply_handle = session;
                (label, if label == service::OK { Claim::SIZE } else { 0 })
            } else {
                self.list(got.label, &data[..got.data_len], &mut reply)
            };
            for (&handle, kept) in received.iter().zip(kept) {
                if !kept {
                    let _ = oceans_rt::close(handle);
                }
            }
            if label == u64::MAX {
                continue;
            }
            let handles_out: &[Handle] = match &reply_handle {
                Some(handle) => core::slice::from_ref(handle),
                None => &[],
            };
            if oceans_rt::ipc_reply_msg(label, &reply[..len], handles_out).is_err()
                && let Some(handle) = reply_handle
            {
                let _ = oceans_rt::close(handle);
            }
        }
    }

    /// Requests on a class driver's session.
    fn session_request(
        &mut self,
        badge: u64,
        label: u64,
        data: &[u8],
        reply: &mut [u8],
    ) -> (u64, usize) {
        let Some(index) = self.session(badge) else {
            return (service::GONE, 0);
        };
        if label == service::REPORTS {
            return match self.reports(index, reply) {
                Ok(len) => (service::OK, len),
                Err(label) => (label, 0),
            };
        }
        let result = match label {
            service::BULK if data.len() == 12 => {
                let offset = u32::from_le_bytes(data[4..8].try_into().expect("4 bytes")) as usize;
                let len = u32::from_le_bytes(data[8..12].try_into().expect("4 bytes")) as usize;
                self.bulk(index, data[0], offset, len)
            }
            service::CONTROL => self.session_control(index, data),
            service::CLEAR_HALT if data.len() == 1 => self.clear_halt(index, data[0]).map(|()| 0),
            _ => Err(service::BAD_REQUEST),
        };
        match result {
            Ok(count) => {
                reply[..4].copy_from_slice(&(count as u32).to_le_bytes());
                (service::OK, 4)
            }
            Err(label) => (label, 0),
        }
    }

    /// `LIST`, and anything unknown.
    fn list(&self, label: u64, data: &[u8], reply: &mut [u8]) -> (u64, usize) {
        {
            {
                match label {
                    service::LIST => {
                        // data: the number of records to skip (paging).
                        let skip = data.first().copied().map_or(0, usize::from);
                        let mut count = 0;
                        for device in self
                            .devices
                            .iter()
                            .flatten()
                            .skip(skip)
                            .take(service::MAX_RECORDS)
                        {
                            let record = Record {
                                port: device.root_port,
                                route: device.route,
                                speed: device.speed.id(),
                                kind: device.kind,
                                claimed: device.claim.is_some(),
                                vendor: device.descriptor.vendor,
                                product: device.descriptor.product,
                                name: device.name,
                                name_len: device.name_len as u8,
                            };
                            let at = count * service::RECORD_SIZE;
                            let slot: &mut [u8; service::RECORD_SIZE] = (&mut reply
                                [at..at + service::RECORD_SIZE])
                                .try_into()
                                .expect("record size");
                            record.encode(slot);
                            count += 1;
                        }
                        (service::OK, count * service::RECORD_SIZE)
                    }
                    _ => (service::BAD_REQUEST, 0),
                }
            }
        }
    }
}

/// A hub's status-change endpoint: the interrupt IN endpoint of its hub
/// interface.
fn find_hub_endpoint(configuration: &[u8]) -> Option<descriptor::Endpoint> {
    let mut in_hub = false;
    for item in descriptor::Items::new(configuration) {
        match item {
            descriptor::Item::Interface(interface) => {
                in_hub = interface.class == descriptor::CLASS_HUB && interface.alternate == 0;
            }
            descriptor::Item::Endpoint(endpoint)
                if in_hub
                    && endpoint.is_in()
                    && endpoint.transfer_type() == descriptor::INTERRUPT =>
            {
                return Some(endpoint);
            }
            _ => {}
        }
    }
    None
}

/// Queues one report transfer of `len` bytes; its buffer is chosen by the TRB's
/// index, so a completion's TRB address tells where its report is.
fn queue_report(ring: &mut Ring, reports: Page, len: usize) {
    let at = usize::from(ring.producer.index());
    ring.push(Trb::normal(
        reports.phys + (at * REPORT_STRIDE % PAGE) as u64,
        len.min(REPORT_STRIDE) as u32,
    ));
}

/// Takes the controller from the firmware (xHCI §4.22.1): sets the OS
/// semaphore and waits for the BIOS one to clear.
fn legacy_handoff(log: Handle, capability: Registers, first: usize, size: usize) {
    let mut offset = first;
    for _ in 0..64 {
        if offset == 0 || offset + 8 > size {
            return;
        }
        let header = capability.read32(offset);
        if header as u8 == xhci::EXT_LEGACY {
            if header & xhci::LEGACY_BIOS_OWNED != 0 {
                capability.write32(offset, header | xhci::LEGACY_OS_OWNED);
                if !wait(
                    || capability.read32(offset) & xhci::LEGACY_BIOS_OWNED == 0,
                    1000,
                ) {
                    say(
                        log,
                        format_args!("the firmware did not release the controller; taking it"),
                    );
                    capability.write32(
                        offset,
                        (header | xhci::LEGACY_OS_OWNED) & !xhci::LEGACY_BIOS_OWNED,
                    );
                }
            }
            // Turn off the firmware's SMIs.
            capability.write32(offset + 4, 0);
            return;
        }
        let next = ((header >> 8) & 0xff) as usize * 4;
        if next == 0 {
            return;
        }
        offset += next;
    }
}
