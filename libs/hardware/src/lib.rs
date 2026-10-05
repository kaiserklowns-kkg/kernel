//! What Oceans supports (ADR-0068), host-tested: the Tier 1 CPU baseline
//! (ADR-0005), checked from CPUID, and what each PCI function gets. The
//! same table drives `sysreport` on a machine and the published
//! compatibility matrix (docs/hardware/compatibility.md, whose rows a test
//! here checks).

#![no_std]

/// CPUID leaves as `sysreport` reads them: `(eax, ebx, ecx, edx)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cpuid {
    /// Leaf 1.
    pub leaf1: (u32, u32, u32, u32),
    /// Leaf 0x8000_0001.
    pub ext1: (u32, u32, u32, u32),
}

/// A requirement of the Tier 1 CPU baseline: its name, and whether CPUID
/// shows it.
pub type Check = (&'static str, bool);

impl Cpuid {
    fn ecx(&self, bit: u32) -> bool {
        self.leaf1.2 & (1 << bit) != 0
    }

    fn edx(&self, bit: u32) -> bool {
        self.leaf1.3 & (1 << bit) != 0
    }

    /// The Tier 1 baseline (ADR-0005): x86-64-v2 (CMPXCHG16B, LAHF in long
    /// mode, POPCNT, SSE3, SSSE3, SSE4.1, SSE4.2), NX, and an APIC.
    pub fn baseline(&self) -> [Check; 9] {
        [
            ("SSE3", self.ecx(0)),
            ("SSSE3", self.ecx(9)),
            ("CMPXCHG16B", self.ecx(13)),
            ("SSE4.1", self.ecx(19)),
            ("SSE4.2", self.ecx(20)),
            ("POPCNT", self.ecx(23)),
            ("LAHF-SAHF", self.ext1.2 & 1 != 0),
            ("NX", self.ext1.3 & (1 << 20) != 0),
            ("APIC", self.edx(9)),
        ]
    }

    /// Whether every baseline requirement holds.
    pub fn meets_baseline(&self) -> bool {
        self.baseline().iter().all(|(_, ok)| *ok)
    }

    /// x2APIC (used when present; not required).
    pub fn x2apic(&self) -> bool {
        self.ecx(21)
    }

    /// `(family, model, stepping)`, extended fields applied.
    pub fn signature(&self) -> (u32, u32, u32) {
        let eax = self.leaf1.0;
        let base_family = (eax >> 8) & 0xf;
        let family = if base_family == 0xf {
            base_family + ((eax >> 20) & 0xff)
        } else {
            base_family
        };
        let mut model = (eax >> 4) & 0xf;
        if base_family == 0x6 || base_family == 0xf {
            model |= ((eax >> 16) & 0xf) << 4;
        }
        (family, model, eax & 0xf)
    }
}

/// What Oceans offers a PCI function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// Driven, and validated (Tier 0 in CI; Tier 1 once reports confirm).
    Driver(&'static str),
    /// Works through the firmware, without a driver of its own.
    Firmware(&'static str),
    /// Part of the platform; nothing to drive.
    Platform,
    /// No driver.
    None,
}

/// One row of the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub what: &'static str,
    /// `(vendor, device)` if specific; else any with the class below.
    pub id: Option<(u16, u16)>,
    /// `(class, subclass, prog_if)`; `None` matches any part.
    pub class: (Option<u8>, Option<u8>, Option<u8>),
    pub support: Support,
}

const fn class(c: u8, s: u8, p: Option<u8>) -> (Option<u8>, Option<u8>, Option<u8>) {
    (Some(c), Some(s), p)
}

/// The matrix, most specific rows first.
pub const MATRIX: &[Row] = &[
    Row {
        what: "NVMe SSD (any vendor)",
        id: None,
        class: class(0x01, 0x08, Some(0x02)),
        support: Support::Driver("nvme (ADR-0040)"),
    },
    Row {
        what: "SATA AHCI controller (any vendor)",
        id: None,
        class: class(0x01, 0x06, Some(0x01)),
        support: Support::Driver("ahci (ADR-0069)"),
    },
    Row {
        what: "USB 3 xHCI controller (any vendor)",
        id: None,
        class: class(0x0c, 0x03, Some(0x30)),
        support: Support::Driver("xhci (ADR-0032): keyboards, mice, tablets, hubs, mass storage"),
    },
    Row {
        what: "Intel 82574L Ethernet",
        id: Some((0x8086, 0x10d3)),
        class: (None, None, None),
        support: Support::Driver("e1000e (ADR-0041)"),
    },
    Row {
        what: "virtio-net (modern)",
        id: Some((0x1af4, 0x1041)),
        class: (None, None, None),
        support: Support::Driver("virtio-net (ADR-0023)"),
    },
    Row {
        what: "virtio-blk (modern)",
        id: Some((0x1af4, 0x1042)),
        class: (None, None, None),
        support: Support::Driver("virtio-blk (ADR-0021)"),
    },
    Row {
        what: "Display controller",
        id: None,
        class: (Some(0x03), None, None),
        support: Support::Firmware("UEFI GOP framebuffer, 32 bpp (ADR-0029, ADR-0057)"),
    },
    Row {
        what: "Host bridge, ISA/LPC bridge, PCI bridge",
        id: None,
        class: (Some(0x06), None, None),
        support: Support::Platform,
    },
];

/// The row for a PCI function, if any.
pub fn lookup(
    vendor: u16,
    device: u16,
    class: u8,
    subclass: u8,
    prog_if: u8,
) -> Option<&'static Row> {
    MATRIX.iter().find(|row| {
        row.id.is_none_or(|id| id == (vendor, device))
            && row.class.0.is_none_or(|c| c == class)
            && row.class.1.is_none_or(|s| s == subclass)
            && row.class.2.is_none_or(|p| p == prog_if)
    })
}

/// What a PCI function gets.
pub fn support(vendor: u16, device: u16, class: u8, subclass: u8, prog_if: u8) -> Support {
    lookup(vendor, device, class, subclass, prog_if).map_or(Support::None, |row| row.support)
}

#[cfg(test)]
mod tests;
