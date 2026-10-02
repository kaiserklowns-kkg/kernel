//! Physical memory management.
//!
//! Phase 1 validates and reports the boot memory map; Phase 2 builds the
//! physical frame allocator on it (ADR-0008). Paging and the kernel heap
//! follow (docs/kernel/memory.md).

mod frames;

use oceans_memory_map::{PAGE_SIZE, summarize};
use spin::Once;

use crate::boot::BootInfo;
use crate::klog;

const MIB: u64 = 1024 * 1024;

/// Virtual offset at which the bootloader maps all physical memory.
static DIRECT_MAP_OFFSET: Once<u64> = Once::new();

pub fn init(boot: &BootInfo) {
    discover(boot);
    frames::init(boot);
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
