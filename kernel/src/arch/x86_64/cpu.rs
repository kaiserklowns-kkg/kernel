//! CPU feature detection and protection control bits.

use core::arch::x86_64::{__cpuid, __cpuid_count};

use ::x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use ::x86_64::registers::model_specific::{Efer, EferFlags};

use crate::klog;

/// CPU features the memory system relies on or can use.
#[derive(Clone, Copy, Debug)]
pub struct Features {
    /// Execute-disable page bit. Required (ADR-0005 baseline).
    pub no_execute: bool,
    /// 1 GiB pages.
    pub gigabyte_pages: bool,
    /// Global pages (kept in the TLB across CR3 switches).
    pub global_pages: bool,
    /// Supervisor mode execution prevention.
    pub smep: bool,
    /// Supervisor mode access prevention.
    pub smap: bool,
    /// User mode instruction prevention.
    pub umip: bool,
}

pub fn features() -> Features {
    // Leaves above the reported maxima are not queried.
    let (max_basic, max_extended) = (__cpuid(0).eax, __cpuid(0x8000_0000).eax);
    let leaf1 = __cpuid(1);
    let leaf7 = if max_basic >= 7 {
        Some(__cpuid_count(7, 0))
    } else {
        None
    };
    let ext1 = if max_extended >= 0x8000_0001 {
        Some(__cpuid(0x8000_0001))
    } else {
        None
    };

    Features {
        no_execute: ext1.is_some_and(|l| l.edx & (1 << 20) != 0),
        gigabyte_pages: ext1.is_some_and(|l| l.edx & (1 << 26) != 0),
        global_pages: leaf1.edx & (1 << 13) != 0,
        smep: leaf7.is_some_and(|l| l.ebx & (1 << 7) != 0),
        smap: leaf7.is_some_and(|l| l.ebx & (1 << 20) != 0),
        umip: leaf7.is_some_and(|l| l.ecx & (1 << 2) != 0),
    }
}

/// Turns on every memory protection the CPU supports, and says which. Must
/// run before any page table with NX bits is activated.
pub fn enable_protections(features: &Features) {
    set_protections(features);
    klog::info!(
        "protections: NX WP{}{}{}{}",
        if features.global_pages { " PGE" } else { "" },
        if features.smep { " SMEP" } else { "" },
        if features.smap { " SMAP" } else { "" },
        if features.umip { " UMIP" } else { "" },
    );
}

/// [`enable_protections`] on another CPU (ADR-0088), silently: the boot
/// CPU said which.
pub fn set_protections(features: &Features) {
    if !features.no_execute {
        panic!("CPU lacks the NX bit, required by Oceans (ADR-0005)");
    }
    // SAFETY: each bit is set only when CPUID reports the feature. NXE makes
    // bit 63 of page entries meaningful; WP enforces read-only pages in ring
    // 0; PGE enables global pages; SMEP/SMAP/UMIP restrict what the kernel
    // may do with user memory, of which none is mapped yet.
    unsafe {
        Efer::update(|flags| flags.insert(EferFlags::NO_EXECUTE_ENABLE));
        Cr0::update(|flags| flags.insert(Cr0Flags::WRITE_PROTECT));
        Cr4::update(|flags| {
            if features.global_pages {
                flags.insert(Cr4Flags::PAGE_GLOBAL);
            }
            if features.smep {
                flags.insert(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION);
            }
            if features.smap {
                flags.insert(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION);
            }
            if features.umip {
                flags.insert(Cr4Flags::USER_MODE_INSTRUCTION_PREVENTION);
            }
        });
    }
}
