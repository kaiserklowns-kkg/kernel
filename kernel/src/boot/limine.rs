//! Limine boot protocol adapter (ADR-0003).

use limine::BaseRevision;
use limine::memory_map::EntryType;
use limine::request::{
    ExecutableAddressRequest, ExecutableCmdlineRequest, HhdmRequest, MemoryMapRequest,
    RequestsEndMarker, RequestsStartMarker,
};
use oceans_memory_map::{Region, RegionKind};

use super::{BOOT_INFO, BootInfo, KernelImage};
use crate::{arch, klog};

// Limine scans the `.limine_requests` section of the kernel image and fills in
// responses before jumping to `kernel_entry`. The linker script keeps these
// sections; `#[used]` keeps the statics. Responses live in
// bootloader-reclaimable memory and are only read here.

#[used]
#[unsafe(link_section = ".limine_requests_start")]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMORY_MAP: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static DIRECT_MAP: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static KERNEL_ADDRESS: ExecutableAddressRequest = ExecutableAddressRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

/// First kernel code to run. Named by `ENTRY` in the linker script.
#[unsafe(no_mangle)]
extern "C" fn kernel_entry() -> ! {
    arch::early_init();

    if !BASE_REVISION.is_supported() {
        // Responses may be laid out differently; nothing below is safe to read.
        panic!("bootloader does not support Limine base revision 3");
    }

    // Built in place: `BootInfo` is several KiB.
    let info = BOOT_INFO.call_once(|| {
        let mut info = BootInfo::new();

        let Some(memory_map) = MEMORY_MAP.get_response() else {
            panic!("bootloader did not provide a memory map");
        };
        for entry in memory_map.entries() {
            info.push_region(Region::new(
                entry.base,
                entry.length,
                region_kind(entry.entry_type),
            ));
        }

        info.direct_map_offset = DIRECT_MAP.get_response().map(|r| r.offset());
        info.kernel_image = KERNEL_ADDRESS.get_response().map(|r| KernelImage {
            physical_base: r.physical_base(),
            virtual_base: r.virtual_base(),
        });

        if let Some(cmdline) = CMDLINE.get_response() {
            match cmdline.cmdline().to_str() {
                Ok(text) => info.set_cmdline(text),
                Err(_) => klog::warn!("ignoring kernel command line: not valid UTF-8"),
            }
        }
        info
    });

    crate::kernel_main(info)
}

fn region_kind(entry_type: EntryType) -> RegionKind {
    match entry_type {
        EntryType::USABLE => RegionKind::Usable,
        EntryType::BOOTLOADER_RECLAIMABLE => RegionKind::BootloaderReclaimable,
        EntryType::ACPI_RECLAIMABLE => RegionKind::AcpiReclaimable,
        EntryType::ACPI_NVS => RegionKind::AcpiNvs,
        EntryType::EXECUTABLE_AND_MODULES => RegionKind::KernelAndModules,
        EntryType::FRAMEBUFFER => RegionKind::Framebuffer,
        EntryType::BAD_MEMORY => RegionKind::BadMemory,
        // Unknown future types are treated as untouchable.
        _ => RegionKind::Reserved,
    }
}
