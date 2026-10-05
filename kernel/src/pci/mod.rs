//! PCI devices (ADR-0021): enumeration through ECAM and the capabilities
//! userspace drivers hold.
//!
//! The kernel finds functions and sizes their BARs at boot, then gets out of
//! the way: drivers are userspace services. A driver holds a *device*
//! capability for exactly one function, opened exclusively, through which
//! it can
//!
//! - read configuration space (writes are the kernel's: only the command
//!   bits a driver needs are set, through `DEVICE_ENABLE`);
//! - map its memory BARs uncached, without the MSI-X table pages;
//! - allocate contiguous DMA memory, kept alive until DMA is switched off;
//! - bind MSI-X vectors to notifications (the kernel programs the table).
//!
//! Closing the device (or the driver dying) masks its interrupts, turns
//! off bus mastering and memory decoding, and only then releases its DMA
//! memory, so a device can never write into memory that has been reused.
//! Endpoints are found with bus mastering off: no device DMAs before a
//! driver asks for it.
//!
//! Without an IOMMU (not yet supported), a driver that can program DMA can
//! reach all of physical memory through its device: drivers are trusted
//! services, and the device capability is the authority that says so.

mod msi;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::ops::Range;
use core::sync::atomic::{AtomicBool, Ordering};

use oceans_abi::device::DeviceRecord;
use oceans_acpi::EcamRegion;
use oceans_memory_map::{PAGE_SIZE, RegionKind};
use oceans_pci::{Bar, ConfigSpace, Header, Msix, command, reg};
use spin::{Mutex, Once};

use crate::acpi::Acpi;
use crate::ipc::Notification;
use crate::memory::paging;
use crate::object::{MAX_HOLES, MemoryObject};
use crate::{arch, boot, klog};

/// Bytes of configuration space per bus (32 devices × 8 functions × 4 KiB).
const BUS_SPAN: u64 = 1 << 20;
/// Bridges nest at most this deep (guards against firmware loops).
const MAX_BRIDGE_DEPTH: usize = 8;
/// Largest BAR a driver can map, and DMA memory per open device.
const MAX_BAR_SIZE: u64 = 1 << 30;
const MAX_DMA_PER_DEVICE: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceError {
    NotFound,
    Busy,
    /// Not a memory BAR, too small to map alone, or overlapping RAM.
    BadBar,
    /// The function has no MSI-X, or the entry does not exist.
    NoInterrupt,
    OutOfMemory,
    BadArgument,
}

impl From<DeviceError> for oceans_abi::Error {
    fn from(error: DeviceError) -> Self {
        use oceans_abi::Error;
        match error {
            DeviceError::NotFound => Error::NotFound,
            DeviceError::Busy => Error::Busy,
            DeviceError::OutOfMemory => Error::OutOfMemory,
            DeviceError::BadBar | DeviceError::NoInterrupt | DeviceError::BadArgument => {
                Error::InvalidArgument
            }
        }
    }
}

/// One function's configuration space, through its ECAM mapping.
struct Ecam(usize);

impl ConfigSpace for Ecam {
    fn read32(&self, offset: u16) -> u32 {
        debug_assert!(offset.is_multiple_of(4) && offset < oceans_pci::CONFIG_SPACE_SIZE);
        // SAFETY: `self.0` maps the function's 4 KiB configuration space
        // uncached (`map_bus`); `offset` is aligned and inside it.
        unsafe { ((self.0 + usize::from(offset)) as *const u32).read_volatile() }
    }

    fn write32(&mut self, offset: u16, value: u32) {
        debug_assert!(offset.is_multiple_of(4) && offset < oceans_pci::CONFIG_SPACE_SIZE);
        // SAFETY: as for `read32`.
        unsafe { ((self.0 + usize::from(offset)) as *mut u32).write_volatile(value) }
    }

    fn write16(&mut self, offset: u16, value: u16) {
        debug_assert!(offset.is_multiple_of(2) && offset < oceans_pci::CONFIG_SPACE_SIZE);
        // SAFETY: as for `read32`; ECAM supports 16-bit accesses.
        unsafe { ((self.0 + usize::from(offset)) as *mut u16).write_volatile(value) }
    }
}

/// A PCI function found at boot.
pub struct Function {
    segment: u16,
    bus: u8,
    slot: u8,
    function: u8,
    header: Header,
    bars: [Bar; 6],
    msix: Option<Msix>,
    /// Kernel address of the configuration space.
    config: usize,
    /// Serialises read-modify-write of control registers.
    control: Mutex<()>,
    open: AtomicBool,
    /// Kernel mapping of the MSI-X table, made on first use (`None`: the
    /// table BAR is unusable).
    msix_table: Once<Option<usize>>,
}

impl Function {
    fn ecam(&self) -> Ecam {
        Ecam(self.config)
    }

    fn record(&self) -> DeviceRecord {
        DeviceRecord {
            segment: self.segment,
            bus: self.bus,
            slot: self.slot,
            function: self.function,
            vendor: self.header.vendor,
            device: self.header.device,
            class: self.header.class,
            subclass: self.header.subclass,
            prog_if: self.header.prog_if,
            revision: self.header.revision,
            msix_vectors: self.msix.map_or(0, |m| m.entries),
            open: self.open.load(Ordering::Acquire),
        }
    }

    /// Sets and clears command register bits.
    fn update_command(&self, set: u16, clear: u16) {
        arch::without_interrupts(|| {
            let _guard = self.control.lock();
            let mut ecam = self.ecam();
            let value = ecam.read16(reg::COMMAND);
            ecam.write16(reg::COMMAND, (value | set) & !clear);
        });
    }

    fn memory_bar(&self, index: usize) -> Option<(u64, u64)> {
        match self.bars.get(index)? {
            Bar::Memory { base, size, .. } => Some((*base, *size)),
            _ => None,
        }
    }

    /// The MSI-X table, mapped into the kernel.
    fn msix_table(&self) -> Option<usize> {
        *self.msix_table.call_once(|| {
            let msix = self.msix?;
            let (bar, offset, len) = msix.table();
            let (base, size) = self.memory_bar(usize::from(bar))?;
            if offset.checked_add(len)? > size {
                return None;
            }
            paging::map_mmio(base + offset, len)
                .ok()
                .map(|ptr| ptr as usize)
        })
    }
}

/// Writes MSI-X table entry `entry`: (address, data, masked).
fn write_msix_entry(table: usize, entry: u16, message: Option<(u64, u32)>) {
    let at = table + usize::from(entry) * Msix::ENTRY_SIZE as usize;
    let write = |offset: usize, value: u32| {
        // SAFETY: `table` maps the whole MSI-X table uncached
        // (`Function::msix_table`) and `entry` is below its entry count.
        unsafe { ((at + offset) as *mut u32).write_volatile(value) }
    };
    // Mask while changing the message, so no half-written one is sent.
    write(12, 1);
    if let Some((address, data)) = message {
        write(0, address as u32);
        write(4, (address >> 32) as u32);
        write(8, data);
        write(12, 0);
    }
}

static FUNCTIONS: Once<Vec<Arc<Function>>> = Once::new();

fn functions() -> &'static [Arc<Function>] {
    FUNCTIONS.get().map_or(&[], Vec::as_slice)
}

/// Enumerates every PCI function reachable through the ACPI MCFG.
pub fn init(acpi: Option<&Acpi>) {
    arch::set_device_handler(msi::dispatch);
    let mut found = Vec::new();
    for region in acpi.map_or(&[][..], Acpi::ecam_regions) {
        let mut visited = [false; 256];
        scan_bus(region, region.start_bus, 0, &mut visited, &mut found);
    }
    for function in &found {
        let header = &function.header;
        let mut line = alloc::string::String::new();
        let _ = write!(
            line,
            "pci {:04x}:{:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x}",
            function.segment,
            function.bus,
            function.slot,
            function.function,
            header.vendor,
            header.device,
            header.class,
            header.subclass
        );
        if let Some(msix) = function.msix {
            let _ = write!(line, ", {} MSI-X vectors", msix.entries);
        }
        klog::info!("{line}");
    }
    klog::info!("PCI: {} function(s)", found.len());
    FUNCTIONS.call_once(|| found);
}

fn scan_bus(
    region: &EcamRegion,
    bus: u8,
    depth: usize,
    visited: &mut [bool; 256],
    found: &mut Vec<Arc<Function>>,
) {
    if visited[usize::from(bus)] || depth > MAX_BRIDGE_DEPTH {
        return;
    }
    visited[usize::from(bus)] = true;
    let Some(phys) = region.function_address(bus, 0, 0) else {
        return;
    };
    let base = match paging::map_mmio(phys, BUS_SPAN) {
        Ok(base) => base as usize,
        Err(err) => {
            klog::warn!("cannot map PCI bus {bus:#x} configuration space: {err:?}");
            return;
        }
    };
    for slot in 0..32u8 {
        for function in 0..8u8 {
            let config = base + (usize::from(slot) << 15) + (usize::from(function) << 12);
            let mut ecam = Ecam(config);
            let Some(header) = oceans_pci::header(&ecam) else {
                if function == 0 {
                    break;
                }
                continue;
            };
            let bars = match header.kind {
                0 => oceans_pci::size_bars(&mut ecam, 6),
                1 => oceans_pci::size_bars(&mut ecam, 2),
                _ => [Bar::None; 6],
            };
            if header.kind == 0 {
                // No DMA until a driver enables it.
                let value = ecam.read16(reg::COMMAND);
                ecam.write16(reg::COMMAND, value & !command::BUS_MASTER);
            }
            if header.is_bridge() {
                let secondary = oceans_pci::secondary_bus(&ecam);
                if secondary > bus {
                    scan_bus(region, secondary, depth + 1, visited, found);
                }
            }
            found.push(Arc::new(Function {
                segment: region.segment,
                bus,
                slot,
                function,
                header,
                bars,
                msix: oceans_pci::msix(&ecam),
                config,
                control: Mutex::new(()),
                open: AtomicBool::new(false),
                msix_table: Once::new(),
            }));
            if function == 0 && !header.multifunction {
                break;
            }
        }
    }
}

/// Records of every function, for `DEVICE_LIST`.
pub fn records() -> Vec<DeviceRecord> {
    functions().iter().map(|f| f.record()).collect()
}

/// Opens the `index`th endpoint with this vendor and device ID,
/// exclusively.
pub fn open(vendor: u16, device: u16, index: u64) -> Result<Arc<Device>, DeviceError> {
    open_matching(index, |h| h.vendor == vendor && h.device == device)
}

/// Opens the `index`th function whose class, subclass and programming
/// interface are `class` (`0xCCSSPP`), exclusively (ADR-0032).
pub fn open_class(class: u32, index: u64) -> Result<Arc<Device>, DeviceError> {
    open_matching(index, |h| {
        u32::from(h.class) << 16 | u32::from(h.subclass) << 8 | u32::from(h.prog_if) == class
    })
}

fn open_matching(
    index: u64,
    matches: impl Fn(&oceans_pci::Header) -> bool,
) -> Result<Arc<Device>, DeviceError> {
    let function = functions()
        .iter()
        .filter(|f| f.header.kind == 0 && matches(&f.header))
        .nth(usize::try_from(index).map_err(|_| DeviceError::NotFound)?)
        .ok_or(DeviceError::NotFound)?;
    function
        .open
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| DeviceError::Busy)?;
    Ok(Arc::new(Device {
        function: function.clone(),
        dma: Mutex::new(Vec::new()),
        irqs: Mutex::new(Vec::new()),
    }))
}

/// An open device: what a driver's device capability names.
pub struct Device {
    function: Arc<Function>,
    /// DMA memory handed out, released only after DMA is switched off.
    dma: Mutex<Vec<Arc<MemoryObject>>>,
    /// Bound MSI-X entries and their vectors.
    irqs: Mutex<Vec<(u16, u8)>>,
}

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let header = &self.function.header;
        write!(f, "Device({:04x}:{:04x})", header.vendor, header.device)
    }
}

impl Device {
    /// Reads configuration space.
    pub fn config_read(&self, offset: u64, width: u64) -> Result<u32, DeviceError> {
        let offset = u16::try_from(offset).map_err(|_| DeviceError::BadArgument)?;
        let width = u8::try_from(width).map_err(|_| DeviceError::BadArgument)?;
        oceans_pci::read(&self.function.ecam(), offset, width).ok_or(DeviceError::BadArgument)
    }

    /// Memory decoding and bus mastering on, legacy interrupts off.
    pub fn enable(&self) {
        self.function.update_command(
            command::MEMORY_SPACE | command::BUS_MASTER | command::INTX_DISABLE,
            0,
        );
    }

    /// A memory object for memory BAR `index`, without the MSI-X pages.
    pub fn bar(&self, index: u64) -> Result<Arc<MemoryObject>, DeviceError> {
        let index = usize::try_from(index).map_err(|_| DeviceError::BadBar)?;
        let (base, size) = self.function.memory_bar(index).ok_or(DeviceError::BadBar)?;
        if size == 0 || size > MAX_BAR_SIZE {
            return Err(DeviceError::BadBar);
        }
        // The object covers whole pages. A BAR smaller than a page, or not
        // page-aligned (some AHCI controllers' 2 KiB ABAR), takes the pages
        // around it, so only if no other BAR decodes there: a driver must
        // never reach another device's registers. The driver finds its
        // registers at `base % PAGE_SIZE` in the object.
        let (pages, exact) = oceans_pci::page_span(base, size, PAGE_SIZE);
        let (start, end) = (pages.start, pages.end);
        if !exact && shares_pages(&self.function, index, pages) {
            klog::warn!("refusing BAR at {base:#x}: another device's BAR shares its page");
            return Err(DeviceError::BadBar);
        }
        if overlaps_ram(start..end) {
            klog::warn!("refusing BAR at {base:#x}: overlaps RAM in the memory map");
            return Err(DeviceError::BadBar);
        }
        let mut holes: [Range<u64>; MAX_HOLES] = [0..0, 0..0];
        if let Some(msix) = self.function.msix {
            for (hole, (bar, offset, len)) in
                holes.iter_mut().zip([msix.table(), msix.pending_bits()])
            {
                if usize::from(bar) == index {
                    // Offsets in the BAR, as offsets in the object.
                    let offset = offset + (base - start);
                    let first = offset - offset % PAGE_SIZE;
                    *hole = first..(offset + len).next_multiple_of(PAGE_SIZE);
                }
            }
        }
        Ok(MemoryObject::new_device(start, end - start, holes))
    }

    /// Contiguous DMA memory and the address the device uses for it.
    pub fn dma_create(&self, size: u64) -> Result<(Arc<MemoryObject>, u64), DeviceError> {
        let object = MemoryObject::new_contiguous(size).map_err(|err| match err {
            crate::object::ObjectError::OutOfBounds => DeviceError::BadArgument,
            _ => DeviceError::OutOfMemory,
        })?;
        let address = object.contiguous_base().expect("contiguous");
        arch::without_interrupts(|| {
            let mut dma = self.dma.lock();
            let total: u64 = dma.iter().map(|m| m.size()).sum();
            if total + object.size() > MAX_DMA_PER_DEVICE {
                return Err(DeviceError::OutOfMemory);
            }
            dma.push(object.clone());
            Ok(())
        })?;
        // No IOMMU: device addresses are physical addresses.
        Ok((object, address))
    }

    /// Delivers MSI-X `entry` as `bits` on `notification`.
    pub fn bind_irq(
        &self,
        entry: u64,
        notification: Arc<Notification>,
        bits: u64,
    ) -> Result<(), DeviceError> {
        let function = &self.function;
        let msix = function.msix.ok_or(DeviceError::NoInterrupt)?;
        let entry = u16::try_from(entry)
            .ok()
            .filter(|&e| e < msix.entries)
            .ok_or(DeviceError::NoInterrupt)?;
        if bits == 0 {
            return Err(DeviceError::BadArgument);
        }
        let table = function.msix_table().ok_or(DeviceError::NoInterrupt)?;

        let bound = arch::without_interrupts(|| {
            self.irqs
                .lock()
                .iter()
                .find(|&&(e, _)| e == entry)
                .map(|&(_, vector)| vector)
        });
        let vector = match bound {
            Some(vector) => {
                msi::rebind(vector, notification, bits);
                vector
            }
            None => {
                let vector = msi::allocate(notification, bits).ok_or(DeviceError::OutOfMemory)?;
                arch::without_interrupts(|| self.irqs.lock().push((entry, vector)));
                vector
            }
        };
        write_msix_entry(table, entry, Some(arch::msi_message(vector)));

        // MSI-X on (unbound entries stay masked), legacy interrupts off.
        arch::without_interrupts(|| {
            let _guard = function.control.lock();
            let mut ecam = function.ecam();
            let control = ecam.read16(msix.control());
            ecam.write16(
                msix.control(),
                (control | Msix::ENABLE) & !Msix::FUNCTION_MASK,
            );
            let value = ecam.read16(reg::COMMAND);
            ecam.write16(reg::COMMAND, value | command::INTX_DISABLE);
        });
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let function = &self.function;
        // 1. Silence interrupts.
        let irqs = arch::without_interrupts(|| core::mem::take(&mut *self.irqs.lock()));
        if let (Some(msix), Some(table)) =
            (function.msix, function.msix_table.get().copied().flatten())
        {
            for &(entry, _) in &irqs {
                write_msix_entry(table, entry, None);
            }
            arch::without_interrupts(|| {
                let _guard = function.control.lock();
                let mut ecam = function.ecam();
                let control = ecam.read16(msix.control());
                ecam.write16(
                    msix.control(),
                    (control & !Msix::ENABLE) | Msix::FUNCTION_MASK,
                );
            });
        }
        for &(_, vector) in &irqs {
            msi::release(vector);
        }
        // 2. Stop DMA and register decoding.
        function.update_command(
            command::INTX_DISABLE,
            command::BUS_MASTER | command::MEMORY_SPACE,
        );
        // 3. Only now may the DMA memory be reused.
        drop(arch::without_interrupts(|| {
            core::mem::take(&mut *self.dma.lock())
        }));
        function.open.store(false, Ordering::Release);
    }
}

/// Whether `range` overlaps RAM (or the kernel) in the boot memory map.
/// Whether a memory BAR other than `function`'s BAR `index` decodes
/// anywhere in `pages`.
fn shares_pages(function: &Arc<Function>, index: usize, pages: Range<u64>) -> bool {
    functions().iter().any(|other| {
        (0..other.bars.len()).any(|i| {
            if Arc::ptr_eq(other, function) && i == index {
                return false;
            }
            other.memory_bar(i).is_some_and(|(base, size)| {
                size > 0 && base < pages.end && pages.start < base + size
            })
        })
    })
}

fn overlaps_ram(range: Range<u64>) -> bool {
    boot::info().memory_regions().iter().any(|region| {
        let ram = !matches!(
            region.kind(),
            RegionKind::Reserved | RegionKind::Framebuffer
        );
        ram && region.base() < range.end && range.start < region.end()
    })
}

/// Device capability checks, for smoke-test boots: exclusive open, kernel
/// refusal to touch device memory, contiguous DMA, and release on close.
pub fn self_test() {
    const VIRTIO_BLOCK: (u16, u16) = (0x1af4, 0x1042);
    if functions().is_empty() {
        klog::warn!("PCI self-test skipped: no PCI functions");
        return;
    }
    let Ok(device) = open(VIRTIO_BLOCK.0, VIRTIO_BLOCK.1, 0) else {
        klog::warn!("PCI self-test skipped: no virtio block device");
        return;
    };
    assert_eq!(
        open(VIRTIO_BLOCK.0, VIRTIO_BLOCK.1, 0).err(),
        Some(DeviceError::Busy),
        "devices open exclusively"
    );
    assert_eq!(device.config_read(0, 2), Ok(u32::from(VIRTIO_BLOCK.0)));
    assert_eq!(device.config_read(1, 2), Err(DeviceError::BadArgument));
    assert_eq!(device.config_read(4096, 1), Err(DeviceError::BadArgument));

    let bar = (0..6)
        .find_map(|index| device.bar(index).ok())
        .expect("virtio block device has a memory BAR");
    let mut byte = [0u8; 1];
    assert!(
        bar.read(0, &mut byte).is_err(),
        "kernel must not read device memory"
    );
    assert_eq!(bar.cache(), paging::Cache::Uncached);
    assert!(
        bar.claim_mapping(false, true).is_err(),
        "device memory is never executable"
    );
    if let Some(msix) = device.function.msix {
        let (table_bar, offset, _) = msix.table();
        if let Ok(table) = device.bar(u64::from(table_bar)) {
            assert_eq!(
                table.page(offset / PAGE_SIZE),
                None,
                "MSI-X table is a hole"
            );
        }
    }

    let (dma, address) = device.dma_create(3 * PAGE_SIZE).expect("DMA memory");
    assert_eq!(dma.size(), 4 * PAGE_SIZE, "rounded to a power of two");
    for page in 0..4 {
        assert_eq!(
            dma.page(page),
            Some(address + page * PAGE_SIZE),
            "contiguous"
        );
    }
    let alive = Arc::downgrade(&dma);
    drop(dma);
    assert!(
        alive.upgrade().is_some(),
        "DMA memory outlives its handle while the device is open"
    );
    drop(device);
    assert!(
        alive.upgrade().is_none(),
        "DMA memory released when the device closes"
    );
    drop(open(VIRTIO_BLOCK.0, VIRTIO_BLOCK.1, 0).expect("reopen after close"));
    assert_eq!(open(0xffff, 0xffff, 0).err(), Some(DeviceError::NotFound));
    klog::info!("PCI self-test passed");
}
