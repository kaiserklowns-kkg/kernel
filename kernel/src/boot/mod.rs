//! Boot layer: turns a bootloader's view of the machine into [`BootInfo`].
//!
//! Only this module knows which boot protocol is in use (ADR-0003). Adding a
//! protocol means adding a sibling of `limine` that produces the same
//! `BootInfo`.

mod limine;

use oceans_memory_map::{MapError, Region};

/// Upper bound on memory map entries we keep before an allocator exists.
/// Firmware maps on supported hardware are far below this.
pub const MAX_MEMORY_REGIONS: usize = 256;

/// Protocol-neutral facts the kernel needs from the bootloader.
pub struct BootInfo {
    regions: [Region; MAX_MEMORY_REGIONS],
    region_count: usize,
    regions_truncated: bool,
    direct_map_offset: Option<u64>,
    cmdline: &'static str,
}

impl BootInfo {
    const fn new() -> Self {
        const EMPTY: Region = match Region::new(0, 0, oceans_memory_map::RegionKind::Reserved) {
            Ok(region) => region,
            Err(_) => panic!("empty region is always valid"),
        };
        Self {
            regions: [EMPTY; MAX_MEMORY_REGIONS],
            region_count: 0,
            regions_truncated: false,
            direct_map_offset: None,
            cmdline: "",
        }
    }

    /// Appends a region reported by the bootloader.
    fn push_region(&mut self, region: Result<Region, MapError>) {
        let region = match region {
            Ok(region) => region,
            // The bootloader handed us an impossible entry: we cannot trust
            // the memory map, and running on a wrong one corrupts memory.
            Err(err) => panic!("bootloader reported an invalid memory region: {err:?}"),
        };
        match self.regions.get_mut(self.region_count) {
            Some(slot) => {
                *slot = region;
                self.region_count += 1;
            }
            None => self.regions_truncated = true,
        }
    }

    /// Physical memory regions, sorted by base address as delivered.
    pub fn memory_regions(&self) -> &[Region] {
        &self.regions[..self.region_count]
    }

    pub fn memory_map_truncated(&self) -> bool {
        self.regions_truncated
    }

    /// Virtual offset at which all physical memory is mapped, if provided.
    pub fn direct_map_offset(&self) -> Option<u64> {
        self.direct_map_offset
    }

    pub fn cmdline(&self) -> &'static str {
        self.cmdline
    }

    /// Whether a whitespace-separated `flag` appears on the command line.
    pub fn cmdline_has(&self, flag: &str) -> bool {
        self.cmdline
            .split_ascii_whitespace()
            .any(|word| word == flag)
    }
}
