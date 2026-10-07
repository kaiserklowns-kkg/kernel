//! Limine boot protocol adapter (ADR-0003).

use core::sync::atomic::{AtomicUsize, Ordering};
use limine::BaseRevision;
use limine::framebuffer::MemoryModel;
use limine::memory_map::EntryType;

use limine::mp::Cpu;
use limine::request::{
    ExecutableAddressRequest, ExecutableCmdlineRequest, FramebufferRequest, HhdmRequest,
    MemoryMapRequest, ModuleRequest, MpRequest, RequestsEndMarker, RequestsStartMarker,
    RsdpRequest,
};
use oceans_memory_map::{Region, RegionKind};

use super::{BOOT_INFO, BootInfo, Framebuffer, KernelImage, MAX_SECONDARY_CPUS};
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
#[unsafe(link_section = ".limine_requests")]
static MODULES: ModuleRequest = ModuleRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static RSDP: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

/// The other CPUs (ADR-0088): Limine starts them and parks each until it
/// is given an address to jump to.
#[used]
#[unsafe(link_section = ".limine_requests")]
static MP: MpRequest = MpRequest::new();

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

        // Base revision 3 reports the RSDP physically; older ones used the
        // direct map. Accept both.
        info.rsdp = RSDP.get_response().map(|r| {
            let address = r.address() as u64;
            match info.direct_map_offset {
                Some(offset) if address >= offset => address - offset,
                _ => address,
            }
        });

        // The first RGB framebuffer; its address is in the direct map.
        if let (Some(response), Some(offset)) = (FRAMEBUFFER.get_response(), info.direct_map_offset)
            && let Some(fb) = response
                .framebuffers()
                .find(|fb| fb.memory_model() == MemoryModel::RGB)
        {
            info.framebuffer = Some(Framebuffer {
                physical: (fb.addr() as u64).wrapping_sub(offset),
                width: fb.width(),
                height: fb.height(),
                pitch: fb.pitch(),
                bpp: fb.bpp(),
                red_shift: fb.red_mask_shift(),
                green_shift: fb.green_mask_shift(),
                blue_shift: fb.blue_mask_shift(),
            });
        }

        if let Some(mp) = MP.get_response() {
            info.bsp_lapic_id = Some(mp.bsp_lapic_id());
            for cpu in mp
                .cpus()
                .iter()
                .filter(|cpu| cpu.lapic_id != mp.bsp_lapic_id())
            {
                let index = info.secondary_count;
                if index == MAX_SECONDARY_CPUS {
                    info.cpus_truncated = true;
                    break;
                }
                info.secondary[index] = cpu.lapic_id;
                SECONDARY[index].store(core::ptr::from_ref::<Cpu>(cpu) as usize, Ordering::Relaxed);
                info.secondary_count += 1;
            }
        }

        if let (Some(modules), Some(offset)) = (MODULES.get_response(), info.direct_map_offset) {
            for module in modules.modules() {
                let path = module.path().to_bytes();
                let physical_base = (module.addr() as u64).wrapping_sub(offset);
                if !info.push_module(path, physical_base, module.size()) {
                    klog::warn!(
                        "ignoring boot module {:?}: too many or bad name",
                        module.path()
                    );
                }
            }
        }
        info
    });

    crate::kernel_main(info)
}

/// Limine's record of each secondary CPU, in `BootInfo::secondary_cpus`
/// order. They live in bootloader-reclaimable memory.
static SECONDARY: [AtomicUsize; MAX_SECONDARY_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_SECONDARY_CPUS];
/// What a started CPU calls, with its argument in its record's `extra`.
static ENTRY: AtomicUsize = AtomicUsize::new(0);

/// Starts secondary CPU `index`: it calls `entry(arg)` on Limine's stack
/// and page tables. One at a time (the entry is shared); only before
/// bootloader memory is reclaimed.
pub fn start_secondary(index: usize, entry: extern "C" fn(u64) -> !, arg: u64) {
    let cpu = SECONDARY[index].load(Ordering::Relaxed) as *const Cpu;
    assert!(!cpu.is_null(), "no secondary CPU {index}");
    // SAFETY: Limine's record of a CPU it parked, in bootloader memory that
    // is still ours (the caller's contract).
    let cpu = unsafe { &*cpu };
    ENTRY.store(entry as usize, Ordering::Release);
    cpu.extra.store(arg, Ordering::Release);
    // Ordered after the stores above (sequentially consistent).
    cpu.goto_address.write(secondary_trampoline);
}

/// Where Limine sends a started CPU.
unsafe extern "C" fn secondary_trampoline(cpu: &Cpu) -> ! {
    let arg = cpu.extra.load(Ordering::Acquire);
    // SAFETY: `start_secondary` stored a valid `extern "C" fn(u64) -> !`.
    let entry: extern "C" fn(u64) -> ! =
        unsafe { core::mem::transmute(ENTRY.load(Ordering::Acquire)) };
    entry(arg)
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
