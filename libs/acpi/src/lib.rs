//! Minimal ACPI table parsing (ADR-0017): RSDP, RSDT/XSDT and the MADT.
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
