//! Boot layer: turns a bootloader's view of the machine into [`BootInfo`].
//!
//! Only this module knows which boot protocol is in use (ADR-0003). Adding a
//! protocol means adding a sibling of `limine` that produces the same
//! `BootInfo`.
//!
//! `BootInfo` is copied into kernel-owned memory, so nothing refers to
//! bootloader memory once it is reclaimed (ADR-0009).

mod limine;

use oceans_memory_map::{MapError, Region, RegionKind};
use spin::Once;

/// Upper bound on memory map entries we keep before an allocator exists.
/// Firmware maps on supported hardware are far below this.
pub const MAX_MEMORY_REGIONS: usize = 256;

/// Longest kernel command line kept; longer ones are truncated with a warning.
pub const MAX_CMDLINE_LEN: usize = 512;

/// Boot modules (program images loaded by the bootloader) kept.
pub const MAX_MODULES: usize = 16;
const MAX_MODULE_NAME: usize = 64;

/// A file the bootloader loaded into memory for the kernel.
#[derive(Clone, Copy)]
pub struct BootModule {
    name: [u8; MAX_MODULE_NAME],
    name_len: usize,
    /// Physical address and size of the contents.
    pub physical_base: u64,
    pub size: u64,
}

impl BootModule {
    const EMPTY: Self = Self {
        name: [0; MAX_MODULE_NAME],
        name_len: 0,
        physical_base: 0,
        size: 0,
    };

    /// File name (last path component).
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }
}

static BOOT_INFO: Once<BootInfo> = Once::new();

/// Boot information, available once the boot layer has run.
pub fn info() -> &'static BootInfo {
    BOOT_INFO.get().expect("boot layer runs before the kernel")
}

/// Where the bootloader loaded the kernel image.
#[derive(Clone, Copy, Debug)]
pub struct KernelImage {
    pub physical_base: u64,
    pub virtual_base: u64,
}

/// Protocol-neutral facts the kernel needs from the bootloader.
pub struct BootInfo {
    regions: [Region; MAX_MEMORY_REGIONS],
    region_count: usize,
    regions_truncated: bool,
    direct_map_offset: Option<u64>,
    kernel_image: Option<KernelImage>,
    cmdline: [u8; MAX_CMDLINE_LEN],
    cmdline_len: usize,
    cmdline_truncated: bool,
    modules: [BootModule; MAX_MODULES],
    module_count: usize,
}

impl BootInfo {
    const fn new() -> Self {
        const EMPTY: Region = match Region::new(0, 0, RegionKind::Reserved) {
            Ok(region) => region,
            Err(_) => panic!("empty region is always valid"),
        };
        Self {
            regions: [EMPTY; MAX_MEMORY_REGIONS],
            region_count: 0,
            regions_truncated: false,
            direct_map_offset: None,
            kernel_image: None,
            cmdline: [0; MAX_CMDLINE_LEN],
            cmdline_len: 0,
            cmdline_truncated: false,
            modules: [BootModule::EMPTY; MAX_MODULES],
            module_count: 0,
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

    /// Records a boot module. Names longer than the limit or extra modules
    /// are reported and skipped, never silently truncated.
    fn push_module(&mut self, path: &[u8], physical_base: u64, size: u64) -> bool {
        let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
        if name.len() > MAX_MODULE_NAME || core::str::from_utf8(name).is_err() {
            return false;
        }
        let Some(slot) = self.modules.get_mut(self.module_count) else {
            return false;
        };
        slot.name[..name.len()].copy_from_slice(name);
        slot.name_len = name.len();
        slot.physical_base = physical_base;
        slot.size = size;
        self.module_count += 1;
        true
    }

    /// Copies `text`, truncating at a character boundary if it is too long.
    fn set_cmdline(&mut self, text: &str) {
        let mut len = text.len().min(MAX_CMDLINE_LEN);
        while !text.is_char_boundary(len) {
            len -= 1;
        }
        self.cmdline[..len].copy_from_slice(&text.as_bytes()[..len]);
        self.cmdline_len = len;
        self.cmdline_truncated = len < text.len();
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

    pub fn kernel_image(&self) -> Option<KernelImage> {
        self.kernel_image
    }

    pub fn modules(&self) -> &[BootModule] {
        &self.modules[..self.module_count]
    }

    pub fn module(&self, name: &str) -> Option<&BootModule> {
        self.modules().iter().find(|m| m.name() == name)
    }

    pub fn cmdline(&self) -> &str {
        // Copied from a `&str` and cut at a character boundary, so valid.
        core::str::from_utf8(&self.cmdline[..self.cmdline_len]).unwrap_or("")
    }

    pub fn cmdline_truncated(&self) -> bool {
        self.cmdline_truncated
    }

    /// Whether a whitespace-separated `flag` appears on the command line.
    pub fn cmdline_has(&self, flag: &str) -> bool {
        self.cmdline()
            .split_ascii_whitespace()
            .any(|word| word == flag)
    }
}
