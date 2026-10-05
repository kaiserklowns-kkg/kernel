//! AHCI for Oceans (ADR-0069): the parts of the Serial ATA AHCI
//! specification (1.3.1) and of the ATA command set (ACS) a driver of SATA
//! disks needs, as plain data, without allocation.
//!
//! - [`hba`], [`ghc`] and [`port`] name the controller's registers (the
//!   `ABAR`, BAR 5); [`Capabilities`], [`LinkStatus`] and [`TaskFile`]
//!   decode the ones a driver reads.
//! - [`layout`] places one port's command list, received-FIS area and
//!   command table in one page of DMA memory, with the alignments the
//!   specification demands.
//! - [`AtaCommand`] builds the commands the driver issues as host-to-device
//!   register FISes; [`command_header`] and [`prd`] describe them to the
//!   controller.
//! - [`Identity`] parses `IDENTIFY DEVICE` data.
//! - [`chunk_sectors`] splits a request into transfers that fit the bounce
//!   buffer.
//!
//! Everything a device reports is bounds-checked here: a controller or a
//! disk is not trusted to keep within the sizes it announced.

#![no_std]

#[cfg(test)]
mod tests;

/// The sector size of the Oceans block protocol, and the only logical
/// sector size served.
pub const SECTOR_SIZE: usize = 512;
/// Bytes of `IDENTIFY DEVICE` data.
pub const IDENTIFY_SIZE: usize = 512;
/// The largest sector count of one 48-bit command (a count of 0 means
/// 65 536, never used here).
pub const MAX_COMMAND_SECTORS: u32 = 65_535;
/// The largest number of bytes one PRD entry describes (22-bit count).
pub const MAX_PRD_BYTES: usize = 4 << 20;
/// Highest sector a 48-bit command can name, plus one.
pub const LBA48_LIMIT: u64 = 1 << 48;

/// Generic host control registers, offsets in the `ABAR`.
pub mod hba {
    /// Host capabilities.
    pub const CAP: usize = 0x00;
    /// Global host control.
    pub const GHC: usize = 0x04;
    /// Interrupt status (one bit per port).
    pub const IS: usize = 0x08;
    /// Ports implemented.
    pub const PI: usize = 0x0c;
    /// Version.
    pub const VS: usize = 0x10;
    /// Host capabilities extended.
    pub const CAP2: usize = 0x24;
    /// BIOS/OS handoff control and status.
    pub const BOHC: usize = 0x28;
    /// The first port's registers.
    pub const PORTS: usize = 0x100;
    /// Bytes of registers per port.
    pub const PORT_SIZE: usize = 0x80;
    /// The most ports a controller has.
    pub const MAX_PORTS: u32 = 32;

    /// Where port `port`'s registers start.
    pub const fn port(port: u32) -> usize {
        PORTS + port as usize * PORT_SIZE
    }
}

/// Global host control (`GHC`) bits.
pub mod ghc {
    /// HBA reset.
    pub const RESET: u32 = 1 << 0;
    /// Interrupt enable.
    pub const INTERRUPTS: u32 = 1 << 1;
    /// AHCI enable: the controller speaks AHCI, not legacy IDE.
    pub const AHCI_ENABLE: u32 = 1 << 31;
}

/// `CAP2` bits.
pub mod cap2 {
    /// BIOS/OS handoff supported.
    pub const HANDOFF: u32 = 1 << 0;
}

/// BIOS/OS handoff (`BOHC`) bits.
pub mod bohc {
    /// The firmware owns the controller.
    pub const BIOS_OWNED: u32 = 1 << 0;
    /// The OS asks for (and then owns) the controller.
    pub const OS_OWNED: u32 = 1 << 1;
    /// The firmware is busy finishing outstanding commands.
    pub const BIOS_BUSY: u32 = 1 << 4;
}

/// Port registers, offsets from the port's start ([`hba::port`]).
pub mod port {
    /// Command list base address (low, high).
    pub const CLB: usize = 0x00;
    pub const CLBU: usize = 0x04;
    /// Received-FIS base address (low, high).
    pub const FB: usize = 0x08;
    pub const FBU: usize = 0x0c;
    /// Interrupt status (write 1 to clear).
    pub const IS: usize = 0x10;
    /// Interrupt enable.
    pub const IE: usize = 0x14;
    /// Command and status.
    pub const CMD: usize = 0x18;
    /// Task file data: the device's status and error registers.
    pub const TFD: usize = 0x20;
    /// The attached device's signature.
    pub const SIG: usize = 0x24;
    /// SATA status (`SStatus`).
    pub const SSTS: usize = 0x28;
    /// SATA control (`SControl`).
    pub const SCTL: usize = 0x2c;
    /// SATA error (`SError`, write 1 to clear).
    pub const SERR: usize = 0x30;
    /// SATA active (native command queuing; unused).
    pub const SACT: usize = 0x34;
    /// Command issue: one bit per command slot.
    pub const CI: usize = 0x38;
}

/// Port command and status (`PxCMD`) bits.
pub mod cmd {
    /// Start: the controller processes the command list.
    pub const START: u32 = 1 << 0;
    /// Spin-up device (with staggered spin-up).
    pub const SPIN_UP: u32 = 1 << 1;
    /// Power on device (with cold presence detection).
    pub const POWER_ON: u32 = 1 << 2;
    /// FIS receive enable.
    pub const FIS_RECEIVE: u32 = 1 << 4;
    /// FIS receive running.
    pub const FIS_RUNNING: u32 = 1 << 14;
    /// Command list running.
    pub const LIST_RUNNING: u32 = 1 << 15;
}

/// Port interrupt status (`PxIS`) bits.
pub mod is {
    /// A device-to-host register FIS arrived.
    pub const D2H_REGISTER: u32 = 1 << 0;
    /// Interface non-fatal error.
    pub const INTERFACE_NON_FATAL: u32 = 1 << 26;
    /// Interface fatal error.
    pub const INTERFACE_FATAL: u32 = 1 << 27;
    /// Host bus data error.
    pub const HOST_BUS_DATA: u32 = 1 << 28;
    /// Host bus fatal error.
    pub const HOST_BUS_FATAL: u32 = 1 << 29;
    /// Task file error: the device reported an error.
    pub const TASK_FILE_ERROR: u32 = 1 << 30;
    /// The errors that stop the command list (the port must be restarted).
    pub const FATAL: u32 = INTERFACE_FATAL | HOST_BUS_DATA | HOST_BUS_FATAL | TASK_FILE_ERROR;
}

/// `SControl` fields.
pub mod sctl {
    /// Device detection initialisation: 1 sends COMRESET, 0 releases it.
    pub const DET_MASK: u32 = 0xf;
    pub const DET_COMRESET: u32 = 1;
    /// No transitions to the partial and slumber power states.
    pub const IPM_NO_PARTIAL_SLUMBER: u32 = 3 << 8;
}

/// Device signatures (`PxSIG`).
pub mod sig {
    /// A SATA disk (ATA).
    pub const ATA: u32 = 0x0000_0101;
    /// A SATAPI device (CD/DVD drives).
    pub const ATAPI: u32 = 0xeb14_0101;
    /// An enclosure management bridge.
    pub const SEMB: u32 = 0xc33c_0101;
    /// A port multiplier.
    pub const PORT_MULTIPLIER: u32 = 0x9669_0101;

    /// What a signature names, for the log.
    pub fn name(signature: u32) -> &'static str {
        match signature {
            ATA => "a SATA disk",
            ATAPI => "an ATAPI device",
            SEMB => "an enclosure bridge",
            PORT_MULTIPLIER => "a port multiplier",
            u32::MAX => "no signature yet",
            _ => "an unknown device",
        }
    }
}

/// ATA commands.
pub mod ata {
    pub const IDENTIFY_DEVICE: u8 = 0xec;
    pub const READ_DMA_EXT: u8 = 0x25;
    pub const WRITE_DMA_EXT: u8 = 0x35;
    pub const FLUSH_CACHE: u8 = 0xe7;
    pub const FLUSH_CACHE_EXT: u8 = 0xea;
}

/// FIS types.
pub mod fis {
    /// Register, host to device.
    pub const H2D_REGISTER: u8 = 0x27;
    /// Length of a register host-to-device FIS in bytes (5 dwords).
    pub const H2D_LEN: usize = 20;
    /// The `C` bit: the FIS carries a command.
    pub const COMMAND: u8 = 0x80;
    /// The device register's LBA bit.
    pub const DEVICE_LBA: u8 = 1 << 6;
}

/// One port's memory, in one 4 KiB page of DMA memory.
pub mod layout {
    /// The command list: 32 headers of 32 bytes (1 KiB aligned).
    pub const COMMAND_LIST: usize = 0;
    pub const COMMAND_LIST_SIZE: usize = 32 * super::COMMAND_HEADER_SIZE;
    /// The received-FIS area (256-byte aligned).
    pub const RECEIVED_FIS: usize = 1024;
    pub const RECEIVED_FIS_SIZE: usize = 256;
    /// The command table of slot 0 (128-byte aligned): the command FIS at
    /// its start, the PRD table at [`PRDT`].
    pub const COMMAND_TABLE: usize = 2048;
    pub const PRDT: usize = COMMAND_TABLE + 0x80;
    /// The page.
    pub const SIZE: usize = 4096;
    /// PRD entries that fit the page.
    pub const MAX_PRDS: usize = (SIZE - PRDT) / super::PRD_SIZE;
}

/// Bytes per command header.
pub const COMMAND_HEADER_SIZE: usize = 32;
/// Bytes per PRD (physical region descriptor) entry.
pub const PRD_SIZE: usize = 16;

/// What the `CAP` register says about the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Ports the controller supports (`PI` says which are implemented).
    pub ports: u32,
    /// Command slots per port.
    pub slots: u32,
    /// 64-bit addresses for DMA (`S64A`); otherwise all below 4 GiB.
    pub addr64: bool,
    /// Staggered spin-up (`SSS`): ports must be told to spin up.
    pub staggered_spin_up: bool,
    /// AHCI only (`SAM`): no legacy mode to leave.
    pub ahci_only: bool,
}

impl Capabilities {
    pub fn decode(cap: u32) -> Self {
        Self {
            ports: (cap & 0x1f) + 1,
            slots: ((cap >> 8) & 0x1f) + 1,
            addr64: cap & (1 << 31) != 0,
            staggered_spin_up: cap & (1 << 27) != 0,
            ahci_only: cap & (1 << 18) != 0,
        }
    }

    /// Whether the controller can reach `len` bytes at device address
    /// `address`.
    pub fn reaches(&self, address: u64, len: usize) -> bool {
        self.addr64 || address.saturating_add(len as u64) <= 1 << 32
    }
}

/// The `VS` register as (major, minor, patch): `0x0001_0301` is 1.3.1.
pub fn version(vs: u32) -> (u16, u8, u8) {
    ((vs >> 16) as u16, (vs >> 8) as u8, vs as u8)
}

/// The implemented ports, lowest first, from `PI`.
pub fn implemented_ports(pi: u32) -> impl Iterator<Item = u32> {
    (0..hba::MAX_PORTS).filter(move |port| pi & (1 << port) != 0)
}

/// What `SStatus` says about a port's link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    /// Device detection: 0 none, 1 present without communication, 3
    /// present with communication, 4 offline.
    pub detection: u8,
    /// Negotiated speed: 1, 2 or 3 (generation); 0 none.
    pub speed: u8,
    /// Interface power: 1 active, 2 partial, 6 slumber, 8 devsleep.
    pub power: u8,
}

impl LinkStatus {
    pub const DEVICE_COMMUNICATING: u8 = 3;
    pub const POWER_ACTIVE: u8 = 1;

    pub fn decode(ssts: u32) -> Self {
        Self {
            detection: (ssts & 0xf) as u8,
            speed: ((ssts >> 4) & 0xf) as u8,
            power: ((ssts >> 8) & 0xf) as u8,
        }
    }

    /// A device is attached and talking, in the active power state.
    pub fn is_up(&self) -> bool {
        self.detection == Self::DEVICE_COMMUNICATING && self.power == Self::POWER_ACTIVE
    }

    /// Nothing is attached at all (no point waiting for the link).
    pub fn is_empty(&self) -> bool {
        self.detection == 0
    }

    pub fn speed_name(&self) -> &'static str {
        match self.speed {
            1 => "1.5 Gb/s",
            2 => "3 Gb/s",
            3 => "6 Gb/s",
            _ => "unknown speed",
        }
    }
}

/// The device's status and error registers, from `PxTFD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskFile {
    pub status: u8,
    pub error: u8,
}

impl TaskFile {
    pub const BUSY: u8 = 1 << 7;
    pub const DATA_REQUEST: u8 = 1 << 3;
    pub const ERROR: u8 = 1 << 0;

    /// Error register bits.
    pub const INTERFACE_CRC: u8 = 1 << 7;
    pub const UNCORRECTABLE: u8 = 1 << 6;
    pub const ID_NOT_FOUND: u8 = 1 << 4;
    pub const ABORTED: u8 = 1 << 2;

    pub fn decode(tfd: u32) -> Self {
        Self {
            status: tfd as u8,
            error: (tfd >> 8) as u8,
        }
    }

    /// The device is busy or wants to move data: no command may be issued.
    pub fn is_busy(&self) -> bool {
        self.status & (Self::BUSY | Self::DATA_REQUEST) != 0
    }

    pub fn has_error(&self) -> bool {
        self.status & Self::ERROR != 0
    }

    /// The sectors named do not exist on the disk.
    pub fn out_of_range(&self) -> bool {
        self.has_error() && self.error & Self::ID_NOT_FOUND != 0
    }

    /// What the device said, for the log.
    pub fn message(&self) -> &'static str {
        if !self.has_error() {
            return "no device error";
        }
        let error = self.error;
        if error & Self::INTERFACE_CRC != 0 {
            "interface CRC error"
        } else if error & Self::UNCORRECTABLE != 0 {
            "uncorrectable data error"
        } else if error & Self::ID_NOT_FOUND != 0 {
            "sector not found"
        } else if error & Self::ABORTED != 0 {
            "command aborted"
        } else {
            "device error"
        }
    }
}

/// What a port's interrupt status says went wrong, for the log.
pub fn port_error_message(status: u32) -> &'static str {
    if status & is::HOST_BUS_FATAL != 0 {
        "host bus fatal error"
    } else if status & is::HOST_BUS_DATA != 0 {
        "host bus data error"
    } else if status & is::INTERFACE_FATAL != 0 {
        "interface fatal error"
    } else if status & is::TASK_FILE_ERROR != 0 {
        "task file error"
    } else {
        "no port error"
    }
}

/// An ATA command, sent as a register host-to-device FIS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtaCommand {
    pub command: u8,
    pub lba: u64,
    /// Sectors (0 for commands without data).
    pub count: u16,
    pub device: u8,
}

impl AtaCommand {
    /// `IDENTIFY DEVICE`: 512 bytes in.
    pub fn identify() -> Self {
        Self {
            command: ata::IDENTIFY_DEVICE,
            lba: 0,
            count: 0,
            device: 0,
        }
    }

    /// `READ DMA EXT` of `count` sectors (1 to [`MAX_COMMAND_SECTORS`]).
    pub fn read(lba: u64, count: u16) -> Self {
        Self::transfer(ata::READ_DMA_EXT, lba, count)
    }

    /// `WRITE DMA EXT` of `count` sectors.
    pub fn write(lba: u64, count: u16) -> Self {
        Self::transfer(ata::WRITE_DMA_EXT, lba, count)
    }

    /// `FLUSH CACHE EXT`, or `FLUSH CACHE` for disks without it.
    pub fn flush(ext: bool) -> Self {
        Self {
            command: if ext {
                ata::FLUSH_CACHE_EXT
            } else {
                ata::FLUSH_CACHE
            },
            lba: 0,
            count: 0,
            device: 0,
        }
    }

    fn transfer(command: u8, lba: u64, count: u16) -> Self {
        debug_assert!(count > 0 && lba + u64::from(count) <= LBA48_LIMIT);
        Self {
            command,
            lba,
            count,
            device: fis::DEVICE_LBA,
        }
    }

    /// Data moves from memory to the device.
    pub fn writes(&self) -> bool {
        self.command == ata::WRITE_DMA_EXT
    }

    /// Bytes the command moves.
    pub fn data_len(&self) -> usize {
        match self.command {
            ata::IDENTIFY_DEVICE => IDENTIFY_SIZE,
            ata::READ_DMA_EXT | ata::WRITE_DMA_EXT => usize::from(self.count) * SECTOR_SIZE,
            _ => 0,
        }
    }

    /// The register host-to-device FIS.
    pub fn fis(&self) -> [u8; fis::H2D_LEN] {
        let lba = self.lba.to_le_bytes();
        let count = self.count.to_le_bytes();
        let mut out = [0u8; fis::H2D_LEN];
        out[0] = fis::H2D_REGISTER;
        out[1] = fis::COMMAND;
        out[2] = self.command;
        // Features (low) stay 0.
        out[4..7].copy_from_slice(&lba[..3]);
        out[7] = self.device;
        out[8..11].copy_from_slice(&lba[3..6]);
        // Features (high) stay 0.
        out[12..14].copy_from_slice(&count);
        out
    }
}

/// The command header of a slot: a command FIS of [`fis::H2D_LEN`] bytes,
/// the direction, `prds` PRD entries and the command table's address
/// (128-byte aligned). The byte count the controller writes back starts
/// at 0.
pub fn command_header(write: bool, prds: u16, table: u64) -> [u8; COMMAND_HEADER_SIZE] {
    debug_assert!(table.is_multiple_of(128));
    let mut dw0 = (fis::H2D_LEN / 4) as u32 | (u32::from(prds) << 16);
    if write {
        dw0 |= 1 << 6;
    }
    let mut out = [0u8; COMMAND_HEADER_SIZE];
    out[0..4].copy_from_slice(&dw0.to_le_bytes());
    out[8..16].copy_from_slice(&table.to_le_bytes());
    out
}

/// PRD entries needed for `len` bytes.
pub fn prd_count(len: usize) -> usize {
    len.div_ceil(MAX_PRD_BYTES)
}

/// PRD entry `index` describing `len` bytes of the physically contiguous
/// buffer at `address`: each entry covers up to [`MAX_PRD_BYTES`]. `None`
/// past the end. Byte counts are even (whole sectors), as required.
pub fn prd(address: u64, len: usize, index: usize) -> Option<[u8; PRD_SIZE]> {
    let start = index.checked_mul(MAX_PRD_BYTES)?;
    if start >= len {
        return None;
    }
    let bytes = (len - start).min(MAX_PRD_BYTES);
    debug_assert!(bytes.is_multiple_of(2));
    let mut out = [0u8; PRD_SIZE];
    out[0..8].copy_from_slice(&(address + start as u64).to_le_bytes());
    // Byte count minus one; no interrupt on completion (polled).
    out[12..16].copy_from_slice(&((bytes - 1) as u32).to_le_bytes());
    Some(out)
}

/// From `IDENTIFY DEVICE` data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    serial: [u8; 20],
    firmware: [u8; 8],
    model: [u8; 40],
    /// An ATA device (word 0 bit 15 clear), not a packet device.
    pub ata: bool,
    /// 48-bit addressing (`READ/WRITE DMA EXT`).
    pub lba48: bool,
    /// Addressable sectors (logical sectors), 48-bit count when supported.
    pub sectors: u64,
    /// Bytes per logical sector.
    pub logical_sector_size: u32,
    /// Bytes per physical sector (at least the logical size).
    pub physical_sector_size: u32,
    /// A volatile write cache exists and is on: writes need a flush.
    pub write_cache: bool,
    /// `FLUSH CACHE EXT` is supported.
    pub flush_ext: bool,
}

impl Identity {
    /// `None` if the data is short.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < IDENTIFY_SIZE {
            return None;
        }
        let word = |i: usize| u16::from_le_bytes([data[2 * i], data[2 * i + 1]]);
        // Words 82..87 are valid when bits 15:14 of 83 and 87 read 01.
        let valid = |w: u16| w & 0xc000 == 0x4000;
        let supported = if valid(word(83)) { word(83) } else { 0 };
        let enabled = if valid(word(87)) { word(85) } else { 0 };
        let lba48 = supported & (1 << 10) != 0;
        let sectors = if lba48 {
            u64::from(word(100))
                | (u64::from(word(101)) << 16)
                | (u64::from(word(102)) << 32)
                | (u64::from(word(103)) << 48)
        } else {
            u64::from(word(60)) | (u64::from(word(61)) << 16)
        };
        // Word 106: sector sizes, when bits 15:14 read 01.
        let sizes = word(106);
        let logical = if valid(sizes) && sizes & (1 << 12) != 0 {
            // Words 117-118: the logical sector size in 16-bit words.
            (u32::from(word(117)) | (u32::from(word(118)) << 16)).saturating_mul(2)
        } else {
            SECTOR_SIZE as u32
        };
        let per_physical = if valid(sizes) && sizes & (1 << 13) != 0 {
            1u32 << (sizes & 0xf)
        } else {
            1
        };
        Some(Self {
            serial: swapped(&data[20..40]),
            firmware: swapped(&data[46..54]),
            model: swapped(&data[54..94]),
            ata: word(0) & (1 << 15) == 0,
            lba48,
            sectors: sectors.min(LBA48_LIMIT),
            logical_sector_size: logical,
            physical_sector_size: logical.saturating_mul(per_physical),
            write_cache: word(82) & (1 << 5) != 0 && enabled & (1 << 5) != 0,
            flush_ext: supported & (1 << 13) != 0,
        })
    }

    pub fn serial(&self) -> &str {
        text(&self.serial)
    }

    pub fn model(&self) -> &str {
        text(&self.model)
    }

    pub fn firmware(&self) -> &str {
        text(&self.firmware)
    }
}

/// An ATA string field: two characters per word, the first in the high
/// byte.
fn swapped<const N: usize>(field: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let pairs = out.as_chunks_mut::<2>().0.iter_mut();
    for (pair, &[high, low]) in pairs.zip(field.as_chunks::<2>().0) {
        *pair = [low, high];
    }
    out
}

/// An ASCII identify field without its padding (spaces or NULs); `?` if it
/// is not printable ASCII.
fn text(field: &[u8]) -> &str {
    let start = field
        .iter()
        .position(|&b| b != b' ' && b != 0)
        .unwrap_or(field.len());
    let end = field
        .iter()
        .rposition(|&b| b != b' ' && b != 0)
        .map_or(start, |i| i + 1);
    let field = &field[start..end];
    if field.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        core::str::from_utf8(field).unwrap_or("?")
    } else {
        "?"
    }
}

/// How many of `remaining` sectors the next command moves: at most
/// `max` (the bounce buffer, at least 1) and [`MAX_COMMAND_SECTORS`].
pub fn chunk_sectors(remaining: u32, max: u32) -> u16 {
    debug_assert!(remaining > 0 && max > 0);
    remaining.min(max).min(MAX_COMMAND_SECTORS) as u16
}
