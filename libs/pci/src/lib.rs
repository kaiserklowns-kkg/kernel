//! PCI configuration space logic (ADR-0021): function headers, BAR sizing,
//! the capability list and MSI-X.
//!
//! Configuration space is device-controlled input. Nothing here trusts it:
//! the capability walk is bounded and rejects pointers into the header,
//! BAR sizes must be powers of two, and every offset is checked against the
//! 4 KiB space. Access goes through [`ConfigSpace`], so the kernel supplies
//! ECAM memory and the tests a simulated device.

#![no_std]

/// Size of one function's (PCI Express) configuration space.
pub const CONFIG_SPACE_SIZE: u16 = 4096;

/// Access to one function's configuration space. Offsets are below
/// [`CONFIG_SPACE_SIZE`] and naturally aligned.
pub trait ConfigSpace {
    fn read32(&self, offset: u16) -> u32;
    fn write32(&mut self, offset: u16, value: u32);
    /// A 16-bit write that leaves the neighbouring 16 bits untouched (the
    /// status register next to the command register is write-1-to-clear).
    fn write16(&mut self, offset: u16, value: u16);

    fn read16(&self, offset: u16) -> u16 {
        (self.read32(offset & !3) >> (u32::from(offset & 2) * 8)) as u16
    }

    fn read8(&self, offset: u16) -> u8 {
        (self.read32(offset & !3) >> (u32::from(offset & 3) * 8)) as u8
    }
}

/// Standard header registers.
pub mod reg {
    pub const VENDOR_ID: u16 = 0x00;
    pub const DEVICE_ID: u16 = 0x02;
    pub const COMMAND: u16 = 0x04;
    pub const STATUS: u16 = 0x06;
    pub const REVISION: u16 = 0x08;
    pub const PROG_IF: u16 = 0x09;
    pub const SUBCLASS: u16 = 0x0a;
    pub const CLASS: u16 = 0x0b;
    pub const HEADER_TYPE: u16 = 0x0e;
    pub const BAR0: u16 = 0x10;
    /// Type 1 (bridge) headers.
    pub const SECONDARY_BUS: u16 = 0x19;
    pub const CAPABILITIES: u16 = 0x34;
}

/// Command register bits.
pub mod command {
    pub const IO_SPACE: u16 = 1 << 0;
    pub const MEMORY_SPACE: u16 = 1 << 1;
    pub const BUS_MASTER: u16 = 1 << 2;
    pub const INTX_DISABLE: u16 = 1 << 10;
}

/// Status register: the function has a capability list.
const STATUS_CAPABILITIES: u16 = 1 << 4;

/// Capability IDs.
pub mod cap {
    pub const MSI: u8 = 0x05;
    pub const VENDOR: u8 = 0x09;
    pub const PCI_EXPRESS: u8 = 0x10;
    pub const MSIX: u8 = 0x11;
}

/// A function's identity, from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    /// 0 = endpoint, 1 = PCI-to-PCI bridge, 2 = CardBus bridge.
    pub kind: u8,
    pub multifunction: bool,
}

impl Header {
    pub const fn is_bridge(&self) -> bool {
        self.kind == 1
    }
}

/// The function's header, or `None` if nothing responds at this address.
pub fn header(config: &impl ConfigSpace) -> Option<Header> {
    let vendor = config.read16(reg::VENDOR_ID);
    if vendor == 0xffff || vendor == 0 {
        return None;
    }
    let header_type = config.read8(reg::HEADER_TYPE);
    Some(Header {
        vendor,
        device: config.read16(reg::DEVICE_ID),
        class: config.read8(reg::CLASS),
        subclass: config.read8(reg::SUBCLASS),
        prog_if: config.read8(reg::PROG_IF),
        revision: config.read8(reg::REVISION),
        kind: header_type & 0x7f,
        multifunction: header_type & 0x80 != 0,
    })
}

/// The bus behind a PCI-to-PCI bridge.
pub fn secondary_bus(config: &impl ConfigSpace) -> u8 {
    config.read8(reg::SECONDARY_BUS)
}

/// A base address register, as sized at enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bar {
    /// Unimplemented, or the upper half of a 64-bit BAR.
    #[default]
    None,
    Memory {
        base: u64,
        size: u64,
        prefetchable: bool,
    },
    Io {
        base: u32,
        size: u32,
    },
}

const BAR_IO: u32 = 1 << 0;
const BAR_TYPE_64: u32 = 0b10 << 1;
const BAR_TYPE_MASK: u32 = 0b11 << 1;
const BAR_PREFETCHABLE: u32 = 1 << 3;

/// Sizes the six BARs of an endpoint (type 0 header) by writing all ones
/// and reading back, with decoding disabled meanwhile so the device never
/// answers at the probe addresses; every register is restored. Bridges
/// have only two BARs; pass `count` 2 for them.
pub fn size_bars(config: &mut impl ConfigSpace, count: usize) -> [Bar; 6] {
    let mut bars = [Bar::None; 6];
    let count = count.min(6);
    let command = config.read16(reg::COMMAND);
    config.write16(
        reg::COMMAND,
        command & !(command::IO_SPACE | command::MEMORY_SPACE),
    );
    let mut index = 0;
    while index < count {
        let offset = reg::BAR0 + 4 * index as u16;
        let original = config.read32(offset);
        config.write32(offset, u32::MAX);
        let probe = config.read32(offset);
        config.write32(offset, original);

        if original & BAR_IO != 0 {
            let mask = probe & !0b11;
            let size = (!mask).wrapping_add(1) & 0xffff;
            if mask != 0 && size.is_power_of_two() {
                bars[index] = Bar::Io {
                    base: original & !0b11,
                    size,
                };
            }
            index += 1;
            continue;
        }

        let wide = original & BAR_TYPE_MASK == BAR_TYPE_64 && index + 1 < count;
        let (base, mask) = if wide {
            let high_offset = offset + 4;
            let high = config.read32(high_offset);
            config.write32(high_offset, u32::MAX);
            let high_probe = config.read32(high_offset);
            config.write32(high_offset, high);
            (
                (u64::from(high) << 32) | u64::from(original & !0xf),
                (u64::from(high_probe) << 32) | u64::from(probe & !0xf),
            )
        } else {
            (
                u64::from(original & !0xf),
                u64::from(probe & !0xf) | 0xffff_ffff_0000_0000,
            )
        };
        let size = (!mask).wrapping_add(1);
        if probe & !0xf != 0 && size.is_power_of_two() {
            bars[index] = Bar::Memory {
                base,
                size,
                prefetchable: original & BAR_PREFETCHABLE != 0,
            };
        }
        index += if wide { 2 } else { 1 };
    }
    config.write16(reg::COMMAND, command);
    bars
}

/// Most capabilities a well-formed list can hold (each takes at least 4
/// bytes of the 192 after the header); bounds walks of malicious loops.
const MAX_CAPABILITIES: usize = 48;

/// The capability list: `(id, offset)` pairs in list order. The walk stops
/// at the first pointer into the header, past the space, or misaligned,
/// and after [`MAX_CAPABILITIES`] entries, so a looping list terminates.
pub fn capabilities(config: &impl ConfigSpace) -> impl Iterator<Item = (u8, u16)> + '_ {
    let mut next = if config.read16(reg::STATUS) & STATUS_CAPABILITIES != 0 {
        u16::from(config.read8(reg::CAPABILITIES) & !0b11)
    } else {
        0
    };
    let mut remaining = MAX_CAPABILITIES;
    core::iter::from_fn(move || {
        if next < 0x40 || remaining == 0 {
            return None;
        }
        remaining -= 1;
        let at = next;
        let id = config.read8(at);
        next = u16::from(config.read8(at + 1) & !0b11);
        Some((id, at))
    })
}

/// An MSI-X capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msix {
    /// Offset of the capability in configuration space.
    pub offset: u16,
    /// Number of table entries (vectors).
    pub entries: u16,
    pub table_bar: u8,
    pub table_offset: u32,
    pub pba_bar: u8,
    pub pba_offset: u32,
}

impl Msix {
    /// Message control register (enable, function mask, table size).
    pub const fn control(&self) -> u16 {
        self.offset + 2
    }

    pub const ENABLE: u16 = 1 << 15;
    pub const FUNCTION_MASK: u16 = 1 << 14;
    /// Bytes per table entry.
    pub const ENTRY_SIZE: u64 = 16;

    /// Byte range of the vector table within its BAR.
    pub fn table(&self) -> (u8, u64, u64) {
        (
            self.table_bar,
            u64::from(self.table_offset),
            u64::from(self.entries) * Self::ENTRY_SIZE,
        )
    }

    /// Byte range of the pending-bit array within its BAR.
    pub fn pending_bits(&self) -> (u8, u64, u64) {
        (
            self.pba_bar,
            u64::from(self.pba_offset),
            u64::from(self.entries).div_ceil(64) * 8,
        )
    }
}

/// The function's MSI-X capability, if it has a valid one.
pub fn msix(config: &impl ConfigSpace) -> Option<Msix> {
    let (_, offset) = capabilities(config).find(|&(id, _)| id == cap::MSIX)?;
    if offset + 12 > CONFIG_SPACE_SIZE {
        return None;
    }
    let control = config.read16(offset + 2);
    let table = config.read32(offset + 4);
    let pba = config.read32(offset + 8);
    let msix = Msix {
        offset,
        entries: (control & 0x7ff) + 1,
        table_bar: (table & 0b111) as u8,
        table_offset: table & !0b111,
        pba_bar: (pba & 0b111) as u8,
        pba_offset: pba & !0b111,
    };
    (msix.table_bar < 6 && msix.pba_bar < 6).then_some(msix)
}

/// Reads `width` (1, 2 or 4) bytes at `offset`, if the access is aligned
/// and inside configuration space.
pub fn read(config: &impl ConfigSpace, offset: u16, width: u8) -> Option<u32> {
    let fits = offset
        .checked_add(u16::from(width))
        .is_some_and(|end| end <= CONFIG_SPACE_SIZE);
    if !fits || !matches!(width, 1 | 2 | 4) || !offset.is_multiple_of(u16::from(width)) {
        return None;
    }
    Some(match width {
        1 => u32::from(config.read8(offset)),
        2 => u32::from(config.read16(offset)),
        _ => config.read32(offset),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A simulated function: registers plus BAR decoders that ignore the
    /// bits below their size, like hardware.
    struct Device {
        regs: [u32; 1024],
        /// Writable address bits of each BAR register (0 = unimplemented).
        bar_masks: [u32; 6],
        /// Writes to BARs seen while memory decoding was on.
        decoded_probes: usize,
    }

    impl Device {
        fn new(vendor: u16, device: u16) -> Self {
            let mut regs = [0u32; 1024];
            regs[0] = u32::from(vendor) | (u32::from(device) << 16);
            Self {
                regs,
                bar_masks: [0; 6],
                decoded_probes: 0,
            }
        }

        fn set8(&mut self, offset: u16, value: u8) {
            let word = &mut self.regs[usize::from(offset / 4)];
            let shift = u32::from(offset % 4) * 8;
            *word = (*word & !(0xff << shift)) | (u32::from(value) << shift);
        }

        fn set16(&mut self, offset: u16, value: u16) {
            self.set8(offset, value as u8);
            self.set8(offset + 1, (value >> 8) as u8);
        }

        fn set32(&mut self, offset: u16, value: u32) {
            self.regs[usize::from(offset / 4)] = value;
        }

        /// A memory BAR at `index` of `size` bytes (64-bit: also the next
        /// register), currently at `base`.
        fn memory_bar(&mut self, index: usize, base: u64, size: u64, wide: bool) {
            let mask = !(size - 1);
            let flags = if wide { BAR_TYPE_64 } else { 0 } | BAR_PREFETCHABLE;
            self.bar_masks[index] = (mask as u32) & !0xf;
            self.set32(reg::BAR0 + 4 * index as u16, (base as u32 & !0xf) | flags);
            if wide {
                self.bar_masks[index + 1] = (mask >> 32) as u32;
                self.set32(reg::BAR0 + 4 * (index as u16 + 1), (base >> 32) as u32);
            }
        }

        fn add_capability(&mut self, at: u16, id: u8, next: u16) {
            self.set16(reg::STATUS, STATUS_CAPABILITIES);
            if self.regs[usize::from(reg::CAPABILITIES / 4)] & 0xff == 0 {
                self.set8(reg::CAPABILITIES, at as u8);
            }
            self.set8(at, id);
            self.set8(at + 1, next as u8);
        }
    }

    impl ConfigSpace for Device {
        fn read32(&self, offset: u16) -> u32 {
            self.regs[usize::from(offset / 4)]
        }

        fn write32(&mut self, offset: u16, value: u32) {
            let index = usize::from(offset / 4);
            let bar = usize::from(offset.wrapping_sub(reg::BAR0) / 4);
            if (reg::BAR0..reg::BAR0 + 24).contains(&offset) {
                if self.read16(reg::COMMAND) & command::MEMORY_SPACE != 0 {
                    self.decoded_probes += 1;
                }
                let mask = self.bar_masks[bar];
                let low_flags = self.regs[index] & !mask & 0xf;
                self.regs[index] = (value & mask) | if mask & 0xf == 0 { low_flags } else { 0 };
            } else {
                self.regs[index] = value;
            }
        }

        fn write16(&mut self, offset: u16, value: u16) {
            self.set16(offset, value);
        }
    }

    #[test]
    fn reads_header_and_detects_absence() {
        let mut device = Device::new(0x1af4, 0x1042);
        device.set8(reg::CLASS, 0x01);
        device.set8(reg::SUBCLASS, 0x00);
        device.set8(reg::REVISION, 1);
        device.set8(reg::HEADER_TYPE, 0x80);
        let header = header(&device).unwrap();
        assert_eq!((header.vendor, header.device), (0x1af4, 0x1042));
        assert_eq!((header.class, header.subclass, header.revision), (1, 0, 1));
        assert!(header.multifunction && !header.is_bridge());

        assert_eq!(super::header(&Device::new(0xffff, 0xffff)), None);
        assert_eq!(super::header(&Device::new(0, 0)), None);
    }

    #[test]
    fn sizes_bars_without_decoding_and_restores_them() {
        let mut device = Device::new(0x1af4, 0x1042);
        device.set16(reg::COMMAND, command::MEMORY_SPACE | command::BUS_MASTER);
        device.memory_bar(1, 0xc000_1000, 0x1000, false);
        device.memory_bar(4, 0x8_0000_0000, 0x4000, true);
        device.set32(reg::BAR0, 0xc001); // I/O BAR, 64 ports
        device.bar_masks[0] = 0xffff_ffc0 | 0b01;
        let before = device.regs;

        let bars = size_bars(&mut device, 6);
        assert_eq!(
            bars[0],
            Bar::Io {
                base: 0xc000,
                size: 64
            }
        );
        assert_eq!(
            bars[1],
            Bar::Memory {
                base: 0xc000_1000,
                size: 0x1000,
                prefetchable: true
            }
        );
        assert_eq!((bars[2], bars[3]), (Bar::None, Bar::None));
        assert_eq!(
            bars[4],
            Bar::Memory {
                base: 0x8_0000_0000,
                size: 0x4000,
                prefetchable: true
            }
        );
        assert_eq!(bars[5], Bar::None, "upper half of the 64-bit BAR");
        assert_eq!(
            device.decoded_probes, 0,
            "probes only while decoding is off"
        );
        assert_eq!(device.regs, before, "every register restored");
    }

    #[test]
    fn rejects_bars_that_are_not_powers_of_two() {
        let mut device = Device::new(1, 1);
        device.set32(reg::BAR0, 0xc000_0000);
        device.bar_masks[0] = 0xfff0_1000; // holes in the mask
        assert_eq!(size_bars(&mut device, 6)[0], Bar::None);
    }

    #[test]
    fn walks_capabilities_and_finds_msix() {
        let mut device = Device::new(0x1af4, 0x1042);
        device.add_capability(0x40, cap::VENDOR, 0x50);
        device.add_capability(0x50, cap::MSIX, 0x60);
        device.set16(0x52, 2); // 3 entries
        device.set32(0x54, 0x2000 | 1); // table in BAR1 at 0x2000
        device.set32(0x58, 0x3000 | 1); // PBA in BAR1 at 0x3000
        device.add_capability(0x60, cap::PCI_EXPRESS, 0);

        let caps: [(u8, u16); 3] = {
            let mut found = [(0, 0); 3];
            for (slot, capability) in found.iter_mut().zip(capabilities(&device)) {
                *slot = capability;
            }
            found
        };
        assert_eq!(
            caps,
            [
                (cap::VENDOR, 0x40),
                (cap::MSIX, 0x50),
                (cap::PCI_EXPRESS, 0x60)
            ]
        );
        let msix = msix(&device).unwrap();
        assert_eq!(msix.entries, 3);
        assert_eq!(msix.table(), (1, 0x2000, 48));
        assert_eq!(msix.pending_bits(), (1, 0x3000, 8));
        assert_eq!(msix.control(), 0x52);
    }

    #[test]
    fn capability_walk_survives_malicious_lists() {
        // A loop: 0x40 -> 0x48 -> 0x40 ...
        let mut device = Device::new(1, 1);
        device.add_capability(0x40, cap::VENDOR, 0x48);
        device.add_capability(0x48, cap::VENDOR, 0x40);
        assert_eq!(capabilities(&device).count(), MAX_CAPABILITIES);
        assert_eq!(msix(&device), None);

        // A pointer into the header ends the walk.
        let mut device = Device::new(1, 1);
        device.add_capability(0x40, cap::VENDOR, 0x10);
        assert_eq!(capabilities(&device).count(), 1);

        // No capability list: the pointer register is ignored.
        let mut device = Device::new(1, 1);
        device.set8(reg::CAPABILITIES, 0x40);
        device.set8(0x40, cap::MSIX);
        assert_eq!(capabilities(&device).count(), 0);

        // An MSI-X capability naming a BAR that does not exist.
        let mut device = Device::new(1, 1);
        device.add_capability(0x40, cap::MSIX, 0);
        device.set32(0x44, 7);
        assert_eq!(msix(&device), None);
    }

    #[test]
    fn checked_reads() {
        let device = Device::new(0x8086, 0x29c0);
        assert_eq!(read(&device, 0, 2), Some(0x8086));
        assert_eq!(read(&device, 2, 2), Some(0x29c0));
        assert_eq!(read(&device, 0, 4), Some(0x29c0_8086));
        assert_eq!(read(&device, 1, 1), Some(0x80));
        assert_eq!(read(&device, 1, 2), None, "misaligned");
        assert_eq!(read(&device, 0, 3), None, "bad width");
        assert_eq!(read(&device, 4094, 4), None, "past the end");
        assert_eq!(read(&device, 4092, 4), Some(0));
    }
}
