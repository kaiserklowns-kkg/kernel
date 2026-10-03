//! virtio 1.x over PCI, for userspace drivers (ADR-0021, ADR-0023).
//!
//! - [`Transport`] finds the device's virtio structures through its PCI
//!   capabilities, maps them through the device capability, resets the
//!   device, negotiates features and sets up queues.
//! - [`Queue`] is a split virtqueue in one page of DMA memory: descriptor
//!   chains go in, used chains come back.
//!
//! Device-written memory and registers are read with volatile accesses, and
//! every index and length the device reports is bounds-checked: a device is
//! not trusted to keep within its queue.

#![no_std]

use core::ptr;
use core::sync::atomic::{Ordering, fence};

use oceans_rt::{Handle, prot};

const PCI_STATUS: u16 = 0x06;
const PCI_STATUS_CAPABILITIES: u32 = 1 << 4;
const PCI_CAPABILITIES: u16 = 0x34;
const PCI_CAP_VENDOR: u32 = 0x09;

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
    pub const CONFIG_GENERATION: usize = 0x15;
    pub const QUEUE_SELECT: usize = 0x16;
    pub const QUEUE_SIZE: usize = 0x18;
    pub const QUEUE_MSIX_VECTOR: usize = 0x1a;
    pub const QUEUE_ENABLE: usize = 0x1c;
    pub const QUEUE_NOTIFY_OFF: usize = 0x1e;
    pub const QUEUE_DESC: usize = 0x20;
    pub const QUEUE_DRIVER: usize = 0x28;
    pub const QUEUE_DEVICE: usize = 0x30;
    pub const LEN: u32 = 0x38;
}

mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const FAILED: u8 = 128;
}

/// Feature bits shared by all devices.
pub const F_VERSION_1: u64 = 1 << 32;

const NO_VECTOR: u16 = 0xffff;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
/// Largest queue this crate sets up (its rings fit one page).
pub const MAX_QUEUE_SIZE: u16 = 128;
const PAGE: usize = 4096;

/// Memory-mapped device registers.
#[derive(Clone, Copy)]
pub struct Mmio(*mut u8);

// SAFETY (all accessors): an `Mmio` is created only for a register region
// checked to lie inside its mapped BAR (`Transport::region`), and callers
// use offsets inside that region.
impl Mmio {
    fn at(self, offset: usize) -> Self {
        Self(unsafe { self.0.add(offset) })
    }

    pub fn read8(self, offset: usize) -> u8 {
        unsafe { ptr::read_volatile(self.0.add(offset)) }
    }

    pub fn read16(self, offset: usize) -> u16 {
        unsafe { ptr::read_volatile(self.0.add(offset).cast()) }
    }

    pub fn read32(self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile(self.0.add(offset).cast()) }
    }

    pub fn write8(self, offset: usize, value: u8) {
        unsafe { ptr::write_volatile(self.0.add(offset), value) }
    }

    pub fn write16(self, offset: usize, value: u16) {
        unsafe { ptr::write_volatile(self.0.add(offset).cast(), value) }
    }

    pub fn write32(self, offset: usize, value: u32) {
        unsafe { ptr::write_volatile(self.0.add(offset).cast(), value) }
    }

    /// 64-bit registers are written as two 32-bit halves, low first.
    pub fn write64(self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}

/// DMA memory: mapped here, and the address the device uses.
#[derive(Clone, Copy)]
pub struct Dma {
    pub virt: *mut u8,
    pub device: u64,
    pub len: usize,
}

impl Dma {
    /// Contiguous DMA memory for `device`, mapped read-write.
    pub fn new(device: Handle, size: usize) -> Result<Self, &'static str> {
        let (memory, address) = oceans_rt::device_dma_create(device, size as u64)
            .map_err(|_| "cannot allocate DMA memory")?;
        let len = oceans_rt::memory_size(memory).map_err(|_| "cannot size DMA memory")? as usize;
        let virt = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
            .map_err(|_| "cannot map DMA memory")?;
        let _ = oceans_rt::close(memory);
        Ok(Self {
            virt,
            device: address,
            len,
        })
    }

    pub fn write<T>(&self, offset: usize, value: T) {
        assert!(offset + size_of::<T>() <= self.len);
        // SAFETY: in bounds (checked); DMA memory is plain RAM shared with
        // the device, hence volatile.
        unsafe { ptr::write_volatile(self.virt.add(offset).cast::<T>(), value) }
    }

    pub fn read<T>(&self, offset: usize) -> T {
        assert!(offset + size_of::<T>() <= self.len);
        // SAFETY: as for `write`.
        unsafe { ptr::read_volatile(self.virt.add(offset).cast::<T>()) }
    }

    /// Copies `bytes` in at `offset`.
    pub fn copy_in(&self, offset: usize, bytes: &[u8]) {
        assert!(offset + bytes.len() <= self.len);
        // SAFETY: in bounds (checked).
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.virt.add(offset), bytes.len()) }
    }

    /// Copies `out.len()` bytes out from `offset`.
    pub fn copy_out(&self, offset: usize, out: &mut [u8]) {
        assert!(offset + out.len() <= self.len);
        // SAFETY: in bounds (checked).
        unsafe { ptr::copy_nonoverlapping(self.virt.add(offset), out.as_mut_ptr(), out.len()) }
    }
}

#[derive(Clone, Copy, Default)]
struct CapRegion {
    bar: u8,
    offset: u32,
    length: u32,
}

/// A virtio PCI device being driven.
pub struct Transport {
    device: Handle,
    common: Mmio,
    notify: Mmio,
    notify_len: u32,
    multiplier: u32,
    config: Mmio,
    config_len: u32,
    device_status: u8,
}

impl Transport {
    /// Finds and maps the virtio structures, enables the device, resets it
    /// and announces a driver.
    pub fn open(device: Handle) -> Result<Self, &'static str> {
        let (common_cap, notify_cap, multiplier, config_cap) = find_regions(device)?;
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
                        .map_err(|_| "cannot get a register BAR")?;
                    let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
                        .map_err(|_| "cannot map a register BAR")?;
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
        let notify = region(notify_cap, 2)?;
        let config = region(config_cap, 1)?;

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
        let device_status = status::ACKNOWLEDGE | status::DRIVER;
        common.write8(common::DEVICE_STATUS, device_status);
        Ok(Self {
            device,
            common,
            notify,
            notify_len: notify_cap.length,
            multiplier,
            config,
            config_len: config_cap.length,
            device_status,
        })
    }

    pub fn device(&self) -> Handle {
        self.device
    }

    /// Marks the device failed (after an error during setup).
    pub fn fail(&self) {
        self.common.write8(common::DEVICE_STATUS, status::FAILED);
    }

    /// Accepts the offered features among `wanted` (virtio 1.x is
    /// required); returns what was accepted.
    pub fn negotiate(&mut self, wanted: u64) -> Result<u64, &'static str> {
        let common = self.common;
        common.write32(common::DEVICE_FEATURE_SELECT, 0);
        let low = common.read32(common::DEVICE_FEATURE);
        common.write32(common::DEVICE_FEATURE_SELECT, 1);
        let high = common.read32(common::DEVICE_FEATURE);
        let offered = (u64::from(high) << 32) | u64::from(low);
        if offered & F_VERSION_1 == 0 {
            return Err("device does not offer virtio 1.x");
        }
        let accepted = offered & (wanted | F_VERSION_1);
        common.write32(common::DRIVER_FEATURE_SELECT, 0);
        common.write32(common::DRIVER_FEATURE, accepted as u32);
        common.write32(common::DRIVER_FEATURE_SELECT, 1);
        common.write32(common::DRIVER_FEATURE, (accepted >> 32) as u32);
        self.device_status |= status::FEATURES_OK;
        common.write8(common::DEVICE_STATUS, self.device_status);
        if common.read8(common::DEVICE_STATUS) & status::FEATURES_OK == 0 {
            return Err("device rejected the features");
        }
        Ok(accepted)
    }

    /// Delivers the device's interrupts (MSI-X entry 0, shared by every
    /// queue set up with `interrupts`) as `bits` on `notification`. Without
    /// MSI-X, returns false: the driver polls.
    pub fn bind_interrupts(&mut self, notification: Handle, bits: u64) -> bool {
        self.common.write16(common::CONFIG_MSIX_VECTOR, NO_VECTOR);
        oceans_rt::device_irq(self.device, 0, notification, bits).is_ok()
    }

    /// Sets up queue `index` with at most `max_size` entries. With
    /// `interrupts`, its completions raise MSI-X entry 0 (if the device
    /// accepts that); returns the queue and whether interrupts are on.
    pub fn queue(
        &mut self,
        index: u16,
        max_size: u16,
        interrupts: bool,
    ) -> Result<(Queue, bool), &'static str> {
        let common = self.common;
        common.write16(common::QUEUE_SELECT, index);
        let maximum = common.read16(common::QUEUE_SIZE);
        if maximum == 0 {
            return Err("queue does not exist");
        }
        // Queue sizes are powers of two: the largest one we use that fits.
        let limit = max_size.clamp(2, MAX_QUEUE_SIZE).min(maximum);
        let size = 1u16 << (15 - limit.leading_zeros());
        let ring = Dma::new(self.device, PAGE)?;
        let layout = Layout::new(size);
        common.write16(common::QUEUE_SIZE, size);
        let interrupts = interrupts && {
            common.write16(common::QUEUE_MSIX_VECTOR, 0);
            common.read16(common::QUEUE_MSIX_VECTOR) == 0
        };
        if !interrupts {
            common.write16(common::QUEUE_MSIX_VECTOR, NO_VECTOR);
        }
        common.write64(common::QUEUE_DESC, ring.device);
        common.write64(common::QUEUE_DRIVER, ring.device + layout.avail as u64);
        common.write64(common::QUEUE_DEVICE, ring.device + layout.used as u64);
        let notify_offset =
            u64::from(common.read16(common::QUEUE_NOTIFY_OFF)) * u64::from(self.multiplier);
        if notify_offset + 2 > u64::from(self.notify_len) {
            return Err("queue notification register outside its region");
        }
        common.write16(common::QUEUE_ENABLE, 1);
        let mut queue = Queue {
            index,
            size,
            ring,
            layout,
            notify: self.notify.at(notify_offset as usize),
            free: [0; MAX_QUEUE_SIZE as usize],
            free_count: 0,
            avail_index: 0,
            used_index: 0,
        };
        for descriptor in (0..size).rev() {
            queue.free[usize::from(queue.free_count)] = descriptor;
            queue.free_count += 1;
        }
        Ok((queue, interrupts))
    }

    /// Setup is complete: the device may start.
    pub fn driver_ok(&mut self) {
        self.device_status |= status::DRIVER_OK;
        self.common
            .write8(common::DEVICE_STATUS, self.device_status);
    }

    /// Reads device-specific configuration consistently (re-reading if the
    /// device changed it meanwhile). `None` if outside the region.
    pub fn config_read(&self, offset: usize, out: &mut [u8]) -> Option<()> {
        if offset + out.len() > self.config_len as usize {
            return None;
        }
        loop {
            let before = self.common.read8(common::CONFIG_GENERATION);
            for (i, byte) in out.iter_mut().enumerate() {
                *byte = self.config.read8(offset + i);
            }
            if self.common.read8(common::CONFIG_GENERATION) == before {
                return Some(());
            }
        }
    }
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

/// Where the rings sit in a queue's page.
#[derive(Clone, Copy)]
struct Layout {
    avail: usize,
    used: usize,
}

impl Layout {
    fn new(size: u16) -> Self {
        let size = usize::from(size);
        let avail = 16 * size;
        let used = (avail + 6 + 2 * size).next_multiple_of(4);
        debug_assert!(used + 6 + 8 * size <= PAGE);
        Self { avail, used }
    }
}

/// One buffer of a descriptor chain.
#[derive(Clone, Copy)]
pub struct Buffer {
    /// Device address.
    pub address: u64,
    pub len: u32,
    /// The device writes it (otherwise it reads it).
    pub device_writes: bool,
}

/// A split virtqueue.
pub struct Queue {
    index: u16,
    size: u16,
    ring: Dma,
    layout: Layout,
    notify: Mmio,
    free: [u16; MAX_QUEUE_SIZE as usize],
    free_count: u16,
    avail_index: u16,
    used_index: u16,
}

impl Queue {
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Free descriptors.
    pub fn free(&self) -> u16 {
        self.free_count
    }

    /// Makes a chain of `buffers` available to the device; returns its head
    /// (the id reported when it is used). `None` if descriptors run out.
    /// Call [`kick`](Self::kick) to tell the device.
    pub fn add(&mut self, buffers: &[Buffer]) -> Option<u16> {
        if buffers.is_empty() || buffers.len() > usize::from(self.free_count) {
            return None;
        }
        let mut ids = [0u16; MAX_QUEUE_SIZE as usize];
        for id in ids.iter_mut().take(buffers.len()) {
            self.free_count -= 1;
            *id = self.free[usize::from(self.free_count)];
        }
        for (i, buffer) in buffers.iter().enumerate() {
            let at = 16 * usize::from(ids[i]);
            let more = i + 1 < buffers.len();
            let flags = if more { DESC_NEXT } else { 0 }
                | if buffer.device_writes { DESC_WRITE } else { 0 };
            self.ring.write(at, buffer.address);
            self.ring.write(at + 8, buffer.len);
            self.ring.write(at + 12, flags);
            self.ring.write(at + 14, if more { ids[i + 1] } else { 0 });
        }
        let head = ids[0];
        let slot = self.layout.avail + 4 + 2 * usize::from(self.avail_index % self.size);
        self.ring.write(slot, head);
        // The chain and its ring entry before the index the device reads.
        fence(Ordering::SeqCst);
        self.avail_index = self.avail_index.wrapping_add(1);
        self.ring.write(self.layout.avail + 2, self.avail_index);
        Some(head)
    }

    /// Tells the device new buffers are available.
    pub fn kick(&self) {
        fence(Ordering::SeqCst);
        self.notify.write16(0, self.index);
    }

    /// The next chain the device has finished with: its head and the bytes
    /// the device wrote. Its descriptors are free again.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        let device_index: u16 = self.ring.read(self.layout.used + 2);
        if device_index == self.used_index {
            return None;
        }
        fence(Ordering::SeqCst);
        let slot = self.layout.used + 4 + 8 * usize::from(self.used_index % self.size);
        let head: u32 = self.ring.read(slot);
        let len: u32 = self.ring.read(slot + 4);
        self.used_index = self.used_index.wrapping_add(1);
        // A device reporting a descriptor it was never given is ignored.
        let head = u16::try_from(head).ok().filter(|&h| h < self.size)?;
        let mut id = head;
        for _ in 0..self.size {
            if usize::from(self.free_count) >= usize::from(self.size)
                || self.free[..usize::from(self.free_count)].contains(&id)
            {
                break;
            }
            self.free[usize::from(self.free_count)] = id;
            self.free_count += 1;
            let at = 16 * usize::from(id);
            let flags: u16 = self.ring.read(at + 12);
            if flags & DESC_NEXT == 0 {
                break;
            }
            id = self.ring.read::<u16>(at + 14) % self.size;
        }
        Some((head, len))
    }
}
