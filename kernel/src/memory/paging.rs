//! Kernel address space (ADR-0009).
//!
//! Builds the kernel's own page tables at boot, replacing the bootloader's:
//!
//! - the kernel image, each segment with exact permissions (W^X);
//! - a direct map of RAM (not MMIO, not the kernel image) using the largest
//!   pages possible, read-write and never executable;
//! - kernel stacks in their own region, each with an unmapped guard page.

use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};

use oceans_memory_map::{PAGE_SIZE, RegionKind};
use spin::{Mutex, Once};

use super::layout::{self, DIRECT_MAP, KERNEL_IMAGE, KERNEL_STACKS};
use crate::arch::{self, AddressSpace, MAX_CPUS};
use crate::boot::{BootInfo, KernelImage};
use crate::klog;

/// Page sizes the architecture can map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    Size4KiB,
    Size2MiB,
    Size1GiB,
}

impl PageSize {
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Size4KiB => 4 << 10,
            Self::Size2MiB => 2 << 20,
            Self::Size1GiB => 1 << 30,
        }
    }
}

/// Memory type of a mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    /// Normal RAM.
    WriteBack,
    /// Device registers (MMIO).
    Uncached,
}

/// Access rights of a mapping. Readable is implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapFlags {
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    /// Same in every address space; kept in the TLB across switches.
    pub global: bool,
    pub cache: Cache,
}

impl MapFlags {
    pub const KERNEL_CODE: Self = Self::kernel(false, true);
    pub const KERNEL_RODATA: Self = Self::kernel(false, false);
    pub const KERNEL_DATA: Self = Self::kernel(true, false);

    const fn kernel(writable: bool, executable: bool) -> Self {
        Self {
            writable,
            executable,
            user: false,
            global: true,
            cache: Cache::WriteBack,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Address not aligned to the page size, or not canonical.
    Misaligned { virt: u64, phys: u64 },
    /// A mapping already exists at this address.
    AlreadyMapped(u64),
    /// A larger page already covers this address.
    HugePageConflict(u64),
    /// No frame for a page table.
    OutOfMemory,
    /// Nothing is mapped at this address.
    NotMapped(u64),
}

/// Size of the kernel stack each thread gets.
pub const KERNEL_STACK_SIZE: u64 = 64 * 1024;
/// Address range reserved per stack: one guard page below the stack, the
/// rest is unused virtual space that separates stacks further.
const STACK_SLOT_SIZE: u64 = 1024 * 1024;

static KERNEL_SPACE: Once<Mutex<AddressSpace>> = Once::new();
static NEXT_STACK_SLOT: AtomicU64 = AtomicU64::new(0);

/// Builds and activates the kernel address space.
pub fn init(boot: &BootInfo) {
    let features = arch::cpu_features();
    arch::enable_protections(&features);

    let Some(image) = boot.kernel_image() else {
        panic!("bootloader did not report the kernel's load address");
    };
    let direct_map_offset = boot
        .direct_map_offset()
        .expect("checked by memory::discover");
    let mut space = AddressSpace::new().unwrap_or_else(|err| panic!("page tables: {err:?}"));

    map_kernel_image(&mut space, image);
    let mapped = map_direct_map(&mut space, boot, direct_map_offset, features.gigabyte_pages);

    // Boot modules (program images) usually sit in kernel+modules memory,
    // which the direct map skips: map those pages, read-only. A module
    // Limine checked against a hash (ADR-0091) stays in the reclaimable
    // buffer it hashed, already in the direct map: those pages are skipped.
    for module in boot.modules() {
        let start = module.physical_base - module.physical_base % PAGE_SIZE;
        let end = (module.physical_base + module.size).next_multiple_of(PAGE_SIZE);
        let mut page = start;
        while page < end {
            if space.translate(direct_map_offset + page).is_some() {
                page += PAGE_SIZE;
                continue;
            }
            let gap = page;
            while page < end && space.translate(direct_map_offset + page).is_none() {
                page += PAGE_SIZE;
            }
            map_range(
                &mut space,
                direct_map_offset + gap,
                gap,
                page - gap,
                MapFlags::KERNEL_RODATA,
                false,
            )
            .unwrap_or_else(|err| panic!("mapping boot module {}: {err:?}", module.name()));
        }
    }

    // Every top-level kernel slot that will ever be used exists now, so
    // address spaces created later can share the kernel half by copying the
    // top-level entries once.
    for region in [layout::KERNEL_HEAP, KERNEL_STACKS, layout::MMIO] {
        space
            .prepare_top_level(region.start)
            .unwrap_or_else(|err| panic!("page tables: {err:?}"));
    }

    // SAFETY: the new tables map the running kernel image at the same
    // addresses with permissions that allow everything it does, and the
    // direct map at the bootloader's offset, which covers the current stack
    // (bootloader-reclaimable memory), the frame metadata and all page tables.
    unsafe { space.activate() };

    klog::info!(
        "kernel address space active: root {:#x}, direct map {} MiB using pages up to {}",
        space.root(),
        mapped / (1024 * 1024),
        if features.gigabyte_pages {
            "1 GiB"
        } else {
            "2 MiB"
        },
    );
    KERNEL_SPACE.call_once(|| Mutex::new(space));
}

/// A kernel stack mapped in the stack region, with an unmapped guard page
/// below it so an overflow faults instead of corrupting memory. Dropping it
/// unmaps the stack and returns its frames, so it must not be the stack the
/// CPU is running on.
pub struct KernelStack {
    slot: u64,
}

impl KernelStack {
    const fn bottom(slot: u64) -> u64 {
        KERNEL_STACKS.start + (slot + 1) * STACK_SLOT_SIZE - KERNEL_STACK_SIZE
    }

    /// Initial stack pointer (16-byte aligned).
    pub const fn top(&self) -> u64 {
        Self::bottom(self.slot) + KERNEL_STACK_SIZE
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        unmap_stack_pages(Self::bottom(self.slot), KERNEL_STACK_SIZE);
        // Another CPU may still cache the old translation (kernel pages are
        // global): the slot waits until every CPU has flushed (ADR-0089).
        let generation = STACK_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        arch::without_interrupts(|| FREE_STACK_SLOTS.lock().push((self.slot, generation)));
    }
}

/// Slots of freed stacks, each with the generation it was freed in; reused
/// before new ones are taken, once no CPU can still translate them.
static FREE_STACK_SLOTS: Mutex<alloc::vec::Vec<(u64, u64)>> = Mutex::new(alloc::vec::Vec::new());
/// Counts freed stacks: the newest generation a CPU must flush past.
static STACK_GENERATION: AtomicU64 = AtomicU64::new(0);
/// The generation each CPU had flushed its TLB past (`u64::MAX`: a CPU not
/// running, which caches nothing).
static FLUSHED: [AtomicU64; MAX_CPUS] = {
    let mut flushed = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
    flushed[0] = AtomicU64::new(0);
    flushed
};

/// CPU `index` starts translating kernel addresses (before its first
/// kernel-stack use): it has flushed nothing yet.
pub fn cpu_online(index: usize) {
    FLUSHED[index].store(STACK_GENERATION.load(Ordering::Acquire), Ordering::Release);
}

/// On every timer tick of every CPU: if stacks were freed since this CPU
/// last flushed its TLB, it flushes now, so their slots can be reused.
pub fn tlb_tick() {
    let cpu = arch::cpu_index();
    let generation = STACK_GENERATION.load(Ordering::Acquire);
    if FLUSHED[cpu].load(Ordering::Relaxed) < generation {
        arch::flush_tlb_all();
        FLUSHED[cpu].store(generation, Ordering::Release);
    }
}

/// The oldest generation every running CPU has flushed past.
fn flushed_everywhere() -> u64 {
    FLUSHED
        .iter()
        .map(|flushed| flushed.load(Ordering::Acquire))
        .min()
        .unwrap_or(u64::MAX)
}

/// A freed slot no CPU can still translate, if there is one.
fn reusable_slot() -> Option<u64> {
    let safe = flushed_everywhere();
    arch::without_interrupts(|| {
        let mut free = FREE_STACK_SLOTS.lock();
        let at = free
            .iter()
            .position(|&(_, generation)| generation <= safe)?;
        Some(free.swap_remove(at).0)
    })
}

/// Allocates and maps a new kernel stack.
pub fn allocate_kernel_stack() -> Result<KernelStack, MapError> {
    let reused = reusable_slot();
    let slot = match reused {
        Some(slot) => slot,
        None => {
            let slot = NEXT_STACK_SLOT.fetch_add(1, Ordering::Relaxed);
            if KERNEL_STACKS.start + (slot + 1) * STACK_SLOT_SIZE > KERNEL_STACKS.end {
                return Err(MapError::OutOfMemory);
            }
            slot
        }
    };
    let bottom = KernelStack::bottom(slot);

    let mut mapped = 0;
    let result = with_kernel_space(|space| {
        for page in (bottom..bottom + KERNEL_STACK_SIZE).step_by(PAGE_SIZE as usize) {
            let frame = super::frames::allocate_frames(0).map_err(|_| MapError::OutOfMemory)?;
            if let Err(err) = space.map(
                page,
                frame.addr(),
                PageSize::Size4KiB,
                MapFlags::KERNEL_DATA,
            ) {
                let _ = super::frames::free_frames(frame);
                return Err(err);
            }
            mapped += PAGE_SIZE;
        }
        Ok(())
    });
    match result {
        Ok(()) => Ok(KernelStack { slot }),
        Err(err) => {
            // Undo the partial stack so neither frames nor the slot leak;
            // the slot waits for every CPU's flush like any freed one.
            unmap_stack_pages(bottom, mapped);
            let generation = STACK_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
            arch::without_interrupts(|| FREE_STACK_SLOTS.lock().push((slot, generation)));
            Err(err)
        }
    }
}

fn unmap_stack_pages(bottom: u64, len: u64) {
    with_kernel_space(|space| {
        for page in (bottom..bottom + len).step_by(PAGE_SIZE as usize) {
            let phys = space
                .unmap(page)
                .unwrap_or_else(|err| panic!("kernel stack page {page:#x}: {err:?}"));
            let frame = oceans_frame_allocator::Frame::from_addr(phys).expect("page-aligned");
            if let Err(err) = super::frames::free_frames(frame) {
                panic!("kernel stack frame {phys:#x} rejected on free: {err:?}");
            }
        }
    });
}

/// Next free address in the MMIO region. MMIO mappings are permanent.
static NEXT_MMIO: AtomicU64 = AtomicU64::new(layout::MMIO.start);

/// Maps device registers at physical `phys..phys + size` uncached,
/// read-write, never executable. Returns the virtual address of `phys`.
pub fn map_mmio(phys: u64, size: u64) -> Result<*mut u8, MapError> {
    map_physical(phys, size, true)
}

/// Maps firmware memory outside the direct map (e.g. ACPI tables in
/// reserved memory) uncached and read-only. Permanent.
pub fn map_physical_readonly(phys: u64, size: u64) -> Result<*const u8, MapError> {
    map_physical(phys, size, false).map(<*mut u8>::cast_const)
}

fn map_physical(phys: u64, size: u64, writable: bool) -> Result<*mut u8, MapError> {
    let start = phys - phys % PAGE_SIZE;
    let end = (phys + size).next_multiple_of(PAGE_SIZE);
    let virt = NEXT_MMIO.fetch_add(end - start, Ordering::Relaxed);
    if virt + (end - start) > layout::MMIO.end {
        return Err(MapError::OutOfMemory);
    }
    let flags = MapFlags {
        writable,
        executable: false,
        user: false,
        global: true,
        cache: Cache::Uncached,
    };
    with_kernel_space(|space| {
        for offset in (0..end - start).step_by(PAGE_SIZE as usize) {
            space.map(virt + offset, start + offset, PageSize::Size4KiB, flags)?;
        }
        Ok(())
    })?;
    Ok((virt + (phys - start)) as *mut u8)
}

/// A new user address space sharing the kernel half.
pub fn new_user_address_space() -> Result<AddressSpace, MapError> {
    with_kernel_space(|kernel| AddressSpace::new_user(kernel))
}

/// Root of the kernel address space (kernel threads run in it).
pub fn kernel_root() -> u64 {
    with_kernel_space(|kernel| kernel.root())
}

/// Physical address and flags `virt` maps to in the kernel address space.
pub fn translate(virt: u64) -> Option<(u64, MapFlags)> {
    with_kernel_space(|space| space.translate(virt))
}

fn with_kernel_space<R>(f: impl FnOnce(&mut AddressSpace) -> R) -> R {
    let space = KERNEL_SPACE.get().expect("paging::init runs first");
    arch::without_interrupts(|| f(&mut space.lock()))
}

unsafe extern "C" {
    static __kernel_start: u8;
    static __requests_start: u8;
    static __requests_end: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
}

/// Kernel image segments with their permissions, from the linker script.
pub fn kernel_segments() -> [(Range<u64>, MapFlags); 4] {
    let addr = |symbol: *const u8| symbol as u64;
    [
        (
            addr(&raw const __requests_start)..addr(&raw const __requests_end),
            MapFlags::KERNEL_DATA,
        ),
        (
            addr(&raw const __text_start)..addr(&raw const __text_end),
            MapFlags::KERNEL_CODE,
        ),
        (
            addr(&raw const __rodata_start)..addr(&raw const __rodata_end),
            MapFlags::KERNEL_RODATA,
        ),
        (
            addr(&raw const __data_start)..addr(&raw const __data_end),
            MapFlags::KERNEL_DATA,
        ),
    ]
}

fn map_kernel_image(space: &mut AddressSpace, image: KernelImage) {
    let linked_at = &raw const __kernel_start as u64;
    if image.virtual_base != linked_at {
        panic!(
            "kernel loaded at {:#x} but linked at {linked_at:#x}",
            image.virtual_base
        );
    }
    for (segment, flags) in kernel_segments() {
        assert!(
            KERNEL_IMAGE.start <= segment.start && segment.end <= KERNEL_IMAGE.end,
            "kernel segment {segment:#x?} outside the kernel image region"
        );
        let phys = image.physical_base + (segment.start - image.virtual_base);
        map_range(
            space,
            segment.start,
            phys,
            segment.end - segment.start,
            flags,
            false,
        )
        .unwrap_or_else(|err| panic!("mapping kernel segment {segment:#x?}: {err:?}"));
    }
}

/// Maps RAM at `offset + phys`. Returns the number of bytes mapped.
fn map_direct_map(
    space: &mut AddressSpace,
    boot: &BootInfo,
    offset: u64,
    gigabyte_pages: bool,
) -> u64 {
    // The bootloader's offset is kept: frame metadata pointers already use it.
    if !DIRECT_MAP.contains(&offset) || !offset.is_multiple_of(PageSize::Size2MiB.bytes()) {
        panic!(
            "bootloader direct map offset {offset:#x} is outside the reserved region or unaligned"
        );
    }

    let mut mapped = 0;
    let mut flush = |range: Range<u64>, space: &mut AddressSpace| {
        let len = range.end - range.start;
        let virt = offset.checked_add(range.start);
        let fits = virt
            .and_then(|v| v.checked_add(len))
            .is_some_and(|end| end <= DIRECT_MAP.end);
        let (true, Some(virt)) = (fits, virt) else {
            panic!("RAM at {range:#x?} does not fit in the direct map region");
        };
        map_range(
            space,
            virt,
            range.start,
            len,
            MapFlags::KERNEL_DATA,
            gigabyte_pages,
        )
        .unwrap_or_else(|err| panic!("direct map {range:#x?}: {err:?}"));
        mapped += len;
    };

    // RAM only: MMIO is mapped on demand with the right cache type, and the
    // kernel image is reachable only through its own W^X mapping. Touching
    // regions are merged so the largest pages can be used.
    let mut pending: Option<Range<u64>> = None;
    for region in boot.memory_regions() {
        let ram = matches!(
            region.kind(),
            RegionKind::Usable
                | RegionKind::BootloaderReclaimable
                | RegionKind::AcpiReclaimable
                | RegionKind::AcpiNvs
        );
        if !ram {
            continue;
        }
        let start = region.base() - region.base() % PAGE_SIZE;
        let end = region.end().next_multiple_of(PAGE_SIZE);
        pending = match pending {
            Some(current) if start <= current.end => Some(current.start..end.max(current.end)),
            Some(current) => {
                flush(current, space);
                Some(start..end)
            }
            None => Some(start..end),
        };
    }
    if let Some(current) = pending {
        flush(current, space);
    }
    mapped
}

/// Maps `[virt, virt + len)` to `[phys, phys + len)` with the largest pages
/// both addresses allow.
fn map_range(
    space: &mut AddressSpace,
    mut virt: u64,
    mut phys: u64,
    len: u64,
    flags: MapFlags,
    gigabyte_pages: bool,
) -> Result<(), MapError> {
    let end = virt + len;
    while virt < end {
        let size = [PageSize::Size1GiB, PageSize::Size2MiB, PageSize::Size4KiB]
            .into_iter()
            .filter(|&s| s != PageSize::Size1GiB || gigabyte_pages)
            .find(|s| {
                let bytes = s.bytes();
                virt.is_multiple_of(bytes) && phys.is_multiple_of(bytes) && end - virt >= bytes
            })
            .unwrap_or(PageSize::Size4KiB);
        space.map(virt, phys, size, flags)?;
        virt += size.bytes();
        phys += size.bytes();
    }
    Ok(())
}

/// Checks that the active page tables enforce the intended layout.
pub fn self_test() {
    for (segment, expected) in kernel_segments() {
        let (_, flags) = translate(segment.start)
            .unwrap_or_else(|| panic!("kernel segment {:#x} not mapped", segment.start));
        assert_eq!(
            flags, expected,
            "kernel segment {:#x} permissions",
            segment.start
        );
    }
    assert!(translate(0).is_none(), "page 0 must never be mapped");

    let stack = allocate_kernel_stack().expect("self-test: allocate a kernel stack");
    let guard = stack.top() - KERNEL_STACK_SIZE - PAGE_SIZE;
    assert!(translate(stack.top() - 8).is_some(), "stack not mapped");
    assert!(translate(guard).is_none(), "stack guard page is mapped");
    klog::info!("paging self-test passed");
}
