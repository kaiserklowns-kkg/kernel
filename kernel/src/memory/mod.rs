//! Memory management: boot memory map, physical frames (ADR-0008), the
//! kernel address space (ADR-0009) and the kernel heap (ADR-0010). See
//! docs/kernel/memory.md.

pub mod frames;
pub mod heap;
pub mod layout;
pub mod paging;

use oceans_memory_map::{PAGE_SIZE, summarize};
use spin::Once;

use crate::boot::BootInfo;
use crate::klog;

const MIB: u64 = 1024 * 1024;

/// Virtual offset at which the bootloader maps all physical memory.
static DIRECT_MAP_OFFSET: Once<u64> = Once::new();

/// Discovers memory, starts the frame allocator and switches to the
/// kernel's own page tables.
pub fn init(boot: &BootInfo) {
    discover(boot);
    frames::init(boot);
    paging::init(boot);
    heap::init();
}

/// Hands bootloader-reclaimable memory to the frame allocator. Call only once
/// nothing uses bootloader memory any more: the kernel runs on its own stack
/// and page tables, and boot information has been copied.
pub fn reclaim_bootloader_memory(boot: &BootInfo) {
    frames::reclaim_bootloader_memory(boot);
}

/// Memory self-tests, for smoke-test boots.
pub fn self_test() {
    frames::self_test();
    paging::self_test();
    heap::self_test();
}

/// Physical address of a direct-map pointer.
pub fn virt_to_phys(virt: *const u8) -> u64 {
    let offset = *DIRECT_MAP_OFFSET
        .get()
        .expect("memory::init sets the direct map first");
    (virt as u64)
        .checked_sub(offset)
        .expect("pointer is not in the direct map")
}

/// Virtual address of physical address `phys` in the direct map.
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    let offset = *DIRECT_MAP_OFFSET
        .get()
        .expect("memory::init sets the direct map first");
    (offset + phys) as *mut u8
}

fn discover(boot: &BootInfo) {
    for region in boot.memory_regions() {
        klog::debug!(
            "{:#018x}..{:#018x} {:>8} KiB {}",
            region.base(),
            region.end(),
            region.length() / 1024,
            region.kind().as_str()
        );
    }

    if boot.memory_map_truncated() {
        // Running with a partial view of RAM is safe (we only lose memory),
        // but must be visible.
        klog::warn!(
            "memory map truncated to {} regions",
            boot.memory_regions().len()
        );
    }

    let summary = match summarize(boot.memory_regions().iter().copied()) {
        Ok(summary) => summary,
        Err(err) => panic!("invalid boot memory map: {err:?}"),
    };
    if summary.usable_pages == 0 {
        panic!("boot memory map contains no usable RAM");
    }

    klog::info!(
        "{} MiB usable ({} pages of {} KiB), {} MiB reclaimable, {} MiB reserved, {} regions",
        summary.usable_bytes() / MIB,
        summary.usable_pages,
        PAGE_SIZE / 1024,
        summary.reclaimable_bytes / MIB,
        summary.reserved_bytes / MIB,
        summary.region_count
    );
    match boot.direct_map_offset() {
        Some(offset) => {
            DIRECT_MAP_OFFSET.call_once(|| offset);
            klog::info!("physical memory direct map at {offset:#018x}");
        }
        None => panic!("bootloader did not provide a physical memory direct map"),
    }
}
