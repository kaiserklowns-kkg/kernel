//! ACPI discovery (ADR-0017, ADR-0021, ADR-0085): the MADT for interrupt
//! routing, the MCFG for PCI Express configuration space, and the FADT and
//! the `\_S5` sleep types for switching off and restarting (`power`).
//!
//! Parsing and validation live in `oceans-acpi`; this module only reads the
//! physical tables. Only what the kernel needs is used: the I/O APICs, the
//! ISA interrupt overrides, the ECAM regions and the power registers. No
//! AML is interpreted: the DSDT and SSDTs are only searched for `\_S5`.

use alloc::vec;
use alloc::vec::Vec;

use oceans_acpi::{
    AcpiError, EcamRegion, Fadt, Madt, MadtEntry, RootTable, SDT_HEADER_LEN, Table, mcfg_regions,
    parse_rsdp, root_entries, sleep_type, table_length,
};

use crate::boot::BootInfo;
use crate::memory::read_physical;
use crate::{klog, power};

/// Largest table the kernel copies (MADTs are a few hundred bytes).
const MAX_TABLE: usize = 64 * 1024;
/// Largest DSDT or SSDT searched for `\_S5` (a PC's DSDT is up to a few
/// hundred KiB); copied only for the search.
const MAX_AML: usize = 4 * 1024 * 1024;

/// What the kernel keeps from the firmware tables.
pub struct Acpi {
    /// A validated copy of the MADT, parsed on demand.
    madt: Option<Vec<u8>>,
    ecam: Vec<EcamRegion>,
}

/// An I/O APIC and the first global system interrupt it serves.
#[derive(Clone, Copy, Debug)]
pub struct IoApic {
    pub address: u64,
    pub gsi_base: u32,
}

impl Acpi {
    fn madt(&self) -> Option<Madt<'_>> {
        let bytes = self.madt.as_deref()?;
        Some(
            Madt::parse(Table::parse(bytes).expect("validated at discovery"))
                .expect("validated at discovery"),
        )
    }

    /// The global system interrupt and MPS flags of ISA `irq`, if the
    /// firmware described interrupt routing.
    pub fn isa_irq(&self, irq: u8) -> Option<(u32, u16)> {
        Some(self.madt()?.isa_irq(irq))
    }

    /// The I/O APIC serving `gsi`: the one with the highest base not above it.
    pub fn io_apic_for(&self, gsi: u32) -> Option<IoApic> {
        self.madt()?
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

    /// PCI Express configuration regions (empty without an MCFG).
    pub fn ecam_regions(&self) -> &[EcamRegion] {
        &self.ecam
    }
}

/// Reads and validates the tables, and tells `power` how this machine
/// switches off. `None` (logged) if the firmware has no usable ACPI.
pub fn discover(boot: &BootInfo) -> Option<Acpi> {
    let mut fadt = None;
    let mut ssdts = Vec::new();
    let acpi = discover_tables(boot, &mut fadt, &mut ssdts);
    let types = fadt.as_ref().and_then(|fadt: &Fadt| {
        let dsdt = (fadt.dsdt != 0).then_some(fadt.dsdt);
        dsdt.into_iter()
            .chain(ssdts.iter().copied())
            .find_map(sleep_type_s5)
    });
    power::init(fadt.as_ref(), types);
    acpi
}

/// The `\_S5` sleep types of the definition block at `address`, if it has
/// them.
fn sleep_type_s5(address: u64) -> Option<(u8, u8)> {
    let bytes = read_table_up_to(address, MAX_AML)?;
    let body = match Table::parse(&bytes) {
        Ok(table) => table.body(),
        // Firmware ships DSDTs with wrong checksums; the search is
        // bounds-checked and its result cut to 3 bits, so it still runs.
        Err(AcpiError::BadChecksum) => {
            klog::warn!("ACPI: definition block at {address:#x} has a bad checksum");
            &bytes[SDT_HEADER_LEN..]
        }
        Err(_) => return None,
    };
    sleep_type(body, *b"_S5_")
}

fn discover_tables(boot: &BootInfo, fadt: &mut Option<Fadt>, ssdts: &mut Vec<u64>) -> Option<Acpi> {
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

    let mut acpi = Acpi {
        madt: None,
        ecam: Vec::new(),
    };
    for address in root_entries(root, root_kind) {
        let mut header = [0u8; SDT_HEADER_LEN];
        if read(address, &mut header).is_none() {
            continue;
        }
        match &header[..4] {
            b"APIC" if acpi.madt.is_none() => {
                let Some(madt) = read_table(address) else {
                    continue;
                };
                match Table::parse(&madt).and_then(Madt::parse) {
                    Ok(_) => acpi.madt = Some(madt),
                    Err(err) => klog::warn!("invalid MADT: {err:?}"),
                }
            }
            b"FACP" if fadt.is_none() => {
                let Some(bytes) = read_table(address) else {
                    continue;
                };
                match Table::parse(&bytes).and_then(Fadt::parse) {
                    Ok(parsed) => *fadt = Some(parsed),
                    Err(err) => klog::warn!("invalid FADT: {err:?}"),
                }
            }
            b"SSDT" => ssdts.push(address),
            b"MCFG" if acpi.ecam.is_empty() => {
                let Some(mcfg) = read_table(address) else {
                    continue;
                };
                match Table::parse(&mcfg).and_then(mcfg_regions) {
                    Ok(regions) => acpi.ecam.extend(regions),
                    Err(err) => klog::warn!("invalid MCFG: {err:?}"),
                }
            }
            _ => {}
        }
    }

    match acpi.madt() {
        Some(madt) => {
            let io_apics = madt
                .entries()
                .filter(|e| matches!(e, MadtEntry::IoApic { .. }))
                .count();
            klog::info!("ACPI: MADT found, {io_apics} I/O APIC(s)");
        }
        None => klog::warn!("ACPI: no MADT"),
    }
    match acpi.ecam.len() {
        0 => klog::warn!("ACPI: no MCFG; PCI devices are unavailable"),
        n => klog::info!("ACPI: MCFG found, {n} PCI Express configuration region(s)"),
    }
    Some(acpi)
}

fn read(address: u64, out: &mut [u8]) -> Option<()> {
    read_physical(address, out)
        .map_err(|err| klog::warn!("cannot read ACPI memory at {address:#x}: {err:?}"))
        .ok()
}

/// Copies the whole table at `address` (header first, to learn its length).
fn read_table(address: u64) -> Option<Vec<u8>> {
    read_table_up_to(address, MAX_TABLE)
}

fn read_table_up_to(address: u64, max: usize) -> Option<Vec<u8>> {
    let mut header = [0u8; SDT_HEADER_LEN];
    read(address, &mut header)?;
    let length = table_length(&header).ok()?;
    if length > max {
        klog::warn!("ACPI table at {address:#x} too large ({length} bytes)");
        return None;
    }
    let mut table = vec![0u8; length];
    read(address, &mut table)?;
    Some(table)
}
