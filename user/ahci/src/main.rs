//! ahci: the block device driver for SATA disks on AHCI controllers
//! (ADR-0069).
//!
//! An ordinary service, like `nvme`. Its authority is exactly what init
//! grants in `services.conf`: one device capability (the first AHCI
//! controller, `grant = device-class:010601`), the endpoint it serves and a
//! log. Through the device capability it maps the controller's registers
//! (the `ABAR`, BAR 5) and allocates DMA memory for one port's command
//! list, received-FIS area and command table, and a bounce buffer. It
//! speaks the block protocol (`oceans-block-proto`) to clients, copying
//! between their session buffers and its bounce buffer, so no client ever
//! sees a physical address.
//!
//! - **One disk:** the first implemented port with a link up and a SATA
//!   disk signature is served. Only disks with 48-bit addressing and
//!   512-byte logical sectors are served; others are refused with a log
//!   line (and the next port is tried).
//! - **One command in flight** (slot 0), as for `nvme`.
//! - **Polling:** the kernel delivers only MSI-X (ADR-0021) and AHCI
//!   controllers offer MSI or pin interrupts, so completions are polled
//!   (`PxCI`, `PxIS`), yielding and sleeping a millisecond between looks.
//! - **Failures:** a device error restarts the port's command engine; a
//!   command that does not complete within 30 s resets the link
//!   (COMRESET); a port that cannot be brought back answers every request
//!   with `IoError`. A disk with a write cache is flushed on `FLUSH` and
//!   when the service stops.
//! - **No disk** (or a controller it cannot use): the driver logs it once
//!   and keeps running, answering every request with `IoError` (as
//!   `usb-storage` does without a stick), so init does not restart it in a
//!   loop.
//! - It never writes to a disk except on a client's `WRITE`.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use oceans_ahci::{
    AtaCommand, Capabilities, IDENTIFY_SIZE, Identity, LinkStatus, TaskFile, bohc, cap2, cmd, ghc,
    hba, is, layout, port, sctl, sig,
};
use oceans_block_proto::{Info, SECTOR_SIZE, Status, Transfer, op};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

/// The controller's registers are BAR 5 (`ABAR`).
const ABAR: u8 = 5;
/// Its register in the configuration space (BAR 5).
const ABAR_REGISTER: u16 = 0x24;
const PAGE_SIZE: usize = 4096;
/// The most one command moves.
const BOUNCE_SIZE: usize = 128 * 1024;
const BOUNCE_SECTORS: u32 = (BOUNCE_SIZE / SECTOR_SIZE) as u32;
const MAX_SESSIONS: usize = 16;
/// A command not completed by then resets the port.
const COMMAND_TIMEOUT_MS: u64 = 30_000;
/// The command list and FIS receive engines stop within 500 ms (AHCI
/// 1.3.1, 10.1.2).
const ENGINE_TIMEOUT_MS: u64 = 500;
/// A link comes up within this after COMRESET.
const LINK_TIMEOUT_MS: u64 = 1_000;
/// Staggered spin-up: links appear within this.
const SPIN_UP_SETTLE_MS: u64 = 20;
/// A disk clears BSY within this after a reset (it may be spinning up).
const READY_TIMEOUT_MS: u64 = 10_000;
/// The firmware gives the controller up within this (AHCI 10.6.3).
const HANDOFF_TIMEOUT_MS: u64 = 2_000;
/// COMRESET is held at least 1 ms.
const COMRESET_HOLD_MS: u64 = 2;

/// Exit codes.
const EXIT_BAD_START: i64 = 2;
const EXIT_RECEIVE: i64 = 3;

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
    let controller = match Controller::start(log, device) {
        Ok(controller) => controller,
        Err(problem) => {
            // Restarting would meet the same controller: say it once and
            // fail requests instead.
            say(
                log,
                format_args!("ahci: {problem}; requests fail with an I/O error"),
            );
            return serve(log, server, &mut None);
        }
    };
    let (major, minor, patch) = oceans_ahci::version(controller.regs.read32(hba::VS));
    let mut version = Buffer::<16>::new();
    let _ = write!(version, "{major}.{minor}");
    if patch != 0 {
        let _ = write!(version, ".{patch}");
    }
    say(
        log,
        format_args!(
            "ahci: AHCI {}, {} ports implemented, {} command slots, polling",
            version.as_str(),
            controller.implemented.count_ones(),
            controller.caps.slots
        ),
    );
    let mut disk = Disk::find(log, device, &controller);
    if disk.is_none() {
        say(
            log,
            format_args!("ahci: no SATA disk attached; requests fail with an I/O error"),
        );
    }
    let code = serve(log, server, &mut disk);
    if let Some(disk) = &mut disk {
        disk.shutdown();
    }
    code
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// Waits up to `timeout_ms` for `done`, sleeping a millisecond between
/// looks.
fn wait_for(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = oceans_rt::clock_ms() + timeout_ms;
    loop {
        if done() {
            return true;
        }
        if oceans_rt::clock_ms() > deadline {
            return false;
        }
        oceans_rt::sleep_ms(1);
    }
}

/// The controller's registers (the `ABAR`), mapped.
#[derive(Clone, Copy)]
struct Registers {
    base: *mut u8,
    size: usize,
}

// SAFETY (both accessors): `base` maps `size` bytes of the controller's
// register BAR read-write for the life of the process; every offset is
// checked against `size`, and registers are naturally aligned 32-bit
// words (every offset used is a multiple of 4).
impl Registers {
    fn read32(self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= self.size);
        unsafe { ptr::read_volatile(self.base.add(offset).cast()) }
    }

    fn write32(self, offset: usize, value: u32) {
        assert!(offset.is_multiple_of(4) && offset + 4 <= self.size);
        unsafe { ptr::write_volatile(self.base.add(offset).cast(), value) }
    }
}

/// The host bus adapter: its registers and what it offers.
struct Controller {
    regs: Registers,
    caps: Capabilities,
    /// Implemented ports (`PI`) whose registers lie inside the `ABAR`.
    implemented: u32,
}

impl Controller {
    fn start(log: Handle, device: Handle) -> Result<Self, &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let (memory, size) =
            oceans_rt::device_bar(device, ABAR).map_err(|_| "cannot get the ABAR (BAR 5)")?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let _ = oceans_rt::close(memory);
        let base = base.map_err(|_| "cannot map the ABAR")?;
        // The object is whole pages; a 2 KiB ABAR need not start one. Its
        // registers are at the BAR's offset in its page.
        let raw = oceans_rt::device_config_read(device, ABAR_REGISTER, 4)
            .map_err(|_| "cannot read the ABAR register")?;
        let within = (raw as usize) & (PAGE_SIZE - 1) & !0xf;
        if within >= size as usize {
            return Err("the ABAR is outside its mapping");
        }
        let regs = Registers {
            // SAFETY: `within` is inside the mapping (checked above).
            base: unsafe { base.add(within) },
            size: size as usize - within,
        };
        if regs.size < hba::PORTS {
            return Err("the ABAR is too small");
        }
        let caps = Capabilities::decode(regs.read32(hba::CAP));
        if regs.read32(hba::CAP2) & cap2::HANDOFF != 0 {
            let control = regs.read32(hba::BOHC);
            regs.write32(hba::BOHC, control | bohc::OS_OWNED);
            if !wait_for(HANDOFF_TIMEOUT_MS, || {
                regs.read32(hba::BOHC) & (bohc::BIOS_OWNED | bohc::BIOS_BUSY) == 0
            }) {
                say(
                    log,
                    format_args!("ahci: the firmware did not release the controller; taking it"),
                );
            }
        }
        // AHCI mode (before anything else), interrupts off: completions are
        // polled.
        let control = regs.read32(hba::GHC);
        regs.write32(hba::GHC, (control | ghc::AHCI_ENABLE) & !ghc::INTERRUPTS);
        let implemented = oceans_ahci::implemented_ports(regs.read32(hba::PI))
            .filter(|&index| hba::port(index) + hba::PORT_SIZE <= regs.size)
            .fold(0u32, |set, index| set | (1 << index));
        Ok(Self {
            regs,
            caps,
            implemented,
        })
    }

    fn port(&self, index: u32) -> Port {
        Port {
            regs: self.regs,
            base: hba::port(index),
            index,
        }
    }
}

/// One port's registers.
#[derive(Clone, Copy)]
struct Port {
    regs: Registers,
    base: usize,
    index: u32,
}

impl Port {
    fn read(self, register: usize) -> u32 {
        self.regs.read32(self.base + register)
    }

    fn write(self, register: usize, value: u32) {
        self.regs.write32(self.base + register, value);
    }

    fn link(self) -> LinkStatus {
        LinkStatus::decode(self.read(port::SSTS))
    }

    fn task_file(self) -> TaskFile {
        TaskFile::decode(self.read(port::TFD))
    }

    /// Clears the error and interrupt status (both write-1-to-clear).
    fn clear_errors(self) {
        self.write(port::SERR, u32::MAX);
        self.write(port::IS, u32::MAX);
    }

    /// Stops the command list engine (`ST`), waiting until it says so.
    fn stop_commands(self) -> Result<(), &'static str> {
        let command = self.read(port::CMD);
        if command & cmd::START != 0 {
            self.write(port::CMD, command & !cmd::START);
        }
        if wait_for(ENGINE_TIMEOUT_MS, || {
            self.read(port::CMD) & cmd::LIST_RUNNING == 0
        }) {
            Ok(())
        } else {
            Err("the command list does not stop")
        }
    }

    /// Stops both engines: the port is idle and its memory may change.
    fn stop(self) -> Result<(), &'static str> {
        self.stop_commands()?;
        let command = self.read(port::CMD);
        if command & cmd::FIS_RECEIVE != 0 {
            self.write(port::CMD, command & !cmd::FIS_RECEIVE);
        }
        if wait_for(ENGINE_TIMEOUT_MS, || {
            self.read(port::CMD) & cmd::FIS_RUNNING == 0
        }) {
            Ok(())
        } else {
            Err("FIS receive does not stop")
        }
    }

    /// Starts the command list once the disk is ready for commands.
    fn start_commands(self) -> Result<(), &'static str> {
        if !wait_for(READY_TIMEOUT_MS, || !self.task_file().is_busy()) {
            return Err("the disk stays busy");
        }
        let command = self.read(port::CMD);
        self.write(port::CMD, command | cmd::START);
        Ok(())
    }

    /// Resets the link (COMRESET) and restarts the port.
    fn reset(self) -> Result<(), &'static str> {
        self.stop_commands()?;
        let control = self.read(port::SCTL) & !sctl::DET_MASK;
        self.write(
            port::SCTL,
            control | sctl::DET_COMRESET | sctl::IPM_NO_PARTIAL_SLUMBER,
        );
        oceans_rt::sleep_ms(COMRESET_HOLD_MS);
        self.write(port::SCTL, control | sctl::IPM_NO_PARTIAL_SLUMBER);
        if !wait_for(LINK_TIMEOUT_MS, || {
            self.link().detection == LinkStatus::DEVICE_COMMUNICATING
        }) {
            return Err("the link does not come back");
        }
        self.clear_errors();
        self.start_commands()
    }
}

/// Why a command did not succeed.
#[derive(Clone, Copy)]
enum Failure {
    Timeout,
    /// The port's interrupt status and the device's task file.
    Device(u32, TaskFile),
}

/// Why a port is not served.
enum Refusal {
    /// It holds something other than a SATA disk (its signature).
    NotDisk(u32),
    Problem(&'static str),
}

impl From<&'static str> for Refusal {
    fn from(problem: &'static str) -> Self {
        Self::Problem(problem)
    }
}

/// The disk served: one port and its memory.
struct Disk {
    log: Handle,
    port: Port,
    /// The command list, received-FIS area and command table
    /// (`oceans_ahci::layout`).
    memory: Dma,
    bounce: Dma,
    info: Info,
    /// The flush command, for a disk with its write cache on.
    flush: Option<AtaCommand>,
    /// The port could not be brought back after a failure.
    failed: bool,
}

impl Disk {
    /// The first port holding a usable SATA disk, set up and identified.
    fn find(log: Handle, device: Handle, controller: &Controller) -> Option<Self> {
        let ports = || oceans_ahci::implemented_ports(controller.implemented);
        if controller.caps.staggered_spin_up {
            for index in ports() {
                let port = controller.port(index);
                let command = port.read(port::CMD);
                port.write(port::CMD, command | cmd::SPIN_UP | cmd::POWER_ON);
            }
            oceans_rt::sleep_ms(SPIN_UP_SETTLE_MS);
        }
        let mut memory = None;
        for index in ports() {
            let port = controller.port(index);
            let link = port.link();
            if link.is_empty() {
                continue;
            }
            if !link.is_up() {
                say(
                    log,
                    format_args!(
                        "ahci: port {index}: a device without a working link (SStatus {:#x})",
                        port.read(port::SSTS)
                    ),
                );
                continue;
            }
            let (page, bounce) = match memory {
                Some(allocated) => allocated,
                None => match allocate(device, &controller.caps) {
                    Ok(allocated) => *memory.insert(allocated),
                    Err(problem) => {
                        say(log, format_args!("ahci: {problem}"));
                        return None;
                    }
                },
            };
            match Self::open(log, port, page, bounce) {
                Ok((disk, identity)) => {
                    disk.describe(&identity, link);
                    return Some(disk);
                }
                Err(refusal) => {
                    // The memory is reused for the next port: this one
                    // must not touch it any more.
                    let stopped = port.stop();
                    match refusal {
                        Refusal::NotDisk(signature) => say(
                            log,
                            format_args!(
                                "ahci: port {index}: {}, not served",
                                sig::name(signature)
                            ),
                        ),
                        Refusal::Problem(problem) => {
                            say(log, format_args!("ahci: port {index}: {problem}"));
                        }
                    }
                    if let Err(problem) = stopped {
                        say(
                            log,
                            format_args!("ahci: port {index}: {problem}; giving up"),
                        );
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Sets port `port` up on `memory`, checks it holds a SATA disk and
    /// identifies it.
    fn open(
        log: Handle,
        port: Port,
        memory: Dma,
        bounce: Dma,
    ) -> Result<(Self, Identity), Refusal> {
        port.stop()?;
        // SAFETY: `virt` maps `len` bytes of our DMA memory, which no
        // engine uses while the port is stopped.
        unsafe { ptr::write_bytes(memory.virt, 0, memory.len) };
        let list = memory.device + layout::COMMAND_LIST as u64;
        let received = memory.device + layout::RECEIVED_FIS as u64;
        port.write(port::CLB, list as u32);
        port.write(port::CLBU, (list >> 32) as u32);
        port.write(port::FB, received as u32);
        port.write(port::FBU, (received >> 32) as u32);
        port.write(port::IE, 0);
        port.clear_errors();
        let command = port.read(port::CMD);
        port.write(port::CMD, command | cmd::FIS_RECEIVE);
        // The signature arrives with the disk's first register FIS, once
        // it is ready (and FIS receive is on).
        if port.start_commands().is_err() {
            port.reset()?;
        }
        let signature = port.read(port::SIG);
        if signature != sig::ATA {
            return Err(Refusal::NotDisk(signature));
        }
        let mut disk = Self {
            log,
            port,
            memory,
            bounce,
            info: Info {
                sectors: 0,
                sector_size: SECTOR_SIZE as u32,
                flags: 0,
            },
            flush: None,
            failed: false,
        };
        disk.execute(AtaCommand::identify())
            .map_err(|_| "IDENTIFY DEVICE failed")?;
        // SAFETY: the bounce buffer maps at least IDENTIFY_SIZE bytes of
        // our DMA memory; the disk wrote it during the completed command.
        let data = unsafe { core::slice::from_raw_parts(bounce.virt, IDENTIFY_SIZE) };
        let identity = Identity::parse(data).ok_or("unreadable identify data")?;
        if !identity.ata {
            return Err("not an ATA device".into());
        }
        if !identity.lba48 {
            return Err("the disk lacks 48-bit addressing, not served".into());
        }
        if identity.logical_sector_size != SECTOR_SIZE as u32 {
            say(
                log,
                format_args!(
                    "ahci: port {}: {}-byte logical sectors",
                    port.index, identity.logical_sector_size
                ),
            );
            return Err("only disks with 512-byte logical sectors are served".into());
        }
        if identity.sectors == 0 {
            return Err("the disk reports no sectors".into());
        }
        disk.info.sectors = identity.sectors;
        disk.flush = identity
            .write_cache
            .then(|| AtaCommand::flush(identity.flush_ext));
        Ok((disk, identity))
    }

    fn describe(&self, identity: &Identity, link: LinkStatus) {
        let index = self.port.index;
        say(
            self.log,
            format_args!(
                "ahci: port {index}: {} (serial {}, firmware {}), {}",
                identity.model(),
                identity.serial(),
                identity.firmware(),
                link.speed_name()
            ),
        );
        let mut physical = Buffer::<32>::new();
        if identity.physical_sector_size != identity.logical_sector_size {
            let _ = write!(
                physical,
                ", {}-byte physical",
                identity.physical_sector_size
            );
        }
        say(
            self.log,
            format_args!(
                "ahci: port {index}: {} sectors ({} MiB), 512-byte sectors{}{}",
                self.info.sectors,
                self.info.bytes() >> 20,
                physical.as_str(),
                if self.flush.is_some() {
                    ", write cache"
                } else {
                    ""
                }
            ),
        );
    }

    /// Runs one command in slot 0 on the bounce buffer and waits for it.
    fn execute(&mut self, command: AtaCommand) -> Result<(), Failure> {
        let port = self.port;
        let len = command.data_len();
        assert!(len <= self.bounce.len);
        let table = self.memory.device + layout::COMMAND_TABLE as u64;
        self.memory.copy_in(layout::COMMAND_TABLE, &command.fis());
        let mut prds = 0;
        while let Some(entry) = oceans_ahci::prd(self.bounce.device, len, prds) {
            assert!(prds < layout::MAX_PRDS);
            self.memory
                .copy_in(layout::PRDT + prds * oceans_ahci::PRD_SIZE, &entry);
            prds += 1;
        }
        self.memory.copy_in(
            layout::COMMAND_LIST,
            &oceans_ahci::command_header(command.writes(), prds as u16, table),
        );
        port.write(port::IS, u32::MAX);
        // The command is in memory before the controller hears of it.
        fence(Ordering::SeqCst);
        port.write(port::CI, 1);

        let deadline = oceans_rt::clock_ms() + COMMAND_TIMEOUT_MS;
        let mut spins = 0u32;
        let result = loop {
            let status = port.read(port::IS);
            if status & is::FATAL != 0 {
                break Err(Failure::Device(status, port.task_file()));
            }
            if port.read(port::CI) & 1 == 0 {
                break Ok(());
            }
            if oceans_rt::clock_ms() >= deadline {
                break Err(Failure::Timeout);
            }
            spins += 1;
            if spins.is_multiple_of(64) {
                oceans_rt::sleep_ms(1);
            } else {
                oceans_rt::yield_now();
            }
        };
        // The data is read after the completion.
        fence(Ordering::Acquire);
        let status = port.read(port::IS);
        port.write(port::IS, status);
        let task = port.task_file();
        match result {
            Ok(()) if status & is::FATAL != 0 || task.has_error() => {
                Err(Failure::Device(status, task))
            }
            other => other,
        }
    }

    /// A command for a client: errors are logged, the port recovered, and
    /// the failure mapped to a block status.
    fn io(&mut self, command: AtaCommand, what: &str) -> Result<(), Status> {
        if self.failed {
            return Err(Status::IoError);
        }
        match self.execute(command) {
            Ok(()) => Ok(()),
            Err(Failure::Device(status, task)) => {
                say(
                    self.log,
                    format_args!(
                        "ahci: {what} failed: {} ({}, task file {:#06x})",
                        task.message(),
                        oceans_ahci::port_error_message(status),
                        (u32::from(task.error) << 8) | u32::from(task.status)
                    ),
                );
                // The engine stops on an error: restart it, or reset the
                // link if the disk is stuck.
                let restarted = self.port.stop_commands().and_then(|()| {
                    self.port.clear_errors();
                    if self.port.task_file().is_busy() {
                        Err("stuck")
                    } else {
                        self.port.start_commands()
                    }
                });
                if restarted.is_err() {
                    self.reset();
                }
                Err(if task.out_of_range() {
                    Status::OutOfRange
                } else {
                    Status::IoError
                })
            }
            Err(Failure::Timeout) => {
                say(
                    self.log,
                    format_args!("ahci: {what} timed out; resetting the port"),
                );
                self.reset();
                Err(Status::IoError)
            }
        }
    }

    fn reset(&mut self) {
        match self.port.reset() {
            Ok(()) => say(self.log, format_args!("ahci: port reset")),
            Err(problem) => {
                say(
                    self.log,
                    format_args!("ahci: the port cannot be reset: {problem}"),
                );
                self.failed = true;
            }
        }
    }

    /// Moves sectors between the disk and a session buffer, through the
    /// bounce buffer.
    fn transfer(&mut self, session: &Session, write: bool, data: &[u8]) -> Status {
        let Some(request) = Transfer::decode(data) else {
            return Status::BadRequest;
        };
        let Some(len) = request.checked_len(self.info.sectors, session.size) else {
            return Status::OutOfRange;
        };
        let mut sector = request.sector;
        let mut remaining = request.count;
        let mut done = 0;
        while remaining > 0 {
            let count = oceans_ahci::chunk_sectors(remaining, BOUNCE_SECTORS);
            let bytes = usize::from(count) * SECTOR_SIZE;
            // SAFETY: `offset + len` lies inside the session buffer
            // (`checked_len`), mapped read-write while the session lives,
            // and `done + bytes <= len`; `bytes` fits the bounce buffer
            // (`BOUNCE_SECTORS`).
            let client = unsafe { session.buffer.add(request.offset as usize + done) };
            let window = self.bounce.virt;
            let result = if write {
                unsafe { ptr::copy_nonoverlapping(client, window, bytes) };
                self.io(AtaCommand::write(sector, count), "a write")
            } else {
                self.io(AtaCommand::read(sector, count), "a read")
                    .inspect(|()| unsafe { ptr::copy_nonoverlapping(window, client, bytes) })
            };
            if let Err(status) = result {
                return status;
            }
            sector += u64::from(count);
            remaining -= u32::from(count);
            done += bytes;
        }
        debug_assert_eq!(done, len);
        Status::Ok
    }

    fn flush(&mut self) -> Status {
        // Without a write cache, writes are durable on completion.
        match self.flush {
            Some(command) => self.io(command, "a flush").err().unwrap_or(Status::Ok),
            None => Status::Ok,
        }
    }

    /// When the service stops: the cache reaches the medium, the port
    /// goes idle.
    fn shutdown(&mut self) {
        let _ = self.flush();
        if let Err(problem) = self.port.stop() {
            say(self.log, format_args!("ahci: at shutdown: {problem}"));
        }
    }
}

/// One page for the port's structures and the bounce buffer, both within
/// the controller's reach.
fn allocate(device: Handle, caps: &Capabilities) -> Result<(Dma, Dma), &'static str> {
    let page = Dma::new(device, layout::SIZE)?;
    let bounce = Dma::new(device, BOUNCE_SIZE)?;
    if !caps.reaches(page.device, page.len) || !caps.reaches(bounce.device, bounce.len) {
        return Err("DMA memory lies beyond the controller's 32-bit addresses");
    }
    Ok((page, bounce))
}

/// Serves the block protocol until the endpoint fails; without a disk,
/// every request is answered with `IoError`.
fn serve(log: Handle, server: Handle, disk: &mut Option<Disk>) -> i64 {
    let mut sessions: [Option<Session>; MAX_SESSIONS] = [None; MAX_SESSIONS];
    let mut next_badge = 1;
    let mut data = [0u8; 64];
    let mut handles = [Handle(0); 4];
    loop {
        let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
            Ok(got) => got,
            Err(error) => {
                say(log, format_args!("ahci: receive failed: {error:?}"));
                return EXIT_RECEIVE;
            }
        };
        let find = |badge| {
            sessions
                .iter()
                .position(|s| s.is_some_and(|s: Session| s.badge == badge))
        };
        if got.closed {
            if let Some(index) = find(got.badge)
                && let Some(session) = sessions[index].take()
            {
                let _ = oceans_rt::memory_unmap(session.buffer);
            }
            continue;
        }
        let received = &handles[..got.handles_len];
        let session = find(got.badge).and_then(|i| sessions[i]);
        let mut reply_handle = None;
        let mut reply_data = [0u8; Info::SIZE];
        let mut reply_len = 0;
        let status = match (disk.as_mut(), got.label, session) {
            (None, _, _) => Status::IoError,
            (Some(disk), op::INFO, _) => {
                reply_data = disk.info.encode();
                reply_len = Info::SIZE;
                Status::Ok
            }
            (Some(_), op::OPEN, None) if got.badge == 0 && received.len() == 1 => {
                match open_session(server, received[0], next_badge, &mut sessions) {
                    Ok(handle) => {
                        next_badge += 1;
                        reply_handle = Some(handle);
                        Status::Ok
                    }
                    Err(status) => status,
                }
            }
            (Some(disk), op::READ, Some(session)) => {
                disk.transfer(&session, false, &data[..got.data_len])
            }
            (Some(disk), op::WRITE, Some(session)) => {
                disk.transfer(&session, true, &data[..got.data_len])
            }
            (Some(disk), op::FLUSH, Some(_)) => disk.flush(),
            _ => Status::BadRequest,
        };
        // Capabilities sent with a request we did not take are closed.
        if !matches!(got.label, op::OPEN) || status != Status::Ok {
            for &handle in received {
                let _ = oceans_rt::close(handle);
            }
        }
        let reply: &[Handle] = match &reply_handle {
            Some(handle) => core::slice::from_ref(handle),
            None => &[],
        };
        if oceans_rt::ipc_reply_msg(status as u64, &reply_data[..reply_len], reply).is_err()
            && let Some(handle) = reply_handle
        {
            let _ = oceans_rt::close(handle);
        }
    }
}

#[derive(Clone, Copy)]
struct Session {
    badge: u64,
    buffer: *mut u8,
    size: usize,
}

/// Maps a client's buffer and mints its session handle.
fn open_session(
    server: Handle,
    memory: Handle,
    badge: u64,
    sessions: &mut [Option<Session>; MAX_SESSIONS],
) -> Result<Handle, Status> {
    let slot = sessions
        .iter()
        .position(Option::is_none)
        .ok_or(Status::NoSpace)?;
    let size = oceans_rt::memory_size(memory).map_err(|_| Status::BadRequest)? as usize;
    if size == 0 || size > oceans_block_proto::MAX_BUFFER || !size.is_multiple_of(SECTOR_SIZE) {
        return Err(Status::BadRequest);
    }
    let buffer = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
        .map_err(|_| Status::BadRequest)?;
    // The mapping keeps the memory; the handle is not needed.
    let _ = oceans_rt::close(memory);
    match oceans_rt::endpoint_mint(server, badge) {
        Ok(handle) => {
            sessions[slot] = Some(Session {
                badge,
                buffer,
                size,
            });
            Ok(handle)
        }
        Err(_) => {
            let _ = oceans_rt::memory_unmap(buffer);
            Err(Status::NoSpace)
        }
    }
}
