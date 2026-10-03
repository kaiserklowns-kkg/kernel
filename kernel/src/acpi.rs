//! ACPI discovery (ADR-0017): finds the MADT for interrupt routing.
//!
//! Parsing and validation live in `oceans-acpi`; this module only reads the
//! physical tables. Only what the kernel needs is used: the I/O APICs and
//! the ISA interrupt overrides. Everything else (power management, device
//! enumeration) belongs to userspace services later.

use alloc::vec;
use alloc::vec::Vec;

use oceans_acpi::{
    Madt, MadtEntry, RootTable, SDT_HEADER_LEN, Table, parse_rsdp, root_entries, table_length,
};

use crate::boot::BootInfo;
use crate::klog;
use crate::memory::read_physical;

/// Largest table the kernel copies (MADTs are a few hundred bytes).
const MAX_TABLE: usize = 64 * 1024;

/// A copy of the MADT, parsed on demand.
pub struct Acpi {
    madt: Vec<u8>,
}

/// An I/O APIC and the first global system interrupt it serves.
#[derive(Clone, Copy, Debug)]
pub struct IoApic {
    pub address: u64,
    pub gsi_base: u32,
}

impl Acpi {
    fn madt(&self) -> Madt<'_> {
        Madt::parse(Table::parse(&self.madt).expect("validated at discovery"))
            .expect("validated at discovery")
    }

    /// The global system interrupt and MPS flags of ISA `irq`.
    pub fn isa_irq(&self, irq: u8) -> (u32, u16) {
        self.madt().isa_irq(irq)
    }

    /// The I/O APIC serving `gsi`: the one with the highest base not above it.
    pub fn io_apic_for(&self, gsi: u32) -> Option<IoApic> {
        self.madt()
            .entries()
            .filter_map(|entry| match entry {
                MadtEntry::IoApic {
                    address, gsi_base, ..
                } if gsi_base <= gsi => Some(IoApic {
                    address: u64::from(address),
                    gsi_base,
                }),
                _ => None,
            })
            .max_by_key(|io| io.gsi_base)
    }
}

/// Reads and validates the tables. `None` (logged) if the firmware has no
/// usable ACPI.
pub fn discover(boot: &BootInfo) -> Option<Acpi> {
    let rsdp_address = boot.rsdp().or_else(|| {
        klog::warn!("bootloader reported no ACPI RSDP");
        None
    })?;
    let mut rsdp = [0u8; 36];
    read(rsdp_address, &mut rsdp)?;
    let root_kind = match parse_rsdp(&rsdp) {
        Ok(kind) => kind,
        Err(err) => {
            klog::warn!("invalid ACPI RSDP: {err:?}");
            return None;
        }
    };
    let root_address = match root_kind {
        RootTable::Rsdt(address) | RootTable::Xsdt(address) => address,
    };
    let root_bytes = read_table(root_address)?;
    let root = Table::parse(&root_bytes).ok()?;

    for address in root_entries(root, root_kind) {
        let mut header = [0u8; SDT_HEADER_LEN];
        read(address, &mut header)?;
        if &header[..4] != b"APIC" {
            continue;
        }
        let madt = read_table(address)?;
        if let Err(err) = Table::parse(&madt).and_then(Madt::parse) {
            klog::warn!("invalid MADT: {err:?}");
            return None;
        }
        let acpi = Acpi { madt };
        let io_apics = acpi
            .madt()
            .entries()
            .filter(|e| matches!(e, MadtEntry::IoApic { .. }))
            .count();
        klog::info!("ACPI: MADT found, {io_apics} I/O APIC(s)");
        return Some(acpi);
    }
    klog::warn!("ACPI: no MADT");
    None
}

fn read(address: u64, out: &mut [u8]) -> Option<()> {
    read_physical(address, out)
        .map_err(|err| klog::warn!("cannot read ACPI memory at {address:#x}: {err:?}"))
        .ok()
}

/// Copies the whole table at `address` (header first, to learn its length).
fn read_table(address: u64) -> Option<Vec<u8>> {
    let mut header = [0u8; SDT_HEADER_LEN];
    read(address, &mut header)?;
    let length = table_length(&header).ok()?;
    if length > MAX_TABLE {
        klog::warn!("ACPI table at {address:#x} too large ({length} bytes)");
        return None;
    }
    let mut table = vec![0u8; length];
    read(address, &mut table)?;
    Some(table)
}
