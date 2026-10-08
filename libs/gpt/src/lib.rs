//! GPT partition tables (ADR-0092): reading them, making one, and swapping
//! two entries so that a power cut at any moment leaves a table that every
//! reader agrees on.
//!
//! A GPT disk keeps two copies of the table: the primary (header at LBA 1,
//! entries after it) and the backup (entries before the last LBA, header
//! there). Each header carries a CRC of itself and of its entries.
//! Firmware checks them and uses the backup when the primary is damaged
//! (edk2 then restores the primary from it); simpler readers take the
//! primary as it is.
//!
//! [`swap`] writes, each followed by a barrier:
//! 1. the backup's entries;
//! 2. the backup's header (the backup is now the new table, the primary
//!    still the old one: every reader still sees the old one);
//! 3. the primary's entries: **the switch**, one sector, so it is on the
//!    disk whole or not at all. The primary's header no longer matches
//!    them: readers that check fall back to the backup, which is new;
//!    readers that do not check read the new entries;
//! 4. the primary's header.
//!
//! [`repair`] finishes or undoes what a power cut interrupted, the way
//! firmware does: a damaged copy is rewritten from the good one.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use oceans_fat::{Disk, IoError};

pub const SECTOR: u64 = 512;
/// Bytes per entry and entries per table, as every tool makes them.
pub const ENTRY_SIZE: usize = 128;
pub const ENTRIES: usize = 128;
const TABLE_BYTES: usize = ENTRY_SIZE * ENTRIES;
const TABLE_SECTORS: u64 = TABLE_BYTES as u64 / SECTOR;
const SIGNATURE: &[u8; 8] = b"EFI PART";
const HEADER_SIZE: usize = 92;

/// The EFI system partition type, as stored (mixed-endian).
pub const EFI_SYSTEM: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
/// An Oceans boot slot (ADR-0092), `0e5ea0b5-...`: a FAT volume holding
/// one signed release. Its own type, so other systems leave it alone.
pub const OCEANS_SLOT: [u8; 16] = [
    0xb5, 0xa0, 0x5e, 0x0e, 0x9b, 0x1c, 0x4e, 0x4f, 0x8a, 0x3d, 0x0c, 0xea, 0x45, 0x05, 0x10, 0x92,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io,
    /// No valid GPT header in either place.
    NotGpt,
    /// A header names entries this code does not handle, or out of range.
    Unsupported,
    /// No partition in this entry.
    NoSuchEntry,
}

impl From<IoError> for Error {
    fn from(IoError: IoError) -> Self {
        Self::Io
    }
}

/// One partition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub kind: [u8; 16],
    pub guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: String,
}

impl Partition {
    /// Where it starts, in bytes.
    pub fn start(&self) -> u64 {
        self.first_lba * SECTOR
    }

    /// Its size, in bytes.
    pub fn size(&self) -> u64 {
        (self.last_lba + 1 - self.first_lba) * SECTOR
    }

    fn encode(&self, out: &mut [u8]) {
        out.fill(0);
        out[..16].copy_from_slice(&self.kind);
        out[16..32].copy_from_slice(&self.guid);
        out[32..40].copy_from_slice(&self.first_lba.to_le_bytes());
        out[40..48].copy_from_slice(&self.last_lba.to_le_bytes());
        for (i, unit) in self.name.encode_utf16().take(36).enumerate() {
            out[56 + 2 * i..58 + 2 * i].copy_from_slice(&unit.to_le_bytes());
        }
    }

    fn decode(raw: &[u8]) -> Option<Self> {
        let kind: [u8; 16] = raw[..16].try_into().ok()?;
        if kind == [0; 16] {
            return None;
        }
        let units: Vec<u16> = raw[56..128]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|&unit| unit != 0)
            .collect();
        Some(Self {
            kind,
            guid: raw[16..32].try_into().ok()?,
            first_lba: u64_at(raw, 32),
            last_lba: u64_at(raw, 40),
            name: String::from_utf16_lossy(&units),
        })
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// CRC-32 (IEEE 802.3, reflected), as GPT uses.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One copy of the table: its header and its entries.
#[derive(Clone)]
struct Copy {
    header: [u8; 512],
    entries: Vec<u8>,
}

impl Copy {
    fn header_lba(&self) -> u64 {
        u64_at(&self.header, 24)
    }

    fn entries_lba(&self) -> u64 {
        u64_at(&self.header, 72)
    }

    /// The header, its CRCs recomputed for `entries`.
    fn sealed(mut header: [u8; 512], entries: &[u8]) -> [u8; 512] {
        header[88..92].copy_from_slice(&crc32(entries).to_le_bytes());
        header[16..20].fill(0);
        let crc = crc32(&header[..HEADER_SIZE]);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        header
    }
}

/// Reads the copy whose header is at `lba`; `None` unless it is whole and
/// both CRCs match.
fn read_copy<D: Disk>(disk: &mut D, lba: u64) -> Result<Option<Copy>, Error> {
    let mut header = [0u8; 512];
    disk.read_at(lba * SECTOR, &mut header)?;
    if &header[..8] != SIGNATURE || u32_at(&header, 12) as usize != HEADER_SIZE {
        return Ok(None);
    }
    let mut zeroed = header;
    zeroed[16..20].fill(0);
    if crc32(&zeroed[..HEADER_SIZE]) != u32_at(&header, 16) || u64_at(&header, 24) != lba {
        return Ok(None);
    }
    if u32_at(&header, 80) as usize != ENTRIES || u32_at(&header, 84) as usize != ENTRY_SIZE {
        return Err(Error::Unsupported);
    }
    let entries_lba = u64_at(&header, 72);
    let sectors = disk.size() / SECTOR;
    if entries_lba
        .checked_add(TABLE_SECTORS)
        .is_none_or(|end| end > sectors)
    {
        return Ok(None);
    }
    let mut entries = vec![0u8; TABLE_BYTES];
    disk.read_at(entries_lba * SECTOR, &mut entries)?;
    if crc32(&entries) != u32_at(&header, 88) {
        return Ok(None);
    }
    Ok(Some(Copy { header, entries }))
}

/// Both copies as found (`None`: missing or damaged).
fn read_both<D: Disk>(disk: &mut D) -> Result<(Option<Copy>, Option<Copy>), Error> {
    let primary = read_copy(disk, 1)?;
    let last = disk.size() / SECTOR - 1;
    let backup_lba = primary
        .as_ref()
        .map_or(last, |copy| u64_at(&copy.header, 32));
    let backup = match read_copy(disk, backup_lba)? {
        Some(copy) => Some(copy),
        None if backup_lba != last => read_copy(disk, last)?,
        None => None,
    };
    Ok((primary, backup))
}

/// The partitions, entry by entry (index 0 is entry 1; `None`: unused),
/// from the primary copy, or the backup if the primary is damaged.
pub fn read<D: Disk>(disk: &mut D) -> Result<Vec<Option<Partition>>, Error> {
    let (primary, backup) = read_both(disk)?;
    let copy = primary.or(backup).ok_or(Error::NotGpt)?;
    Ok(copy
        .entries
        .as_chunks::<ENTRY_SIZE>()
        .0
        .iter()
        .map(|raw| Partition::decode(raw))
        .collect())
}

/// What [`repair`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repair {
    /// Both copies were whole and the same.
    Nothing,
    /// The primary was rewritten from the backup.
    Primary,
    /// The backup was rewritten from the primary.
    Backup,
}

/// Makes both copies whole and equal: a damaged or differing one is
/// rewritten from the other (the primary wins when both are whole; the
/// backup is only newer while [`swap`] runs, and then the primary is
/// damaged).
pub fn repair<D: Disk>(disk: &mut D) -> Result<Repair, Error> {
    let (primary, backup) = read_both(disk)?;
    match (primary, backup) {
        (None, None) => Err(Error::NotGpt),
        (Some(primary), Some(backup)) if primary.entries == backup.entries => Ok(Repair::Nothing),
        (Some(primary), backup) => {
            let last = disk.size() / SECTOR - 1;
            let lba = backup.map_or(last, |copy| copy.header_lba());
            write_copy(disk, &primary, lba, lba - TABLE_SECTORS)?;
            Ok(Repair::Backup)
        }
        (None, Some(backup)) => {
            write_copy(disk, &backup, 1, 2)?;
            Ok(Repair::Primary)
        }
    }
}

/// Writes `source`'s entries and a header for them at `header_lba`
/// (entries at `entries_lba`): entries first, header after a barrier.
fn write_copy<D: Disk>(
    disk: &mut D,
    source: &Copy,
    header_lba: u64,
    entries_lba: u64,
) -> Result<(), Error> {
    let mut header = source.header;
    // The primary names the backup, and the other way round.
    let other = if header_lba == 1 {
        source.header_lba()
    } else {
        1
    };
    header[24..32].copy_from_slice(&header_lba.to_le_bytes());
    header[32..40].copy_from_slice(&other.to_le_bytes());
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    disk.write_at(entries_lba * SECTOR, &source.entries)?;
    disk.flush()?;
    disk.write_at(header_lba * SECTOR, &Copy::sealed(header, &source.entries))?;
    disk.flush()?;
    Ok(())
}

/// Swaps entries `a` and `b` (numbered from 1, as firmware and Limine
/// number partitions; both in the same sector of the table, so the switch
/// is one sector), crash-safely (module docs). The table is repaired
/// first.
pub fn swap<D: Disk>(disk: &mut D, a: usize, b: usize) -> Result<(), Error> {
    let (ia, ib) = (a.wrapping_sub(1), b.wrapping_sub(1));
    if ia >= ENTRIES || ib >= ENTRIES || a == b {
        return Err(Error::NoSuchEntry);
    }
    let per_sector = SECTOR as usize / ENTRY_SIZE;
    if ia / per_sector != ib / per_sector {
        return Err(Error::Unsupported);
    }
    repair(disk)?;
    let (Some(primary), Some(backup)) = read_both(disk)? else {
        return Err(Error::NotGpt);
    };
    let mut entries = primary.entries.clone();
    for i in 0..ENTRY_SIZE {
        entries.swap(ia * ENTRY_SIZE + i, ib * ENTRY_SIZE + i);
    }
    let sector = ia / per_sector;
    let changed = &entries[sector * SECTOR as usize..(sector + 1) * SECTOR as usize];
    for copy in [&backup, &primary] {
        // Only the sector holding both entries changes.
        disk.write_at((copy.entries_lba() + sector as u64) * SECTOR, changed)?;
        disk.flush()?;
        let header = Copy::sealed(copy.header, &entries);
        disk.write_at(copy.header_lba() * SECTOR, &header)?;
        disk.flush()?;
    }
    Ok(())
}

/// A whole new table over a disk of `sectors`: the protective MBR, the
/// primary and the backup, `partitions` in entries 1, 2, ... As bytes to
/// write at each LBA (the caller writes them, then the volumes).
pub fn create(sectors: u64, disk_guid: [u8; 16], partitions: &[Partition]) -> Vec<(u64, Vec<u8>)> {
    let mut mbr = vec![0u8; SECTOR as usize];
    mbr[446 + 1..446 + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr[446 + 4] = 0xee;
    mbr[446 + 5..446 + 8].copy_from_slice(&[0xff, 0xff, 0xff]);
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    let covered = u32::try_from(sectors - 1).unwrap_or(u32::MAX);
    mbr[446 + 12..446 + 16].copy_from_slice(&covered.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xaa;

    let mut entries = vec![0u8; TABLE_BYTES];
    for (partition, raw) in partitions
        .iter()
        .zip(entries.as_chunks_mut::<ENTRY_SIZE>().0)
    {
        partition.encode(raw);
    }
    let last = sectors - 1;
    let backup_entries = last - TABLE_SECTORS;
    let header = |current: u64, other: u64, entries_lba: u64| {
        let mut h = [0u8; 512];
        h[..8].copy_from_slice(SIGNATURE);
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        h[24..32].copy_from_slice(&current.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + TABLE_SECTORS).to_le_bytes());
        h[48..56].copy_from_slice(&(backup_entries - 1).to_le_bytes());
        h[56..72].copy_from_slice(&disk_guid);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        Copy::sealed(h, &entries).to_vec()
    };
    let (primary, backup) = (header(1, last, 2), header(last, 1, backup_entries));
    vec![
        (0, mbr),
        (1, primary),
        (2, entries.clone()),
        (backup_entries, entries),
        (last, backup),
    ]
}

/// The first LBA a partition may use, and the last, on a disk of
/// `sectors` (what [`create`] leaves for the tables).
pub fn usable(sectors: u64) -> (u64, u64) {
    (2 + TABLE_SECTORS, sectors - 2 - TABLE_SECTORS)
}

#[cfg(test)]
mod tests;
