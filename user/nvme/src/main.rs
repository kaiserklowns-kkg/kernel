//! nvme: the block device driver for NVM Express controllers (ADR-0040).
//!
//! An ordinary service, like `virtio-blk`. Its authority is exactly what
//! init grants in `services.conf`: one device capability (the first NVMe
//! controller, `grant = device-class:010802`), the endpoint it serves and a
//! log. Through the device capability it maps the controller's registers
//! (BAR 0), allocates DMA memory for one admin and one I/O queue pair, a
//! bounce buffer and its PRP list, and receives completions as MSI-X
//! interrupts on a notification. It speaks the block protocol
//! (`oceans-block-proto`) to clients, copying between their session
//! buffers and its bounce buffer, so no client ever sees a physical
//! address.
//!
//! - **One namespace**, the first active one, is served as the disk.
//!   Namespaces formatted with logical blocks larger than the protocol's
//!   512-byte sectors are served by whole blocks around each request
//!   (`oceans_nvme::next_chunk`); a partial block is read before it is
//!   written.
//! - **One command in flight** at a time, as for virtio-blk: simple, and
//!   enough for a disk that serves one filesystem.
//! - **Failures:** a command that does not complete within 30 s resets the
//!   controller and brings it up again; a controller that cannot be
//!   brought back answers every request with `IoError`. A controller with
//!   a volatile write cache is flushed on `FLUSH`. When the service stops,
//!   the controller is shut down cleanly.
//! - Without MSI-X it falls back to polling.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use oceans_block_proto::{Info, SECTOR_SIZE, Status, Transfer, info_flags, op};
use oceans_nvme::{
    COMMAND_SIZE, COMPLETION_SIZE, Capabilities, Command, Completion, ControllerInfo,
    IDENTIFY_SIZE, NamespaceInfo, PAGE_SIZE, Ring, StatusCode, cc, cns, csts, reg,
};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const ADMIN_ENTRIES: u16 = 16;
const IO_ENTRIES: u16 = 16;
const IO_QUEUE: u16 = 1;
/// The most one device transfer moves (its PRP list fits one page).
const BOUNCE_SIZE: usize = 128 * 1024;
/// The largest logical block served.
const MAX_BLOCK_SIZE: u32 = 4096;
const MAX_SESSIONS: usize = 16;
/// A command not completed by then resets the controller.
const COMMAND_TIMEOUT_MS: u64 = 30_000;
/// Readiness changes take at most `CAP.TO`; never wait less than this.
const MIN_READY_TIMEOUT_MS: u64 = 500;

/// Notification bits.
const IRQ: u64 = 1 << 0;
const TIMEOUT: u64 = 1 << 1;

/// Exit codes.
const EXIT_BAD_START: i64 = 2;
const EXIT_DEVICE: i64 = 3;
const EXIT_RECEIVE: i64 = 4;

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
    let (mut controller, identity) = match Controller::start(log, device) {
        Ok(started) => started,
        Err(problem) => {
            say(log, format_args!("nvme: {problem}"));
            return EXIT_DEVICE;
        }
    };
    let (major, minor) = oceans_nvme::version(controller.regs.read32(reg::VS));
    say(
        log,
        format_args!(
            "nvme: {} (serial {}, firmware {}), NVMe {major}.{minor}, {}",
            identity.model(),
            identity.serial(),
            identity.firmware(),
            if controller.irq.is_some() {
                "MSI-X"
            } else {
                "polling"
            }
        ),
    );
    let info = controller.info;
    say(
        log,
        format_args!(
            "nvme: namespace {}: {} sectors ({} MiB), {}-byte blocks{}{}",
            controller.namespace,
            info.sectors,
            info.bytes() >> 20,
            controller.block_size,
            if info.read_only() { ", read-only" } else { "" },
            if controller.flush {
                ", write cache"
            } else {
                ""
            }
        ),
    );
    let code = controller.serve(server);
    controller.shutdown();
    code
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// The controller's registers (BAR 0), mapped.
#[derive(Clone, Copy)]
struct Registers {
    base: *mut u8,
    size: usize,
}

// SAFETY (all accessors): `base` maps `size` bytes of the controller's
// register BAR read-write for the life of the process; every offset is
// checked against `size`, and registers are naturally aligned.
impl Registers {
    fn read32(self, offset: usize) -> u32 {
        assert!(offset + 4 <= self.size);
        unsafe { ptr::read_volatile(self.base.add(offset).cast()) }
    }

    fn write32(self, offset: usize, value: u32) {
        assert!(offset + 4 <= self.size);
        unsafe { ptr::write_volatile(self.base.add(offset).cast(), value) }
    }

    fn read64(self, offset: usize) -> u64 {
        u64::from(self.read32(offset)) | (u64::from(self.read32(offset + 4)) << 32)
    }

    /// Written as two halves, low first (as the specification allows).
    fn write64(self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}

/// A submission and completion queue pair in DMA memory.
struct Queue {
    id: u16,
    submissions: Dma,
    completions: Dma,
    submit: Ring,
    complete: Ring,
}

impl Queue {
    fn new(device: Handle, id: u16, entries: u16) -> Result<Self, &'static str> {
        let entries_bytes = |size: usize| (usize::from(entries) * size).next_multiple_of(PAGE_SIZE);
        Ok(Self {
            id,
            submissions: Dma::new(device, entries_bytes(COMMAND_SIZE))?,
            completions: Dma::new(device, entries_bytes(COMPLETION_SIZE))?,
            submit: Ring::new(entries),
            complete: Ring::new(entries),
        })
    }

    /// Empties the queue for a controller that has just been reset.
    fn reset(&mut self) {
        for dma in [self.submissions, self.completions] {
            // SAFETY: `virt` maps `len` bytes of our DMA memory.
            unsafe { ptr::write_bytes(dma.virt, 0, dma.len) };
        }
        self.submit = Ring::new(self.submit.entries);
        self.complete = Ring::new(self.complete.entries);
    }

    fn submit(&mut self, regs: Registers, caps: &Capabilities, command: &Command) {
        let slot = usize::from(self.submit.index) * COMMAND_SIZE;
        self.submissions.copy_in(slot, &command.encode());
        // The entry is in memory before the controller hears of it.
        fence(Ordering::SeqCst);
        self.submit.advance();
        regs.write32(
            caps.submission_doorbell(self.id),
            u32::from(self.submit.index),
        );
    }

    /// The next completion, if the controller has posted one.
    fn poll(&mut self, regs: Registers, caps: &Capabilities) -> Option<Completion> {
        let slot = usize::from(self.complete.index) * COMPLETION_SIZE;
        let last: u32 = self.completions.read(slot + 12);
        if (last & (1 << 16) != 0) != self.complete.phase {
            return None;
        }
        // The rest of the entry is read after its phase tag.
        fence(Ordering::Acquire);
        let mut entry = [0u8; COMPLETION_SIZE];
        for (i, dword) in entry.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *dword = self.completions.read::<u32>(slot + 4 * i).to_le_bytes();
        }
        self.complete.advance();
        regs.write32(
            caps.completion_doorbell(self.id),
            u32::from(self.complete.index),
        );
        Some(Completion::decode(&entry))
    }
}

/// Why a command did not succeed.
#[derive(Clone, Copy)]
enum Failure {
    Timeout,
    Device(StatusCode),
}

struct Controller {
    log: Handle,
    regs: Registers,
    caps: Capabilities,
    admin: Queue,
    io: Queue,
    /// `IDENTIFY` data lands here.
    page: Dma,
    bounce: Dma,
    prp_list: Dma,
    /// `Some`: completions interrupt (MSI-X vector 0) on the notification.
    irq: Option<Handle>,
    next_id: u16,
    namespace: u32,
    block_size: u32,
    max_blocks: u32,
    flush: bool,
    info: Info,
    /// The controller could not be brought back after a failure.
    failed: bool,
}

impl Controller {
    fn start(log: Handle, device: Handle) -> Result<(Self, ControllerInfo), &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let (memory, size) =
            oceans_rt::device_bar(device, 0).map_err(|_| "cannot get the register BAR")?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let _ = oceans_rt::close(memory);
        let regs = Registers {
            base: base.map_err(|_| "cannot map the register BAR")?,
            size: size as usize,
        };
        if regs.size < reg::DOORBELLS + 8 {
            return Err("the register BAR is too small");
        }
        let caps = Capabilities::decode(regs.read64(reg::CAP));
        if !caps.nvm_command_set {
            return Err("the controller does not support the NVM command set");
        }
        if !caps.supports_4k_pages() {
            return Err("the controller does not support 4 KiB pages");
        }
        if caps.completion_doorbell(IO_QUEUE) + 4 > regs.size {
            return Err("the doorbells lie outside the register BAR");
        }
        let io_entries = IO_ENTRIES.min(caps.max_queue_entries.min(u32::from(u16::MAX)) as u16);
        let bounce = Dma::new(device, BOUNCE_SIZE)?;
        let prp_list = Dma::new(device, PAGE_SIZE)?;
        for index in 0..BOUNCE_SIZE / PAGE_SIZE - 1 {
            prp_list.write(index * 8, oceans_nvme::prp_list_entry(bounce.device, index));
        }
        let notification =
            oceans_rt::notification_create().map_err(|_| "cannot create a notification")?;
        let irq = oceans_rt::device_irq(device, 0, notification, IRQ)
            .is_ok()
            .then_some(notification);
        let mut controller = Self {
            log,
            regs,
            caps,
            admin: Queue::new(device, 0, ADMIN_ENTRIES)?,
            io: Queue::new(device, IO_QUEUE, io_entries)?,
            page: Dma::new(device, IDENTIFY_SIZE)?,
            bounce,
            prp_list,
            irq,
            next_id: 0,
            namespace: 0,
            block_size: SECTOR_SIZE as u32,
            max_blocks: 1,
            flush: false,
            info: Info {
                sectors: 0,
                sector_size: SECTOR_SIZE as u32,
                flags: 0,
            },
            failed: false,
        };
        let identity = controller.bring_up()?;
        Ok((controller, identity))
    }

    /// Resets the controller and sets it up: admin queue, identify, I/O
    /// queue pair, namespace.
    fn bring_up(&mut self) -> Result<ControllerInfo, &'static str> {
        self.disable()?;
        self.admin.reset();
        self.io.reset();
        let regs = self.regs;
        let admin = u32::from(ADMIN_ENTRIES - 1);
        regs.write32(reg::AQA, (admin << 16) | admin);
        regs.write64(reg::ASQ, self.admin.submissions.device);
        regs.write64(reg::ACQ, self.admin.completions.device);
        if self.irq.is_none() {
            // Polling: no pin interrupts (the mask is not used with MSI-X).
            regs.write32(reg::INTMS, u32::MAX);
        }
        regs.write32(
            reg::CC,
            cc::ENABLE | cc::NVM_COMMAND_SET | cc::IOSQES | cc::IOCQES,
        );
        self.wait_status(|status| status & csts::READY != 0, true)
            .map_err(|()| "the controller does not become ready")?;

        self.admin(Command::identify(cns::CONTROLLER, 0, self.page.device))?;
        let identity = ControllerInfo::parse(self.identify_data())
            .ok_or("unreadable controller identify data")?;
        self.admin(Command::set_queue_count(1))?;
        let entries = self.io.submit.entries;
        let vector = self.irq.map(|_| 0);
        self.admin(Command::create_completion_queue(
            IO_QUEUE,
            entries,
            self.io.completions.device,
            vector,
        ))?;
        self.admin(Command::create_submission_queue(
            IO_QUEUE,
            entries,
            self.io.submissions.device,
            IO_QUEUE,
        ))?;

        // The first active namespace (NVMe 1.0 controllers lack the list:
        // namespace 1 then).
        let listed = self
            .admin(Command::identify(
                cns::ACTIVE_NAMESPACES,
                0,
                self.page.device,
            ))
            .ok()
            .and_then(|()| oceans_nvme::first_namespace(self.identify_data()));
        self.namespace = listed.unwrap_or(1);
        self.admin(Command::identify(
            cns::NAMESPACE,
            self.namespace,
            self.page.device,
        ))?;
        let namespace = NamespaceInfo::parse(self.identify_data())
            .ok_or("unreadable namespace identify data")?;
        if namespace.blocks == 0 {
            return Err("the namespace is empty");
        }
        if namespace.block_size > MAX_BLOCK_SIZE {
            return Err("the namespace's blocks are larger than 4 KiB");
        }
        if namespace.metadata_size != 0 {
            return Err("the namespace is formatted with metadata");
        }
        let transfer = (BOUNCE_SIZE as u64).min(
            identity
                .max_transfer(self.caps.min_page_shift)
                .unwrap_or(u64::MAX),
        );
        self.block_size = namespace.block_size;
        self.max_blocks = (transfer / u64::from(namespace.block_size)) as u32;
        if self.max_blocks == 0 {
            return Err("the controller's largest transfer is smaller than a block");
        }
        self.flush = identity.volatile_write_cache;
        self.info = Info {
            sectors: namespace.blocks * u64::from(namespace.block_size / SECTOR_SIZE as u32),
            sector_size: SECTOR_SIZE as u32,
            flags: if namespace.write_protected {
                info_flags::READ_ONLY
            } else {
                0
            },
        };
        self.failed = false;
        Ok(identity)
    }

    fn identify_data(&self) -> &[u8] {
        // SAFETY: `page` maps at least IDENTIFY_SIZE bytes of our DMA
        // memory; the controller wrote it during the completed command.
        unsafe { core::slice::from_raw_parts(self.page.virt, IDENTIFY_SIZE) }
    }

    /// Turns the controller off and waits until it says so.
    fn disable(&mut self) -> Result<(), &'static str> {
        let config = self.regs.read32(reg::CC);
        if config & cc::ENABLE != 0 {
            self.regs.write32(reg::CC, config & !cc::ENABLE);
        }
        self.wait_status(|status| status & csts::READY == 0, false)
            .map_err(|()| "the controller does not reset")
    }

    /// Waits up to `CAP.TO` for `CSTS` to satisfy `done`; `fatal` stops at
    /// a controller fatal status.
    fn wait_status(&self, done: impl Fn(u32) -> bool, fatal: bool) -> Result<(), ()> {
        let deadline = oceans_rt::clock_ms() + self.caps.timeout_ms.max(MIN_READY_TIMEOUT_MS);
        loop {
            let status = self.regs.read32(reg::CSTS);
            if done(status) {
                return Ok(());
            }
            if (fatal && status & csts::FATAL != 0) || oceans_rt::clock_ms() > deadline {
                return Err(());
            }
            oceans_rt::sleep_ms(1);
        }
    }

    /// Runs one command and waits for its completion.
    fn execute(&mut self, admin: bool, mut command: Command) -> Result<Completion, Failure> {
        command.id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let (regs, caps, irq) = (self.regs, self.caps, self.irq);
        let queue = if admin { &mut self.admin } else { &mut self.io };
        queue.submit(regs, &caps, &command);
        let deadline = oceans_rt::clock_ms() + COMMAND_TIMEOUT_MS;
        if let Some(irq) = irq {
            let _ = oceans_rt::timer_set(irq, TIMEOUT, COMMAND_TIMEOUT_MS);
        }
        let mut spins = 0u32;
        let result = loop {
            match queue.poll(regs, &caps) {
                Some(done) if done.id == command.id => break Ok(done),
                // A late completion of a command given up on.
                Some(_) => continue,
                None => {}
            }
            if oceans_rt::clock_ms() >= deadline {
                break Err(Failure::Timeout);
            }
            match irq {
                // Latched: a completion before the wait is not lost; the
                // timer bounds the wait.
                Some(irq) => {
                    let _ = oceans_rt::notification_wait(irq);
                }
                None => {
                    spins += 1;
                    if spins.is_multiple_of(64) {
                        oceans_rt::sleep_ms(1);
                    } else {
                        oceans_rt::yield_now();
                    }
                }
            }
        };
        if let Some(irq) = irq {
            let _ = oceans_rt::timer_set(irq, TIMEOUT, 0);
        }
        match result {
            Ok(done) if done.status.is_success() => Ok(done),
            Ok(done) => Err(Failure::Device(done.status)),
            Err(failure) => Err(failure),
        }
    }

    /// An admin command during bring-up.
    fn admin(&mut self, command: Command) -> Result<(), &'static str> {
        match self.execute(true, command) {
            Ok(_) => Ok(()),
            Err(Failure::Timeout) => Err("an admin command timed out"),
            Err(Failure::Device(status)) => Err(status.message()),
        }
    }

    /// An I/O command on the bounce buffer.
    fn io(&mut self, command: Command, what: &str) -> Result<(), Status> {
        if self.failed {
            return Err(Status::IoError);
        }
        match self.execute(false, command) {
            Ok(_) => Ok(()),
            Err(Failure::Device(status)) => {
                say(
                    self.log,
                    format_args!("nvme: {what} failed: {}", status.message()),
                );
                Err(match (status.code_type(), status.code()) {
                    (0, 0x20) => Status::ReadOnly,
                    (0, 0x80) => Status::OutOfRange,
                    (0, 0x01) => Status::Unsupported,
                    _ => Status::IoError,
                })
            }
            Err(Failure::Timeout) => {
                say(
                    self.log,
                    format_args!("nvme: {what} timed out; resetting the controller"),
                );
                self.recover();
                Err(Status::IoError)
            }
        }
    }

    fn recover(&mut self) {
        let fatal = self.regs.read32(reg::CSTS) & csts::FATAL != 0;
        match self.bring_up() {
            Ok(_) => say(
                self.log,
                format_args!(
                    "nvme: controller reset{}",
                    if fatal { " after a fatal error" } else { "" }
                ),
            ),
            Err(problem) => {
                say(
                    self.log,
                    format_args!("nvme: the controller cannot be reset: {problem}"),
                );
                self.failed = true;
            }
        }
    }

    /// Moves `blocks` logical blocks at `lba` between the disk and the
    /// start of the bounce buffer.
    fn blocks(&mut self, write: bool, lba: u64, blocks: u32) -> Result<(), Status> {
        let len = blocks as usize * self.block_size as usize;
        let prp = oceans_nvme::prp(self.bounce.device, len, self.prp_list.device);
        if write {
            self.io(Command::write(self.namespace, lba, blocks, prp), "a write")
        } else {
            self.io(Command::read(self.namespace, lba, blocks, prp), "a read")
        }
    }

    /// Moves sectors between the disk and a session buffer, through the
    /// bounce buffer, in chunks of whole logical blocks.
    fn transfer(&mut self, session: &Session, write: bool, data: &[u8]) -> Status {
        let Some(request) = Transfer::decode(data) else {
            return Status::BadRequest;
        };
        let Some(len) = request.checked_len(self.info.sectors, session.size) else {
            return Status::OutOfRange;
        };
        if write && self.info.read_only() {
            return Status::ReadOnly;
        }
        let start = request.sector * SECTOR_SIZE as u64;
        let end = start + len as u64;
        let mut position = start;
        while position < end {
            let chunk = oceans_nvme::next_chunk(position, end, self.block_size, self.max_blocks);
            let done = (position - start) as usize;
            // SAFETY: `offset + len` lies inside the session buffer
            // (`checked_len`), mapped read-write while the session lives;
            // the chunk lies inside the bounce buffer (`max_blocks`).
            let client = unsafe { session.buffer.add(request.offset as usize + done) };
            let window = unsafe { self.bounce.virt.add(chunk.offset) };
            let result = if write {
                let filled = if chunk.partial {
                    self.blocks(false, chunk.lba, chunk.blocks)
                } else {
                    Ok(())
                };
                filled.and_then(|()| {
                    unsafe { ptr::copy_nonoverlapping(client, window, chunk.len) };
                    self.blocks(true, chunk.lba, chunk.blocks)
                })
            } else {
                self.blocks(false, chunk.lba, chunk.blocks)
                    .inspect(|()| unsafe { ptr::copy_nonoverlapping(window, client, chunk.len) })
            };
            if let Err(status) = result {
                return status;
            }
            position += chunk.len as u64;
        }
        Status::Ok
    }

    fn flush(&mut self) -> Status {
        // Without a volatile write cache, writes are durable on completion.
        if !self.flush {
            return Status::Ok;
        }
        self.io(Command::flush(self.namespace), "a flush")
            .err()
            .unwrap_or(Status::Ok)
    }

    /// A normal shutdown: the controller finishes what it holds.
    fn shutdown(&mut self) {
        let config = self.regs.read32(reg::CC);
        if config & cc::ENABLE == 0 {
            return;
        }
        self.regs
            .write32(reg::CC, (config & !cc::SHUTDOWN_MASK) | cc::SHUTDOWN_NORMAL);
        let finished = self.wait_status(
            |status| status & csts::SHUTDOWN_MASK == csts::SHUTDOWN_COMPLETE,
            true,
        );
        if finished.is_err() {
            say(
                self.log,
                format_args!("nvme: the shutdown did not complete"),
            );
        }
    }

    fn serve(&mut self, server: Handle) -> i64 {
        let mut sessions: [Option<Session>; MAX_SESSIONS] = [None; MAX_SESSIONS];
        let mut next_badge = 1;
        let mut data = [0u8; 64];
        let mut handles = [Handle(0); 4];
        loop {
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                Err(error) => {
                    say(self.log, format_args!("nvme: receive failed: {error:?}"));
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
            let status = match (got.label, session) {
                (op::INFO, _) => {
                    reply_data = self.info.encode();
                    reply_len = Info::SIZE;
                    Status::Ok
                }
                (op::OPEN, None) if got.badge == 0 && received.len() == 1 => {
                    match open_session(server, received[0], next_badge, &mut sessions) {
                        Ok(handle) => {
                            next_badge += 1;
                            reply_handle = Some(handle);
                            Status::Ok
                        }
                        Err(status) => status,
                    }
                }
                (op::READ, Some(session)) => self.transfer(&session, false, &data[..got.data_len]),
                (op::WRITE, Some(session)) => self.transfer(&session, true, &data[..got.data_len]),
                (op::FLUSH, Some(_)) => self.flush(),
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
