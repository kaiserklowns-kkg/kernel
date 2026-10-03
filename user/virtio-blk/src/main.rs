//! virtio-blk: the block device driver for virtio disks (virtio 1.x, PCI
//! transport, ADR-0021).
//!
//! An ordinary service. Its authority is exactly what init grants in
//! `services.conf`: one device capability (`grant = device:1af4:1042`),
//! the endpoint it serves (`provide = block`) and a log. Through the device
//! capability it maps the virtio register BAR, allocates DMA memory for the
//! virtqueue and a bounce buffer, and receives completions as MSI-X
//! interrupts on a notification. It speaks the block protocol
//! (`oceans-block-proto`) to clients, copying between their session buffers
//! and its DMA buffer, so no client ever sees a physical address.
//!
//! One request is in flight at a time: simple, and plenty for a disk that
//! serves one filesystem. Without MSI-X it falls back to polling.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use oceans_block_proto::{Info, SECTOR_SIZE, Status, Transfer, info_flags, op};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};

oceans_rt::entry!(main);

/// PCI configuration space.
const PCI_STATUS: u16 = 0x06;
const PCI_STATUS_CAPABILITIES: u32 = 1 << 4;
const PCI_CAPABILITIES: u16 = 0x34;
const PCI_CAP_VENDOR: u32 = 0x09;

/// virtio PCI capability types.
const CAP_COMMON: u32 = 1;
const CAP_NOTIFY: u32 = 2;
const CAP_DEVICE: u32 = 4;

/// Common configuration registers.
mod common {
    pub const DEVICE_FEATURE_SELECT: usize = 0x00;
    pub const DEVICE_FEATURE: usize = 0x04;
    pub const DRIVER_FEATURE_SELECT: usize = 0x08;
    pub const DRIVER_FEATURE: usize = 0x0c;
    pub const CONFIG_MSIX_VECTOR: usize = 0x10;
    pub const DEVICE_STATUS: usize = 0x14;
    pub const QUEUE_SELECT: usize = 0x16;
    pub const QUEUE_SIZE: usize = 0x18;
    pub const QUEUE_MSIX_VECTOR: usize = 0x1a;
    pub const QUEUE_ENABLE: usize = 0x1c;
    pub const QUEUE_NOTIFY_OFF: usize = 0x1e;
    pub const QUEUE_DESC: usize = 0x20;
    pub const QUEUE_DRIVER: usize = 0x28;
    pub const QUEUE_DEVICE: usize = 0x30;
    pub const CONFIG_GENERATION: usize = 0x15;
    pub const LEN: u32 = 0x38;
}

mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const FAILED: u8 = 128;
}

mod feature {
    pub const BLK_RO: u64 = 1 << 5;
    pub const BLK_FLUSH: u64 = 1 << 9;
    pub const VERSION_1: u64 = 1 << 32;
}

const NO_VECTOR: u16 = 0xffff;

/// Request types.
const REQUEST_IN: u32 = 0;
const REQUEST_OUT: u32 = 1;
const REQUEST_FLUSH: u32 = 4;

/// Descriptor flags.
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

/// Queue entries used (one request needs three descriptors).
const QUEUE_SIZE: u16 = 16;
/// Layout of the ring page: descriptor table, available ring, used ring,
/// then the request header and status byte.
const AVAIL_OFFSET: usize = 16 * QUEUE_SIZE as usize;
const USED_OFFSET: usize = 2048;
const HEADER_OFFSET: usize = 3072;
const STATUS_OFFSET: usize = HEADER_OFFSET + 16;
/// Bounce buffer: the most one device request moves.
const BOUNCE_SIZE: usize = 64 * 1024;
const MAX_SESSIONS: usize = 16;

/// Exit codes.
const EXIT_BAD_START: i64 = 2;
const EXIT_DEVICE: i64 = 3;

/// Memory-mapped registers.
#[derive(Clone, Copy)]
struct Mmio(*mut u8);

impl Mmio {
    fn at(self, offset: usize) -> Self {
        // SAFETY (for every accessor): offsets stay within the capability
        // region checked against the BAR size in `Device::region`.
        Self(unsafe { self.0.add(offset) })
    }

    fn read8(self, offset: usize) -> u8 {
        unsafe { ptr::read_volatile(self.0.add(offset)) }
    }

    fn read16(self, offset: usize) -> u16 {
        unsafe { ptr::read_volatile(self.0.add(offset).cast()) }
    }

    fn read32(self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile(self.0.add(offset).cast()) }
    }

    fn write8(self, offset: usize, value: u8) {
        unsafe { ptr::write_volatile(self.0.add(offset), value) }
    }

    fn write16(self, offset: usize, value: u16) {
        unsafe { ptr::write_volatile(self.0.add(offset).cast(), value) }
    }

    fn write32(self, offset: usize, value: u32) {
        unsafe { ptr::write_volatile(self.0.add(offset).cast(), value) }
    }

    /// 64-bit registers are written as two 32-bit halves, low first.
    fn write64(self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}

/// A virtio structure: which BAR, where in it, how long.
#[derive(Clone, Copy, Default)]
struct CapRegion {
    bar: u8,
    offset: u32,
    length: u32,
}

/// DMA memory: mapped here, and the address the device uses.
#[derive(Clone, Copy)]
struct Dma {
    virt: *mut u8,
    device: u64,
}

struct Driver {
    log: Handle,
    notify: Mmio,
    ring: Dma,
    bounce: Dma,
    /// `None`: polling.
    irq: Option<Handle>,
    queue_size: u16,
    avail_index: u16,
    used_index: u16,
    info: Info,
    flush: bool,
}

#[derive(Clone, Copy)]
struct Session {
    badge: u64,
    buffer: *mut u8,
    size: usize,
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
    let mut driver = match Driver::start(log, device) {
        Ok(driver) => driver,
        Err(problem) => {
            say(log, format_args!("virtio-blk: {problem}"));
            return EXIT_DEVICE;
        }
    };
    let info = driver.info;
    say(
        log,
        format_args!(
            "virtio-blk: {} sectors ({} MiB){}, {}",
            info.sectors,
            info.bytes() >> 20,
            if info.read_only() { ", read-only" } else { "" },
            if driver.irq.is_some() {
                "MSI-X"
            } else {
                "polling"
            }
        ),
    );
    driver.serve(server)
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// The virtio structures listed in the device's PCI capabilities.
fn find_regions(device: Handle) -> Result<(CapRegion, CapRegion, u32, CapRegion), &'static str> {
    let config = |offset: u16, width: u8| oceans_rt::device_config_read(device, offset, width);
    let error = |_| "cannot read configuration space";
    if config(PCI_STATUS, 2).map_err(error)? & PCI_STATUS_CAPABILITIES == 0 {
        return Err("no PCI capabilities");
    }
    let (mut common, mut notify, mut device_cfg) = (None, None, None);
    let mut multiplier = 0;
    let mut next = config(PCI_CAPABILITIES, 1).map_err(error)? as u16 & !3;
    // Bounded: a malicious device cannot loop us forever.
    for _ in 0..48 {
        if next < 0x40 {
            break;
        }
        let at = next;
        next = config(at + 1, 1).map_err(error)? as u16 & !3;
        if config(at, 1).map_err(error)? != PCI_CAP_VENDOR {
            continue;
        }
        let region = CapRegion {
            bar: config(at + 4, 1).map_err(error)? as u8,
            offset: config(at + 8, 4).map_err(error)?,
            length: config(at + 12, 4).map_err(error)?,
        };
        // The first structure of each type is the one to use.
        match config(at + 3, 1).map_err(error)? {
            CAP_COMMON if common.is_none() => common = Some(region),
            CAP_NOTIFY if notify.is_none() => {
                notify = Some(region);
                multiplier = config(at + 16, 4).map_err(error)?;
            }
            CAP_DEVICE if device_cfg.is_none() => device_cfg = Some(region),
            _ => {}
        }
    }
    match (common, notify, device_cfg) {
        (Some(c), Some(n), Some(d)) => Ok((c, n, multiplier, d)),
        _ => Err("not a modern virtio device (missing virtio PCI capabilities)"),
    }
}

impl Driver {
    fn start(log: Handle, device: Handle) -> Result<Self, &'static str> {
        let (common_cap, notify_cap, multiplier, device_cap) = find_regions(device)?;
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;

        let mut bars: [Option<(*mut u8, u64)>; 6] = [None; 6];
        let mut region = |cap: CapRegion, min_len: u32| -> Result<Mmio, &'static str> {
            let index = usize::from(cap.bar);
            if index >= 6 || cap.length < min_len {
                return Err("bad virtio capability");
            }
            let (base, size) = match bars[index] {
                Some(mapped) => mapped,
                None => {
                    let (memory, size) = oceans_rt::device_bar(device, cap.bar)
                        .map_err(|_| "cannot get the register BAR")?;
                    let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
                        .map_err(|_| "cannot map the register BAR")?;
                    let _ = oceans_rt::close(memory);
                    bars[index] = Some((base, size));
                    (base, size)
                }
            };
            if u64::from(cap.offset) + u64::from(cap.length) > size {
                return Err("virtio capability outside its BAR");
            }
            Ok(Mmio(base).at(cap.offset as usize))
        };
        let common = region(common_cap, common::LEN)?;
        let notify_base = region(notify_cap, 2)?;
        let device_cfg = region(device_cap, 8)?;

        // Reset, then announce ourselves (virtio 1.x §3.1.1).
        common.write8(common::DEVICE_STATUS, 0);
        let mut settled = false;
        for _ in 0..1000 {
            if common.read8(common::DEVICE_STATUS) == 0 {
                settled = true;
                break;
            }
            oceans_rt::sleep_ms(1);
        }
        if !settled {
            return Err("device did not reset");
        }
        let mut device_status = status::ACKNOWLEDGE | status::DRIVER;
        common.write8(common::DEVICE_STATUS, device_status);
        let fail = |problem| {
            common.write8(common::DEVICE_STATUS, status::FAILED);
            problem
        };

        // Features: virtio 1.x is required; read-only and flush are used.
        common.write32(common::DEVICE_FEATURE_SELECT, 0);
        let low = common.read32(common::DEVICE_FEATURE);
        common.write32(common::DEVICE_FEATURE_SELECT, 1);
        let high = common.read32(common::DEVICE_FEATURE);
        let offered = (u64::from(high) << 32) | u64::from(low);
        if offered & feature::VERSION_1 == 0 {
            return Err(fail("device does not offer virtio 1.x"));
        }
        let accepted = offered & (feature::VERSION_1 | feature::BLK_RO | feature::BLK_FLUSH);
        common.write32(common::DRIVER_FEATURE_SELECT, 0);
        common.write32(common::DRIVER_FEATURE, accepted as u32);
        common.write32(common::DRIVER_FEATURE_SELECT, 1);
        common.write32(common::DRIVER_FEATURE, (accepted >> 32) as u32);
        device_status |= status::FEATURES_OK;
        common.write8(common::DEVICE_STATUS, device_status);
        if common.read8(common::DEVICE_STATUS) & status::FEATURES_OK == 0 {
            return Err(fail("device rejected the features"));
        }

        // Interrupts: queue 0 completions on MSI-X entry 0, if possible.
        let irq = oceans_rt::notification_create()
            .ok()
            .filter(|&n| oceans_rt::device_irq(device, 0, n, 1).is_ok());
        common.write16(common::CONFIG_MSIX_VECTOR, NO_VECTOR);

        // Queue 0: the request queue.
        common.write16(common::QUEUE_SELECT, 0);
        let maximum = common.read16(common::QUEUE_SIZE);
        if maximum == 0 {
            return Err(fail("device has no request queue"));
        }
        // Queue sizes are powers of two: the largest one we use that fits.
        let size = QUEUE_SIZE.min(1 << (15 - maximum.leading_zeros()));
        if size < 4 {
            return Err(fail("request queue too small"));
        }
        let ring = dma(device, 4096).map_err(fail)?;
        let bounce = dma(device, BOUNCE_SIZE as u64).map_err(fail)?;
        common.write16(common::QUEUE_SIZE, size);
        let irq = irq.filter(|_| {
            common.write16(common::QUEUE_MSIX_VECTOR, 0);
            common.read16(common::QUEUE_MSIX_VECTOR) == 0
        });
        if irq.is_none() {
            common.write16(common::QUEUE_MSIX_VECTOR, NO_VECTOR);
        }
        common.write64(common::QUEUE_DESC, ring.device);
        common.write64(common::QUEUE_DRIVER, ring.device + AVAIL_OFFSET as u64);
        common.write64(common::QUEUE_DEVICE, ring.device + USED_OFFSET as u64);
        let notify_offset =
            u64::from(common.read16(common::QUEUE_NOTIFY_OFF)) * u64::from(multiplier);
        if notify_offset + 2 > u64::from(notify_cap.length) {
            return Err(fail("queue notification register outside its region"));
        }
        let notify = notify_base.at(notify_offset as usize);
        common.write16(common::QUEUE_ENABLE, 1);
        device_status |= status::DRIVER_OK;
        common.write8(common::DEVICE_STATUS, device_status);

        // Capacity, read consistently (the generation changes if the
        // device updates its configuration meanwhile).
        let sectors = loop {
            let before = common.read8(common::CONFIG_GENERATION);
            let low = device_cfg.read32(0);
            let high = device_cfg.read32(4);
            if common.read8(common::CONFIG_GENERATION) == before {
                break (u64::from(high) << 32) | u64::from(low);
            }
        };
        Ok(Self {
            log,
            notify,
            ring,
            bounce,
            irq,
            queue_size: size,
            avail_index: 0,
            used_index: 0,
            info: Info {
                sectors,
                sector_size: SECTOR_SIZE as u32,
                flags: if accepted & feature::BLK_RO != 0 {
                    info_flags::READ_ONLY
                } else {
                    0
                },
            },
            flush: accepted & feature::BLK_FLUSH != 0,
        })
    }

    fn ring_write<T>(&self, offset: usize, value: T) {
        // SAFETY: `offset` is inside the ring page, aligned for `T`.
        unsafe { ptr::write_volatile(self.ring.virt.add(offset).cast::<T>(), value) }
    }

    fn ring_read<T>(&self, offset: usize) -> T {
        // SAFETY: as for `ring_write`.
        unsafe { ptr::read_volatile(self.ring.virt.add(offset).cast::<T>()) }
    }

    fn descriptor(&self, index: u16, address: u64, len: u32, flags: u16, next: u16) {
        let at = 16 * usize::from(index);
        self.ring_write(at, address);
        self.ring_write(at + 8, len);
        self.ring_write(at + 12, flags);
        self.ring_write(at + 14, next);
    }

    /// Runs one device request on the bounce buffer and waits for it.
    fn request(&mut self, kind: u32, sector: u64, len: usize) -> Result<(), Status> {
        self.ring_write(HEADER_OFFSET, kind);
        self.ring_write(HEADER_OFFSET + 4, 0u32);
        self.ring_write(HEADER_OFFSET + 8, sector);
        self.ring_write(STATUS_OFFSET, 0xffu8);

        let header = self.ring.device + HEADER_OFFSET as u64;
        let status_address = self.ring.device + STATUS_OFFSET as u64;
        if len == 0 {
            self.descriptor(0, header, 16, DESC_NEXT, 2);
        } else {
            let data_flags = if kind == REQUEST_IN { DESC_WRITE } else { 0 };
            self.descriptor(0, header, 16, DESC_NEXT, 1);
            self.descriptor(1, self.bounce.device, len as u32, data_flags | DESC_NEXT, 2);
        }
        self.descriptor(2, status_address, 1, DESC_WRITE, 0);

        // Publish the chain, then the index, then tell the device.
        let slot = AVAIL_OFFSET + 4 + 2 * usize::from(self.avail_index % self.queue_size);
        self.ring_write(slot, 0u16);
        fence(Ordering::SeqCst);
        self.avail_index = self.avail_index.wrapping_add(1);
        self.ring_write(AVAIL_OFFSET + 2, self.avail_index);
        fence(Ordering::SeqCst);
        self.notify.write16(0, 0);

        let mut spins = 0u32;
        loop {
            fence(Ordering::SeqCst);
            if self.ring_read::<u16>(USED_OFFSET + 2) != self.used_index {
                break;
            }
            match self.irq {
                // Latched: a completion before the wait is not lost.
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
        }
        self.used_index = self.used_index.wrapping_add(1);
        fence(Ordering::SeqCst);
        match self.ring_read::<u8>(STATUS_OFFSET) {
            0 => Ok(()),
            2 => Err(Status::Unsupported),
            _ => Err(Status::IoError),
        }
    }

    /// Moves sectors between the disk and a session buffer, through the
    /// bounce buffer, in chunks.
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
        let mut done = 0;
        while done < len {
            let chunk = (len - done).min(BOUNCE_SIZE);
            let sector = request.sector + (done / SECTOR_SIZE) as u64;
            // SAFETY: `offset + len` lies inside the session buffer
            // (`checked_len`), mapped read-write while the session lives;
            // the bounce buffer holds BOUNCE_SIZE bytes.
            let client = unsafe { session.buffer.add(request.offset as usize + done) };
            let result = if write {
                unsafe { ptr::copy_nonoverlapping(client, self.bounce.virt, chunk) };
                self.request(REQUEST_OUT, sector, chunk)
            } else {
                self.request(REQUEST_IN, sector, chunk)
                    .inspect(|()| unsafe {
                        ptr::copy_nonoverlapping(self.bounce.virt, client, chunk);
                    })
            };
            if let Err(status) = result {
                return status;
            }
            done += chunk;
        }
        Status::Ok
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
                    say(
                        self.log,
                        format_args!("virtio-blk: receive failed: {error:?}"),
                    );
                    return 4;
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
                (op::FLUSH, Some(_)) if self.flush => self
                    .request(REQUEST_FLUSH, 0, 0)
                    .err()
                    .unwrap_or(Status::Ok),
                // Without the flush feature, writes are durable on completion.
                (op::FLUSH, Some(_)) => Status::Ok,
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

/// DMA memory of `size` bytes, mapped read-write.
fn dma(device: Handle, size: u64) -> Result<Dma, &'static str> {
    let (memory, address) =
        oceans_rt::device_dma_create(device, size).map_err(|_| "cannot allocate DMA memory")?;
    let virt = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
        .map_err(|_| "cannot map DMA memory")?;
    let _ = oceans_rt::close(memory);
    Ok(Dma {
        virt,
        device: address,
    })
}
