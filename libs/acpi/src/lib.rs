//! Minimal ACPI table parsing (ADR-0017, ADR-0021, ADR-0085): RSDP, RSDT/XSDT,
//! the MADT, the MCFG, the FADT and the sleep types of the DSDT.
//!
//! Firmware tables are untrusted input. Every structure is length- and
//! checksum-validated before use, nothing is read out of bounds, and the
//! parser never allocates. Physical memory access is the caller's job: it
//! passes the table bytes it has mapped.

#![no_std]

const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
const RSDP_V1_LEN: usize = 20;
const RSDP_V2_LEN: usize = 36;
pub const SDT_HEADER_LEN: usize = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpiError {
    /// Fewer bytes than the structure needs.
    Truncated,
    /// Wrong signature.
    BadSignature,
    /// Bytes do not sum to zero.
    BadChecksum,
    /// A length field is inconsistent.
    BadLength,
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// Where the system description tables are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootTable {
    /// ACPI 1.0: 32-bit table pointers.
    Rsdt(u64),
    /// ACPI 2.0+: 64-bit table pointers.
    Xsdt(u64),
}

/// Parses the Root System Description Pointer. `bytes` should hold at least
/// 36 bytes when available (20 suffice for ACPI 1.0).
pub fn parse_rsdp(bytes: &[u8]) -> Result<RootTable, AcpiError> {
    let v1 = bytes.get(..RSDP_V1_LEN).ok_or(AcpiError::Truncated)?;
    if &v1[..8] != RSDP_SIGNATURE {
        return Err(AcpiError::BadSignature);
    }
    if !checksum_ok(v1) {
        return Err(AcpiError::BadChecksum);
    }
    let revision = v1[15];
    if revision >= 2 {
        let v2 = bytes.get(..RSDP_V2_LEN).ok_or(AcpiError::Truncated)?;
        let length = u32_at(v2, 20) as usize;
        if length < RSDP_V2_LEN {
            return Err(AcpiError::BadLength);
        }
        if !checksum_ok(v2) {
            return Err(AcpiError::BadChecksum);
        }
        let xsdt = u64_at(v2, 24);
        if xsdt != 0 {
            return Ok(RootTable::Xsdt(xsdt));
        }
    }
    Ok(RootTable::Rsdt(u64::from(u32_at(v1, 16))))
}

/// Length of the table whose header starts `bytes` (to know how much to
/// map before validating).
pub fn table_length(header: &[u8]) -> Result<usize, AcpiError> {
    let header = header.get(..SDT_HEADER_LEN).ok_or(AcpiError::Truncated)?;
    let length = u32_at(header, 4) as usize;
    if length < SDT_HEADER_LEN {
        return Err(AcpiError::BadLength);
    }
    Ok(length)
}

/// A validated system description table.
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    bytes: &'a [u8],
}

impl<'a> Table<'a> {
    /// Validates `bytes` (exactly one table, including its header).
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AcpiError> {
        let length = table_length(bytes)?;
        let bytes = bytes.get(..length).ok_or(AcpiError::Truncated)?;
        if !checksum_ok(bytes) {
            return Err(AcpiError::BadChecksum);
        }
        Ok(Self { bytes })
    }

    pub fn signature(&self) -> [u8; 4] {
        self.bytes[..4].try_into().expect("4 bytes")
    }

    /// The bytes after the header.
    pub fn body(&self) -> &'a [u8] {
        &self.bytes[SDT_HEADER_LEN..]
    }
}

/// Physical addresses of the tables listed by an RSDT or XSDT.
pub fn root_entries<'a>(root: Table<'a>, kind: RootTable) -> impl Iterator<Item = u64> + 'a {
    let width = match kind {
        RootTable::Rsdt(_) => 4,
        RootTable::Xsdt(_) => 8,
    };
    root.body().chunks_exact(width).map(move |entry| {
        if width == 4 {
            u64::from(u32_at(entry, 0))
        } else {
            u64_at(entry, 0)
        }
    })
}

/// An entry of the Multiple APIC Description Table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MadtEntry {
    LocalApic {
        processor: u8,
        apic_id: u8,
        enabled: bool,
    },
    IoApic {
        id: u8,
        address: u32,
        gsi_base: u32,
    },
    /// An ISA IRQ delivered on a different global system interrupt or with
    /// non-default polarity/trigger.
    InterruptOverride {
        source: u8,
        gsi: u32,
        flags: u16,
    },
    Other {
        kind: u8,
    },
}

/// The MADT body: local APIC address, flags, then variable-length entries.
#[derive(Clone, Copy, Debug)]
pub struct Madt<'a> {
    pub local_apic_address: u32,
    entries: &'a [u8],
}

impl<'a> Madt<'a> {
    pub fn parse(table: Table<'a>) -> Result<Self, AcpiError> {
        if &table.signature() != b"APIC" {
            return Err(AcpiError::BadSignature);
        }
        let body = table.body();
        let fixed = body.get(..8).ok_or(AcpiError::Truncated)?;
        Ok(Self {
            local_apic_address: u32_at(fixed, 0),
            entries: &body[8..],
        })
    }

    /// Entries in table order. Stops at the first malformed entry.
    pub fn entries(&self) -> impl Iterator<Item = MadtEntry> + 'a {
        let mut rest = self.entries;
        core::iter::from_fn(move || {
            let (&kind, after) = rest.split_first()?;
            let length = usize::from(*after.first()?);
            if length < 2 || length > rest.len() {
                rest = &[];
                return None;
            }
            let entry = &rest[..length];
            rest = &rest[length..];
            Some(match (kind, length) {
                (0, 8..) => MadtEntry::LocalApic {
                    processor: entry[2],
                    apic_id: entry[3],
                    enabled: u32_at(entry, 4) & 1 != 0,
                },
                (1, 12..) => MadtEntry::IoApic {
                    id: entry[2],
                    address: u32_at(entry, 4),
                    gsi_base: u32_at(entry, 8),
                },
                (2, 10..) => MadtEntry::InterruptOverride {
                    source: entry[3],
                    gsi: u32_at(entry, 4),
                    flags: u16_at(entry, 8),
                },
                _ => MadtEntry::Other { kind },
            })
        })
    }

    /// The global system interrupt and MPS flags for ISA `irq`: the
    /// override if there is one, else identity mapping with ISA defaults
    /// (flags 0 = bus default: active high, edge triggered).
    pub fn isa_irq(&self, irq: u8) -> (u32, u16) {
        self.entries()
            .find_map(|entry| match entry {
                MadtEntry::InterruptOverride { source, gsi, flags } if source == irq => {
                    Some((gsi, flags))
                }
                _ => None,
            })
            .unwrap_or((u32::from(irq), 0))
    }
}

/// A PCI Express enhanced configuration (ECAM) region: configuration space
/// of buses `start_bus..=end_bus` in `segment`, 1 MiB per bus from `base`
/// (ADR-0021).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcamRegion {
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl EcamRegion {
    /// Physical address of the 4 KiB configuration space of
    /// `bus:device.function`, if this region covers the bus.
    pub fn function_address(&self, bus: u8, device: u8, function: u8) -> Option<u64> {
        if bus < self.start_bus || bus > self.end_bus || device > 31 || function > 7 {
            return None;
        }
        let offset = (u64::from(bus - self.start_bus) << 20)
            | (u64::from(device) << 15)
            | (u64::from(function) << 12);
        self.base.checked_add(offset)
    }
}

/// The ECAM regions listed by an MCFG table. Entries with an empty or
/// inverted bus range, or a zero base, are skipped.
pub fn mcfg_regions<'a>(
    table: Table<'a>,
) -> Result<impl Iterator<Item = EcamRegion> + 'a, AcpiError> {
    if &table.signature() != b"MCFG" {
        return Err(AcpiError::BadSignature);
    }
    // 8 reserved bytes, then 16-byte allocation entries.
    let entries = table.body().get(8..).ok_or(AcpiError::Truncated)?;
    Ok(entries.as_chunks::<16>().0.iter().filter_map(|entry| {
        let region = EcamRegion {
            base: u64_at(entry, 0),
            segment: u16_at(entry, 8),
            start_bus: entry[10],
            end_bus: entry[11],
        };
        (region.base != 0 && region.start_bus <= region.end_bus).then_some(region)
    }))
}

/// Address spaces of a [`GenericAddress`] Oceans can use.
pub mod space {
    pub const SYSTEM_MEMORY: u8 = 0;
    pub const SYSTEM_IO: u8 = 1;
    pub const PCI_CONFIG: u8 = 2;
}

/// An ACPI Generic Address Structure: a register in some address space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenericAddress {
    /// See [`space`].
    pub space: u8,
    pub bit_width: u8,
    pub bit_offset: u8,
    /// 0 undefined, 1 byte, 2 word, 3 dword, 4 qword.
    pub access_size: u8,
    pub address: u64,
}

impl GenericAddress {
    /// The 12-byte structure at `at`; `None` if out of bounds or zero (absent).
    fn at(bytes: &[u8], at: usize) -> Option<Self> {
        let raw = bytes.get(at..at + 12)?;
        let address = u64_at(raw, 4);
        (address != 0).then_some(Self {
            space: raw[0],
            bit_width: raw[1],
            bit_offset: raw[2],
            access_size: raw[3],
            address,
        })
    }

    /// A legacy 32-bit I/O port block of `len` bytes (`None` if absent).
    fn io(port: u32, len: u8) -> Option<Self> {
        (port != 0 && len != 0).then_some(Self {
            space: space::SYSTEM_IO,
            bit_width: len.saturating_mul(8),
            bit_offset: 0,
            access_size: 0,
            address: u64::from(port),
        })
    }
}

// FADT field offsets, from the start of the table (header included).
const FADT_DSDT: usize = 40;
const FADT_SMI_CMD: usize = 48;
const FADT_ACPI_ENABLE: usize = 52;
const FADT_PM1A_CNT: usize = 64;
const FADT_PM1B_CNT: usize = 68;
const FADT_PM1_CNT_LEN: usize = 89;
const FADT_FLAGS: usize = 112;
const FADT_RESET_REG: usize = 116;
const FADT_RESET_VALUE: usize = 128;
const FADT_X_DSDT: usize = 140;
const FADT_X_PM1A_CNT: usize = 172;
const FADT_X_PM1B_CNT: usize = 184;
const FADT_SLEEP_CONTROL: usize = 244;
/// The shortest FADT (ACPI 1.0) ends after the flags.
const FADT_V1_LEN: usize = 116;
const FLAG_RESET_REG_SUP: u32 = 1 << 10;
const FLAG_HW_REDUCED_ACPI: u32 = 1 << 20;

/// What the Fixed ACPI Description Table says about power (ADR-0085).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fadt {
    /// Physical address of the DSDT (0: none).
    pub dsdt: u64,
    /// Where `acpi_enable` is written to switch the firmware into ACPI
    /// mode (0: always in ACPI mode).
    pub smi_command: u32,
    pub acpi_enable: u8,
    /// The PM1 control registers (SLP_TYP, SLP_EN, SCI_EN).
    pub pm1a_control: Option<GenericAddress>,
    pub pm1b_control: Option<GenericAddress>,
    /// The reset register and the value that resets the machine, when the
    /// firmware says it works.
    pub reset: Option<(GenericAddress, u8)>,
    /// Hardware-reduced ACPI: no PM1 blocks; sleep goes through
    /// `sleep_control`.
    pub hardware_reduced: bool,
    pub sleep_control: Option<GenericAddress>,
}

impl Fadt {
    pub fn parse(table: Table<'_>) -> Result<Self, AcpiError> {
        if &table.signature() != b"FACP" {
            return Err(AcpiError::BadSignature);
        }
        let bytes = table.bytes;
        if bytes.len() < FADT_V1_LEN {
            return Err(AcpiError::Truncated);
        }
        let flags = u32_at(bytes, FADT_FLAGS);
        // 64-bit fields win when present (ACPI 6.5 §5.2.9).
        let dsdt = match bytes
            .get(FADT_X_DSDT..FADT_X_DSDT + 8)
            .map(|b| u64_at(b, 0))
        {
            Some(x) if x != 0 => x,
            _ => u64::from(u32_at(bytes, FADT_DSDT)),
        };
        let len = bytes[FADT_PM1_CNT_LEN];
        let pm1a_control = GenericAddress::at(bytes, FADT_X_PM1A_CNT)
            .or_else(|| GenericAddress::io(u32_at(bytes, FADT_PM1A_CNT), len));
        let pm1b_control = GenericAddress::at(bytes, FADT_X_PM1B_CNT)
            .or_else(|| GenericAddress::io(u32_at(bytes, FADT_PM1B_CNT), len));
        let reset = if flags & FLAG_RESET_REG_SUP != 0 {
            GenericAddress::at(bytes, FADT_RESET_REG).zip(bytes.get(FADT_RESET_VALUE).copied())
        } else {
            None
        };
        Ok(Self {
            dsdt,
            smi_command: u32_at(bytes, FADT_SMI_CMD),
            acpi_enable: bytes[FADT_ACPI_ENABLE],
            pm1a_control,
            pm1b_control,
            reset,
            hardware_reduced: flags & FLAG_HW_REDUCED_ACPI != 0,
            sleep_control: GenericAddress::at(bytes, FADT_SLEEP_CONTROL),
        })
    }
}

/// The SLP_TYPa and SLP_TYPb values of sleep state `\_Sx` (`name` is
/// `*b"_S5_"` for soft off), found by scanning AML for `Name (_Sx,
/// Package () { a, b, ... })`, the form firmware uses. A method or any
/// other form gives `None`. Each value is cut to its 3 bits.
pub fn sleep_type(aml: &[u8], name: [u8; 4]) -> Option<(u8, u8)> {
    const NAME_OP: u8 = 0x08;
    const PACKAGE_OP: u8 = 0x12;
    const ROOT: u8 = b'\\';
    let mut from = 0;
    while let Some(found) = aml
        .get(from..)?
        .windows(4)
        .position(|window| window == name)
    {
        let at = from + found;
        from = at + 1;
        let named = match at {
            0 => false,
            1 => aml[0] == NAME_OP,
            _ => aml[at - 1] == NAME_OP || (aml[at - 1] == ROOT && aml[at - 2] == NAME_OP),
        };
        if !named || aml.get(at + 4) != Some(&PACKAGE_OP) {
            continue;
        }
        if let Some(types) = package_sleep_types(&aml[at + 5..]) {
            return Some(types);
        }
    }
    None
}

/// `PkgLength NumElements a [b ...]` of a sleep package.
fn package_sleep_types(bytes: &[u8]) -> Option<(u8, u8)> {
    let lead = *bytes.first()?;
    // Bits 7-6: how many more PkgLength bytes follow.
    let length_bytes = 1 + usize::from(lead >> 6);
    let elements = *bytes.get(length_bytes)?;
    if elements == 0 {
        return None;
    }
    let mut rest = bytes.get(length_bytes + 1..)?;
    let a = aml_integer(&mut rest)?;
    let b = if elements >= 2 {
        aml_integer(&mut rest)?
    } else {
        0
    };
    Some(((a & 0b111) as u8, (b & 0b111) as u8))
}

/// One AML integer constant, consumed from the front of `bytes`.
fn aml_integer(bytes: &mut &[u8]) -> Option<u64> {
    let (&op, rest) = bytes.split_first()?;
    let (value, used) = match op {
        0x00 => (0, 0),
        0x01 => (1, 0),
        0x0a => (u64::from(*rest.first()?), 1),
        0x0b => (u64::from(u16_at(rest.get(..2)?, 0)), 2),
        0x0c => (u64::from(u32_at(rest.get(..4)?, 0)), 4),
        0x0e => (u64_at(rest.get(..8)?, 0), 8),
        _ => return None,
    };
    *bytes = &rest[used..];
    Some(value)
}

/// MPS INTI flags: active low polarity.
pub fn active_low(flags: u16) -> bool {
    flags & 0b11 == 0b11
}

/// MPS INTI flags: level triggered.
pub fn level_triggered(flags: u16) -> bool {
    (flags >> 2) & 0b11 == 0b11
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn fix_checksum(bytes: &mut [u8], at: usize) {
        bytes[at] = 0;
        let sum = bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        bytes[at] = 0u8.wrapping_sub(sum);
    }

    fn rsdp_v2(xsdt: u64) -> Vec<u8> {
        let mut bytes = std::vec![0u8; 36];
        bytes[..8].copy_from_slice(RSDP_SIGNATURE);
        bytes[15] = 2;
        bytes[16..20].copy_from_slice(&0x1234u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&36u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&xsdt.to_le_bytes());
        fix_checksum(&mut bytes[..20], 8);
        fix_checksum(&mut bytes, 32);
        bytes
    }

    fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = std::vec![0u8; SDT_HEADER_LEN];
        bytes[..4].copy_from_slice(signature);
        bytes.extend_from_slice(body);
        let len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        fix_checksum(&mut bytes, 9);
        bytes
    }

    fn madt_body() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0xfee0_0000u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]); // local APIC 0
        body.extend_from_slice(&[1, 12, 0, 0]); // I/O APIC id 0
        body.extend_from_slice(&0xfec0_0000u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&[2, 10, 0, 0]); // ISA IRQ 0 -> GSI 2
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&[2, 10, 0, 9]); // IRQ 9 -> 9, level, active low
        body.extend_from_slice(&9u32.to_le_bytes());
        body.extend_from_slice(&0b1111u16.to_le_bytes());
        body
    }

    #[test]
    fn parses_rsdp_v2_and_v1() {
        assert_eq!(
            parse_rsdp(&rsdp_v2(0xdead_0000)),
            Ok(RootTable::Xsdt(0xdead_0000))
        );
        let mut v1 = rsdp_v2(0)[..20].to_vec();
        v1[15] = 0;
        fix_checksum(&mut v1, 8);
        assert_eq!(parse_rsdp(&v1), Ok(RootTable::Rsdt(0x1234)));
    }

    #[test]
    fn rejects_bad_rsdp() {
        let mut bad = rsdp_v2(1);
        bad[0] = b'X';
        assert_eq!(parse_rsdp(&bad), Err(AcpiError::BadSignature));
        let mut bad = rsdp_v2(1);
        bad[17] ^= 1;
        assert_eq!(parse_rsdp(&bad), Err(AcpiError::BadChecksum));
        let mut bad = rsdp_v2(1);
        bad[30] ^= 1; // extended checksum
        assert_eq!(parse_rsdp(&bad), Err(AcpiError::BadChecksum));
        assert_eq!(parse_rsdp(&[0; 10]), Err(AcpiError::Truncated));
    }

    #[test]
    fn validates_tables_and_lists_root_entries() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x1000u64.to_le_bytes());
        body.extend_from_slice(&0x2000u64.to_le_bytes());
        let xsdt = table(b"XSDT", &body);
        let parsed = Table::parse(&xsdt).unwrap();
        let entries: Vec<u64> = root_entries(parsed, RootTable::Xsdt(0)).collect();
        assert_eq!(entries, [0x1000, 0x2000]);

        let mut corrupt = xsdt.clone();
        corrupt[40] ^= 1;
        assert_eq!(Table::parse(&corrupt).err(), Some(AcpiError::BadChecksum));
        assert_eq!(Table::parse(&xsdt[..30]).err(), Some(AcpiError::Truncated));
        let mut short = xsdt.clone();
        short[4..8].copy_from_slice(&10u32.to_le_bytes());
        assert_eq!(Table::parse(&short).err(), Some(AcpiError::BadLength));
    }

    #[test]
    fn parses_madt_entries_and_isa_overrides() {
        let bytes = table(b"APIC", &madt_body());
        let madt = Madt::parse(Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(madt.local_apic_address, 0xfee0_0000);
        let entries: Vec<MadtEntry> = madt.entries().collect();
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries[1],
            MadtEntry::IoApic {
                id: 0,
                address: 0xfec0_0000,
                gsi_base: 0
            }
        );
        assert_eq!(madt.isa_irq(0), (2, 0), "timer override");
        assert_eq!(madt.isa_irq(4), (4, 0), "COM1: identity, ISA defaults");
        let (gsi, flags) = madt.isa_irq(9);
        assert_eq!(gsi, 9);
        assert!(active_low(flags) && level_triggered(flags));
        assert!(!active_low(0) && !level_triggered(0));
    }

    fn mcfg_entry(base: u64, segment: u16, start: u8, end: u8) -> Vec<u8> {
        let mut entry = Vec::new();
        entry.extend_from_slice(&base.to_le_bytes());
        entry.extend_from_slice(&segment.to_le_bytes());
        entry.extend_from_slice(&[start, end, 0, 0, 0, 0]);
        entry
    }

    #[test]
    fn parses_mcfg_regions() {
        let mut body = std::vec![0u8; 8];
        body.extend(mcfg_entry(0xb000_0000, 0, 0, 255));
        body.extend(mcfg_entry(0, 1, 0, 3)); // zero base: skipped
        body.extend(mcfg_entry(0xc000_0000, 2, 9, 4)); // inverted: skipped
        body.extend(mcfg_entry(0xd000_0000, 3, 16, 31));
        body.extend_from_slice(&[0; 7]); // trailing partial entry: ignored
        let bytes = table(b"MCFG", &body);
        let regions: Vec<EcamRegion> = mcfg_regions(Table::parse(&bytes).unwrap())
            .unwrap()
            .collect();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].base, 0xb000_0000);
        assert_eq!(regions[1].segment, 3);

        let q35 = regions[0];
        assert_eq!(q35.function_address(0, 0, 0), Some(0xb000_0000));
        assert_eq!(
            q35.function_address(1, 2, 3),
            Some(0xb000_0000 + (1 << 20) + (2 << 15) + (3 << 12))
        );
        assert_eq!(q35.function_address(0, 32, 0), None);
        assert_eq!(q35.function_address(0, 0, 8), None);
        let high = regions[1];
        assert_eq!(high.function_address(15, 0, 0), None, "below start bus");
        assert_eq!(
            high.function_address(16, 0, 0),
            Some(0xd000_0000),
            "start bus is offset 0"
        );

        assert!(mcfg_regions(Table::parse(&table(b"APIC", &[0; 8])).unwrap()).is_err());
        let short = table(b"MCFG", &[0; 4]);
        assert!(mcfg_regions(Table::parse(&short).unwrap()).is_err());
    }

    fn gas(space: u8, width: u8, address: u64) -> [u8; 12] {
        let mut raw = [0u8; 12];
        raw[0] = space;
        raw[1] = width;
        raw[3] = 1;
        raw[4..].copy_from_slice(&address.to_le_bytes());
        raw
    }

    /// A FADT of `length` bytes (header included), as QEMU's q35 has it.
    fn fadt(length: usize, flags: u32) -> Vec<u8> {
        let mut body = std::vec![0u8; length - SDT_HEADER_LEN];
        let mut put = |at: usize, bytes: &[u8]| {
            body[at - SDT_HEADER_LEN..at - SDT_HEADER_LEN + bytes.len()].copy_from_slice(bytes)
        };
        put(FADT_DSDT, &0x7ff0_0000u32.to_le_bytes());
        put(FADT_SMI_CMD, &0xb2u32.to_le_bytes());
        put(FADT_ACPI_ENABLE, &[0xf1]);
        put(FADT_PM1A_CNT, &0x604u32.to_le_bytes());
        put(FADT_PM1_CNT_LEN, &[2]);
        put(FADT_FLAGS, &flags.to_le_bytes());
        if length > FADT_RESET_VALUE {
            put(FADT_RESET_REG, &gas(space::SYSTEM_IO, 8, 0xcf9));
            put(FADT_RESET_VALUE, &[0x0f]);
        }
        table(b"FACP", &body)
    }

    #[test]
    fn parses_fadt_power_registers() {
        let bytes = fadt(244, FLAG_RESET_REG_SUP);
        let parsed = Fadt::parse(Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(parsed.dsdt, 0x7ff0_0000);
        assert_eq!((parsed.smi_command, parsed.acpi_enable), (0xb2, 0xf1));
        let pm1a = parsed.pm1a_control.unwrap();
        assert_eq!((pm1a.space, pm1a.bit_width, pm1a.address), (1, 16, 0x604));
        assert_eq!(parsed.pm1b_control, None);
        let (reset, value) = parsed.reset.unwrap();
        assert_eq!((reset.space, reset.address, value), (1, 0xcf9, 0x0f));
        assert!(!parsed.hardware_reduced);

        // Without RESET_REG_SUP the register is not to be used.
        let parsed = Fadt::parse(Table::parse(&fadt(244, 0)).unwrap()).unwrap();
        assert_eq!(parsed.reset, None);
        // ACPI 1.0: 116 bytes, no reset register, no 64-bit fields.
        let parsed = Fadt::parse(Table::parse(&fadt(116, FLAG_RESET_REG_SUP)).unwrap()).unwrap();
        assert_eq!(parsed.reset, None);
        assert_eq!(parsed.pm1a_control.unwrap().address, 0x604);
        assert_eq!(
            Fadt::parse(Table::parse(&table(b"FACP", &[0; 40])).unwrap()).err(),
            Some(AcpiError::Truncated)
        );
        assert_eq!(
            Fadt::parse(Table::parse(&table(b"APIC", &[0; 200])).unwrap()).err(),
            Some(AcpiError::BadSignature)
        );
    }

    #[test]
    fn fadt_prefers_64_bit_fields_and_knows_hardware_reduced() {
        let mut bytes = fadt(268, FLAG_HW_REDUCED_ACPI);
        bytes[FADT_X_DSDT..FADT_X_DSDT + 8].copy_from_slice(&0x1_0000_0000u64.to_le_bytes());
        bytes[FADT_X_PM1A_CNT..FADT_X_PM1A_CNT + 12].copy_from_slice(&gas(
            space::SYSTEM_MEMORY,
            16,
            0xfed0_0004,
        ));
        bytes[FADT_SLEEP_CONTROL..FADT_SLEEP_CONTROL + 12].copy_from_slice(&gas(
            space::SYSTEM_IO,
            8,
            0x1004,
        ));
        fix_checksum(&mut bytes, 9);
        let parsed = Fadt::parse(Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(parsed.dsdt, 0x1_0000_0000);
        let pm1a = parsed.pm1a_control.unwrap();
        assert_eq!(
            (pm1a.space, pm1a.address),
            (space::SYSTEM_MEMORY, 0xfed0_0004)
        );
        assert!(parsed.hardware_reduced);
        assert_eq!(parsed.sleep_control.unwrap().address, 0x1004);
    }

    #[test]
    fn finds_sleep_types_in_aml() {
        // QEMU: Name (_S5, Package (0x04) { Zero, Zero, Zero, Zero })
        let qemu = [
            0x10, 0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0, 0, 0, 0,
        ];
        assert_eq!(sleep_type(&qemu, *b"_S5_"), Some((0, 0)));
        // A PC: Name (\_S5, Package () { 0x07, 0x07, ... }) after a method
        // that merely mentions _S5_.
        let mut pc = std::vec![0x14, 0x05, b'_', b'S', b'5', b'_', 0x00];
        pc.extend_from_slice(&[0x08, b'\\', b'_', b'S', b'5', b'_', 0x12, 0x0a, 0x04]);
        pc.extend_from_slice(&[0x0a, 0x07, 0x0a, 0x07, 0x00, 0x00]);
        assert_eq!(sleep_type(&pc, *b"_S5_"), Some((7, 7)));
        // Two-byte PkgLength, word and one-element forms; values cut to 3 bits.
        let long = [
            0x08, b'_', b'S', b'5', b'_', 0x12, 0x40, 0x01, 0x02, 0x0b, 0x0d, 0x00, 0x01,
        ];
        assert_eq!(sleep_type(&long, *b"_S5_"), Some((5, 1)));
        let single = [0x08, b'_', b'S', b'5', b'_', 0x12, 0x03, 0x01, 0x01];
        assert_eq!(sleep_type(&single, *b"_S5_"), Some((1, 0)));
        // Not there, a method, truncated, empty, or not integers.
        assert_eq!(sleep_type(&qemu, *b"_S4_"), None);
        assert_eq!(
            sleep_type(&[0x14, 0x05, b'_', b'S', b'5', b'_', 0x12], *b"_S5_"),
            None
        );
        assert_eq!(sleep_type(&qemu[..10], *b"_S5_"), None);
        assert_eq!(
            sleep_type(&[0x08, b'_', b'S', b'5', b'_', 0x12, 0x02, 0x00], *b"_S5_"),
            None
        );
        assert_eq!(
            sleep_type(
                &[0x08, b'_', b'S', b'5', b'_', 0x12, 0x03, 0x01, 0x70],
                *b"_S5_"
            ),
            None
        );
        assert_eq!(sleep_type(b"_S5_", *b"_S5_"), None);
        assert_eq!(sleep_type(&[], *b"_S5_"), None);
    }

    #[test]
    fn malformed_madt_entries_stop_iteration() {
        let mut body = madt_body();
        body.extend_from_slice(&[1, 200]); // claims 200 bytes, has 2
        let bytes = table(b"APIC", &body);
        let madt = Madt::parse(Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(madt.entries().count(), 4);
        let mut body = madt_body();
        body.truncate(8);
        body.extend_from_slice(&[0, 0]); // zero length would loop forever
        let bytes = table(b"APIC", &body);
        let madt = Madt::parse(Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(madt.entries().count(), 0);
        assert_eq!(
            Madt::parse(Table::parse(&table(b"FACP", &[0; 8])).unwrap()).err(),
            Some(AcpiError::BadSignature)
        );
    }
}
