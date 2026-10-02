//! Bootloader-independent physical memory map model.
//!
//! The kernel's boot layer translates whatever the bootloader reports into
//! [`Region`]s so that the rest of the kernel never depends on a specific boot
//! protocol (ADR-0003). This crate is `no_std`, allocation-free and fully
//! testable on the host.

#![no_std]

#[cfg(test)]
extern crate std;

/// Size of the smallest page the kernel manages.
pub const PAGE_SIZE: u64 = 4096;

/// What a physical memory region may be used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    /// Free RAM the kernel may hand to its frame allocator.
    Usable,
    /// RAM in use by the bootloader; reclaimable once boot data is consumed.
    BootloaderReclaimable,
    /// ACPI tables; reclaimable after ACPI has been parsed.
    AcpiReclaimable,
    /// ACPI non-volatile storage. Never reclaimable.
    AcpiNvs,
    /// The kernel image and boot modules.
    KernelAndModules,
    /// Memory backing a firmware-provided framebuffer.
    Framebuffer,
    /// Defective RAM reported by firmware.
    BadMemory,
    /// Firmware/hardware reserved, or any type the boot layer does not recognise.
    Reserved,
}

impl RegionKind {
    /// Whether the region can become usable RAM after boot data is released.
    pub const fn is_reclaimable(self) -> bool {
        matches!(self, Self::BootloaderReclaimable | Self::AcpiReclaimable)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usable => "usable",
            Self::BootloaderReclaimable => "bootloader-reclaimable",
            Self::AcpiReclaimable => "acpi-reclaimable",
            Self::AcpiNvs => "acpi-nvs",
            Self::KernelAndModules => "kernel+modules",
            Self::Framebuffer => "framebuffer",
            Self::BadMemory => "bad-memory",
            Self::Reserved => "reserved",
        }
    }
}

/// Errors detected while building or validating a memory map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// `base + length` overflows the physical address space.
    Overflow { base: u64, length: u64 },
    /// Regions are not sorted by base address, or they overlap.
    UnorderedOrOverlapping { previous_end: u64, next_base: u64 },
}

/// A contiguous range of physical memory, `[base, base + length)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    base: u64,
    length: u64,
    kind: RegionKind,
}

impl Region {
    pub const fn new(base: u64, length: u64, kind: RegionKind) -> Result<Self, MapError> {
        if base.checked_add(length).is_none() {
            return Err(MapError::Overflow { base, length });
        }
        Ok(Self { base, length, kind })
    }

    pub const fn base(&self) -> u64 {
        self.base
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub const fn kind(&self) -> RegionKind {
        self.kind
    }

    /// Exclusive end address. Cannot overflow: checked in [`Region::new`].
    pub const fn end(&self) -> u64 {
        self.base + self.length
    }

    /// The whole pages fully contained in this region, as `(start, page_count)`.
    ///
    /// Returns `None` if no complete page fits.
    pub const fn whole_pages(&self) -> Option<(u64, u64)> {
        let Some(start) = align_up(self.base, PAGE_SIZE) else {
            return None;
        };
        let end = align_down(self.end(), PAGE_SIZE);
        if end <= start {
            return None;
        }
        Some((start, (end - start) / PAGE_SIZE))
    }
}

/// Aggregate statistics over a memory map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub region_count: usize,
    /// Whole pages of immediately usable RAM.
    pub usable_pages: u64,
    /// Bytes that become usable once boot/ACPI data is released.
    pub reclaimable_bytes: u64,
    /// Bytes that are never available to the allocator.
    pub reserved_bytes: u64,
    /// Exclusive end of the highest usable page, if any.
    pub usable_end: Option<u64>,
}

impl Summary {
    pub const fn usable_bytes(&self) -> u64 {
        self.usable_pages * PAGE_SIZE
    }
}

/// Validates ordering and summarises a memory map in a single pass.
///
/// Regions must be sorted by base address and must not overlap; boot layers
/// are expected to deliver maps in that form, and anything else is reported
/// rather than silently repaired.
pub fn summarize<I>(regions: I) -> Result<Summary, MapError>
where
    I: IntoIterator<Item = Region>,
{
    let mut summary = Summary::default();
    let mut previous_end: Option<u64> = None;

    for region in regions {
        if let Some(previous_end) = previous_end
            && region.base < previous_end
        {
            return Err(MapError::UnorderedOrOverlapping {
                previous_end,
                next_base: region.base,
            });
        }
        previous_end = Some(region.end());
        summary.region_count += 1;

        match region.kind {
            RegionKind::Usable => {
                if let Some((start, pages)) = region.whole_pages() {
                    summary.usable_pages += pages;
                    summary.usable_end = Some(start + pages * PAGE_SIZE);
                }
            }
            kind if kind.is_reclaimable() => summary.reclaimable_bytes += region.length,
            _ => summary.reserved_bytes += region.length,
        }
    }

    Ok(summary)
}

const fn align_up(value: u64, align: u64) -> Option<u64> {
    debug_assert!(align.is_power_of_two());
    match value.checked_add(align - 1) {
        Some(v) => Some(v & !(align - 1)),
        None => None,
    }
}

const fn align_down(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    value & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(base: u64, length: u64, kind: RegionKind) -> Region {
        Region::new(base, length, kind).unwrap()
    }

    #[test]
    fn rejects_overflowing_region() {
        assert_eq!(
            Region::new(u64::MAX - 10, 100, RegionKind::Usable),
            Err(MapError::Overflow {
                base: u64::MAX - 10,
                length: 100
            })
        );
    }

    #[test]
    fn whole_pages_trims_unaligned_edges() {
        let r = region(0x1001, 3 * PAGE_SIZE, RegionKind::Usable);
        assert_eq!(r.whole_pages(), Some((0x2000, 2)));
    }

    #[test]
    fn whole_pages_none_when_too_small() {
        assert_eq!(
            region(0x1800, 0x1000, RegionKind::Usable).whole_pages(),
            None
        );
        assert_eq!(region(0x1000, 0, RegionKind::Usable).whole_pages(), None);
    }

    #[test]
    fn whole_pages_near_top_of_address_space() {
        let r = region(u64::MAX - 0x800, 0x800, RegionKind::Usable);
        assert_eq!(r.whole_pages(), None);
    }

    #[test]
    fn summarizes_typical_map() {
        let map = [
            region(0x0, 0x9f000, RegionKind::Usable),
            region(0x9f000, 0x1000, RegionKind::Reserved),
            region(0x100000, 0x7ee0000, RegionKind::Usable),
            region(0x7fe0000, 0x10000, RegionKind::AcpiReclaimable),
            region(0x7ff0000, 0x8000, RegionKind::BootloaderReclaimable),
            region(0x7ff8000, 0x8000, RegionKind::AcpiNvs),
        ];
        let s = summarize(map).unwrap();
        assert_eq!(s.region_count, 6);
        assert_eq!(s.usable_pages, 0x9f + 0x7ee0);
        assert_eq!(s.usable_bytes(), (0x9f + 0x7ee0) * PAGE_SIZE);
        assert_eq!(s.reclaimable_bytes, 0x18000);
        assert_eq!(s.reserved_bytes, 0x9000);
        assert_eq!(s.usable_end, Some(0x7fe0000));
    }

    #[test]
    fn rejects_overlap() {
        let map = [
            region(0x0, 0x2000, RegionKind::Usable),
            region(0x1000, 0x1000, RegionKind::Reserved),
        ];
        assert_eq!(
            summarize(map),
            Err(MapError::UnorderedOrOverlapping {
                previous_end: 0x2000,
                next_base: 0x1000
            })
        );
    }

    #[test]
    fn empty_map() {
        assert_eq!(summarize([]), Ok(Summary::default()));
    }
}
