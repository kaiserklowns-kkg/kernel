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
//! - Enumerates devices on the root ports, at start and when plugged in:
//!   port reset, slot, address, descriptors, product string.
//! - Drives boot keyboards: configures the interrupt IN endpoint, keeps
//!   reports queued, and types their keys into the console (the same
//!   bytes as the PS/2 keyboard).
//! - Answers `LIST` on `usb` (for `lsusb`).
//!
//! Its interrupt (MSI-X) is a notification bound to its endpoint, so one
//! thread serves clients and the controller. Hubs and other classes are
//! enumerated and listed but not driven yet.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, compiler_fence};

use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_usb::Speed;
use oceans_usb::descriptor::{self, Configuration, Device};
use oceans_usb::hid::Keyboard;
use oceans_usb::request::Setup;
use oceans_usb::service::{self, Kind, Record};
use oceans_usb::xhci::{
    self, Consumer, Event, InputContext, Params, Producer, TRB_SIZE, Trb, cap, completion,
    endpoint_type, interrupter, op, port,
};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const PAGE: usize = 4096;
/// TRBs per ring: one page.
const RING_TRBS: u16 = (PAGE / TRB_SIZE) as u16;
/// Pages for the controller's own structures and every device's.
const POOL_PAGES: usize = 64;
const MAX_PORTS: usize = 32;
const MAX_DEVICES: usize = 8;
/// Interrupt reports kept queued per keyboard.
const QUEUED_REPORTS: usize = 4;
/// Spacing of report buffers in a keyboard's buffer page.
const REPORT_STRIDE: usize = 64;

/// Notification bit: controller interrupt or polling tick.
const IRQ: u64 = 1;
const POLL_MS: u64 = 10;
const COMMAND_MS: u64 = 1000;
const TRANSFER_MS: u64 = 1000;

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

/// The DMA pages a port's device uses; kept across unplug and replug.
#[derive(Clone, Copy)]
struct Pages {
    output: Page,
    input: Page,
    control: Page,
    buffer: Page,
    reports: Page,
    interrupt: Page,
}

struct KeyboardDriver {
    dci: u8,
    ring: Ring,
    hid: Keyboard,
}

struct UsbDevice {
    port: u8,
    slot: u8,
    speed: Speed,
    descriptor: Device,
    name: [u8; service::MAX_NAME],
    name_len: usize,
    kind: Kind,
    control: Ring,
    max_packet0: u16,
    keyboard: Option<KeyboardDriver>,
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
    pages: [Option<Pages>; MAX_PORTS],
    devices: [Option<UsbDevice>; MAX_DEVICES],
    /// Ports whose status changed while the driver was busy.
    pending_ports: u32,
    notification: Handle,
    interrupts: bool,
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
            pages: [None; MAX_PORTS],
            devices: [const { None }; MAX_DEVICES],
            pending_ports: 0,
            notification,
            interrupts,
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
            } => self.keyboard_report(slot, endpoint, trb, code, residue),
            Event::PortStatus { port } if (1..=32).contains(&port) => {
                self.pending_ports |= 1 << (port - 1);
            }
            Event::HostController { code } => {
                say(self.log, format_args!("host controller event, code {code}"));
            }
            _ => {}
        }
    }

    /// Everything the controller has reported since the last call.
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
        while self.pending_ports != 0 {
            let index = self.pending_ports.trailing_zeros();
            self.pending_ports &= !(1 << index);
            self.port_changed(index as u8 + 1);
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
        let device = self.devices[index].as_mut().ok_or("no such device")?;
        let mut pages = self.pages[usize::from(device.port) - 1].ok_or("no pages")?;
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

    // ---- Ports and enumeration -------------------------------------------

    fn portsc(&self, number: u8) -> u32 {
        self.operational
            .read32(op::PORTS + 0x10 * usize::from(number - 1))
    }

    fn set_portsc(&self, number: u8, value: u32) {
        self.operational
            .write32(op::PORTS + 0x10 * usize::from(number - 1), value);
    }

    fn device_on(&self, number: u8) -> Option<usize> {
        self.devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.port == number))
    }

    /// Reconciles a port with what the driver knows: attaches a newly
    /// connected device, forgets a removed one.
    fn port_changed(&mut self, number: u8) {
        if number == 0 || usize::from(number) > MAX_PORTS || number > self.params.max_ports {
            return;
        }
        let status = self.portsc(number);
        // Acknowledge every change reported so far.
        self.set_portsc(number, port::write_value(status, status & port::CHANGES));
        let connected = status & port::CONNECTED != 0;
        match (connected, self.device_on(number)) {
            (true, None) => {
                if let Err(problem) = self.attach(number) {
                    say(self.log, format_args!("port {number}: {problem}"));
                }
            }
            (false, Some(index)) => self.detach(index),
            _ => {}
        }
    }

    fn detach(&mut self, index: usize) {
        if let Some(device) = self.devices[index].take() {
            let _ = self.command(Trb::disable_slot(device.slot));
            self.dcbaa.write64(usize::from(device.slot) * 8, 0);
            say(
                self.log,
                format_args!("port {}: device removed", device.port),
            );
        }
    }

    fn attach(&mut self, number: u8) -> Result<(), &'static str> {
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
        let free = self
            .devices
            .iter()
            .position(Option::is_none)
            .ok_or("too many devices")?;
        let pages = match self.pages[usize::from(number) - 1] {
            Some(pages) => pages,
            None => {
                let pages = Pages {
                    output: self.pool.page()?,
                    input: self.pool.page()?,
                    control: self.pool.page()?,
                    buffer: self.pool.page()?,
                    reports: self.pool.page()?,
                    interrupt: self.pool.page()?,
                };
                self.pages[usize::from(number) - 1] = Some(pages);
                pages
            }
        };
        for page in [
            pages.output,
            pages.input,
            pages.control,
            pages.buffer,
            pages.reports,
            pages.interrupt,
        ] {
            page.zero();
        }
        let slot = self.command(Trb::enable_slot())?;
        if slot == 0 || slot > self.params.max_slots {
            return Err("the controller gave an invalid slot");
        }
        self.dcbaa.write64(usize::from(slot) * 8, pages.output.phys);
        let control = Ring::new(pages.control);
        let max_packet0 = speed.default_max_packet0();
        self.devices[free] = Some(UsbDevice {
            port: number,
            slot,
            speed,
            descriptor: Device::default(),
            name: [0; service::MAX_NAME],
            name_len: 0,
            kind: Kind::Other,
            control,
            max_packet0,
            keyboard: None,
        });
        let result = self.enumerate(free, pages);
        if result.is_err()
            && let Some(device) = self.devices[free].take()
        {
            let _ = self.command(Trb::disable_slot(device.slot));
            self.dcbaa.write64(usize::from(device.slot) * 8, 0);
        }
        result
    }

    fn enumerate(&mut self, index: usize, mut pages: Pages) -> Result<(), &'static str> {
        let size = self.params.context_size;
        let (slot, speed, port, ring, max_packet0) = {
            let d = self.devices[index].as_ref().ok_or("gone")?;
            (d.slot, d.speed, d.port, d.control, d.max_packet0)
        };
        {
            let mut input = InputContext::new(pages.input.bytes(), size);
            input.add(0b11);
            input.slot(speed, port, 1);
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
        let kind = if keyboard.is_some() {
            Kind::Keyboard
        } else if descriptor::find_boot_interface(full, descriptor::HID_PROTOCOL_MOUSE).is_some() {
            Kind::Mouse
        } else if device.class == descriptor::CLASS_HUB {
            Kind::Hub
        } else if descriptor::Items::new(full).any(|item| {
            matches!(item, descriptor::Item::Interface(i) if i.class == descriptor::CLASS_MASS_STORAGE)
        }) {
            Kind::Storage
        } else {
            Kind::Other
        };
        if let Some(d) = self.devices[index].as_mut() {
            d.descriptor = device;
            d.name = name;
            d.name_len = name_len;
            d.kind = kind;
        }
        let text = core::str::from_utf8(&name[..name_len]).unwrap_or("");
        let role = match kind {
            Kind::Keyboard if self.console.is_none() => "keyboard (no console-input grant)",
            other => other.describe(),
        };
        say(
            self.log,
            format_args!(
                "port {port}: {:04x}:{:04x} {text} ({}), {role}",
                device.vendor,
                device.product,
                speed.name(),
            ),
        );

        if let Some((interface, endpoint)) = keyboard
            && self.console.is_some()
        {
            self.control(index, Setup::set_configuration(configuration.value), None)?;
            self.control(index, Setup::hid_set_boot_protocol(interface.number), None)?;
            // Optional for keyboards: some stall it.
            let _ = self.control(index, Setup::hid_set_idle(interface.number), None);
            let dci = endpoint.dci();
            let mut interrupt = Ring::new(pages.interrupt);
            {
                let mut input = InputContext::new(pages.input.bytes(), size);
                input.add(1 | 1 << dci);
                input.slot(speed, port, dci);
                input.endpoint(
                    dci,
                    endpoint_type::INTERRUPT_IN,
                    endpoint.packet_size(),
                    xhci::interrupt_interval(speed, endpoint.interval),
                    interrupt.page.phys,
                    true,
                );
            }
            self.command(Trb::configure_endpoint(pages.input.phys, slot))?;
            for _ in 0..QUEUED_REPORTS {
                queue_report(&mut interrupt, pages.reports);
            }
            self.doorbells
                .write32(4 * usize::from(slot), u32::from(dci));
            if let Some(d) = self.devices[index].as_mut() {
                d.keyboard = Some(KeyboardDriver {
                    dci,
                    ring: interrupt,
                    hid: Keyboard::new(),
                });
            }
        }
        Ok(())
    }

    /// A report from a keyboard: its new keys go to the console, and the
    /// TRB is queued again.
    fn keyboard_report(&mut self, slot: u8, endpoint: u8, trb: u64, code: u8, residue: u32) {
        let Some(index) = self
            .devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.slot == slot))
        else {
            return;
        };
        let port = self.devices[index].as_ref().map_or(0, |d| d.port);
        let Some(mut pages) = self
            .pages
            .get(usize::from(port).wrapping_sub(1))
            .copied()
            .flatten()
        else {
            return;
        };
        let console = self.console;
        let Some(device) = self.devices[index].as_mut() else {
            return;
        };
        let Some(keyboard) = device.keyboard.as_mut() else {
            return;
        };
        if keyboard.dci != endpoint {
            return;
        }
        let Some(at) = keyboard.ring.index_of(trb) else {
            return;
        };
        let offset = at * REPORT_STRIDE % PAGE;
        if code == completion::SUCCESS || code == completion::SHORT_PACKET {
            let len = 8usize.saturating_sub(residue as usize);
            let mut report = [0u8; 8];
            report[..len].copy_from_slice(&pages.reports.bytes()[offset..offset + len]);
            let mut typed = [0u8; 6];
            let mut count = 0;
            keyboard.hid.report(&report[..len], |byte| {
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
        queue_report(&mut keyboard.ring, pages.reports);
        let dci = keyboard.dci;
        self.doorbells
            .write32(4 * usize::from(slot), u32::from(dci));
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
            for &handle in &handles[..got.handles_len] {
                let _ = oceans_rt::close(handle);
            }
            if got.signals != 0 {
                self.service_events();
                continue;
            }
            if got.closed {
                continue;
            }
            let mut reply = [0u8; service::MAX_RECORDS * service::RECORD_SIZE];
            let (label, len) = match got.label {
                service::LIST => {
                    let mut count = 0;
                    for device in self.devices.iter().flatten().take(service::MAX_RECORDS) {
                        let record = Record {
                            port: device.port,
                            speed: device.speed.id(),
                            kind: device.kind,
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
            };
            let _ = oceans_rt::ipc_reply_msg(label, &reply[..len], &[]);
        }
    }
}

/// Queues one 8-byte report transfer; its buffer is chosen by the TRB's
/// index, so a completion's TRB address tells where its report is.
fn queue_report(ring: &mut Ring, reports: Page) {
    let at = usize::from(ring.producer.index());
    ring.push(Trb::normal(
        reports.phys + (at * REPORT_STRIDE % PAGE) as u64,
        8,
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
