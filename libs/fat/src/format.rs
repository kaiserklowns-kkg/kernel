//! Making FAT32 volumes (ADR-0092): what `mkfs.fat -F 32` makes, so that
//! images are built without dosfstools, and a partition's volume can be
//! remade on a running system.

use alloc::vec;

use crate::{Disk, Error, IoError, SECTOR};

/// Reserved sectors before the first FAT (boot sector, FSInfo, backup).
const RESERVED: u32 = 32;
const FSINFO_SECTOR: u16 = 1;
const BACKUP_SECTOR: u16 = 6;
const FATS: u32 = 2;
/// FAT32 needs at least this many clusters (FAT spec §3.5).
const MIN_CLUSTERS: u64 = 65525;
/// The largest volume made: what 32 KiB clusters address.
const MAX_SECTORS: u64 = 0xffff_ffff;

/// Bytes per cluster for a volume of `sectors`, as Microsoft's table
/// (and dosfstools) choose them; `None` if FAT32 cannot hold it.
fn sectors_per_cluster(sectors: u64) -> Option<u32> {
    let per_cluster: u32 = match sectors {
        s if s < 66_600 => return None,
        s if s <= 532_480 => 1,
        s if s <= 16_777_216 => 8,
        s if s <= 33_554_432 => 16,
        s if s <= 67_108_864 => 32,
        s if s <= MAX_SECTORS => 64,
        _ => return None,
    };
    Some(per_cluster)
}

/// A volume label as stored: upper case, padded with spaces.
fn stored_label(label: &str) -> [u8; 11] {
    let mut stored = [b' '; 11];
    for (slot, byte) in stored.iter_mut().zip(label.bytes()) {
        *slot = if byte.is_ascii_graphic() || byte == b' ' {
            byte.to_ascii_uppercase()
        } else {
            b'_'
        };
    }
    stored
}

/// Writes an empty FAT32 volume over the whole of `disk`, labelled
/// `label` (up to 11 ASCII characters), with the volume serial `serial`.
/// Fails if the disk is too small for FAT32 (about 33 MiB) or too large.
pub fn format<D: Disk>(disk: &mut D, label: &str, serial: u32) -> Result<(), Error> {
    let sectors = disk.size() / SECTOR;
    let per_cluster = sectors_per_cluster(sectors).ok_or(Error::NoSpace)?;
    // FAT spec §3.5: the FAT size that fits the clusters it describes.
    let fat_sectors = {
        let data = sectors - u64::from(RESERVED);
        let per_fat_sector = (256 * u64::from(per_cluster) + u64::from(FATS)) / 2;
        data.div_ceil(per_fat_sector)
    };
    let meta = u64::from(RESERVED) + u64::from(FATS) * fat_sectors;
    let clusters = (sectors - meta) / u64::from(per_cluster);
    if clusters < MIN_CLUSTERS {
        return Err(Error::NoSpace);
    }
    let fat_sectors = u32::try_from(fat_sectors).map_err(|_| Error::NoSpace)?;
    let total = u32::try_from(sectors).map_err(|_| Error::NoSpace)?;
    let label = stored_label(label);

    let mut boot = [0u8; 512];
    boot[..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"OCEANS  ");
    boot[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    boot[13] = per_cluster as u8;
    boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    boot[16] = FATS as u8;
    // Root entries and the 16-bit sizes stay 0 on FAT32.
    boot[21] = 0xf8;
    boot[24..26].copy_from_slice(&32u16.to_le_bytes());
    boot[26..28].copy_from_slice(&64u16.to_le_bytes());
    boot[32..36].copy_from_slice(&total.to_le_bytes());
    boot[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
    // Flags 0: both FATs mirrored. Version 0.0.
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48..50].copy_from_slice(&FSINFO_SECTOR.to_le_bytes());
    boot[50..52].copy_from_slice(&BACKUP_SECTOR.to_le_bytes());
    boot[64] = 0x80;
    boot[66] = 0x29;
    boot[67..71].copy_from_slice(&serial.to_le_bytes());
    boot[71..82].copy_from_slice(&label);
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xaa;

    let free = u32::try_from(clusters - 1).map_err(|_| Error::NoSpace)?;
    let mut fsinfo = [0u8; 512];
    fsinfo[..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    fsinfo[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    fsinfo[488..492].copy_from_slice(&free.to_le_bytes());
    fsinfo[492..496].copy_from_slice(&3u32.to_le_bytes());
    fsinfo[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes());

    // The reserved area, both FATs and the root directory's cluster, all
    // zero first; then the structures that make it a volume, the boot
    // sector last.
    let zeros = vec![0u8; 64 * 1024];
    let cluster_bytes = u64::from(per_cluster) * SECTOR;
    let end = meta * SECTOR + cluster_bytes;
    let mut at = 0;
    while at < end {
        let len = (end - at).min(zeros.len() as u64) as usize;
        disk.write_at(at, &zeros[..len])
            .map_err(|IoError| Error::Io)?;
        at += len as u64;
    }
    let mut first_fat = [0u8; 512];
    first_fat[..4].copy_from_slice(&0x0fff_fff8u32.to_le_bytes());
    first_fat[4..8].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    // The root directory: cluster 2, one cluster long.
    first_fat[8..12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    for copy in 0..u64::from(FATS) {
        let fat = (u64::from(RESERVED) + copy * u64::from(fat_sectors)) * SECTOR;
        disk.write_at(fat, &first_fat)
            .map_err(|IoError| Error::Io)?;
    }
    let mut root = [0u8; 512];
    root[..11].copy_from_slice(&label);
    root[11] = crate::ATTR_VOLUME;
    disk.write_at(meta * SECTOR, &root)
        .map_err(|IoError| Error::Io)?;
    for (sector, bytes) in [
        (u64::from(FSINFO_SECTOR), &fsinfo),
        (u64::from(BACKUP_SECTOR) + u64::from(FSINFO_SECTOR), &fsinfo),
        (u64::from(BACKUP_SECTOR), &boot),
    ] {
        disk.write_at(sector * SECTOR, bytes)
            .map_err(|IoError| Error::Io)?;
    }
    disk.flush().map_err(|IoError| Error::Io)?;
    disk.write_at(0, &boot).map_err(|IoError| Error::Io)?;
    disk.flush().map_err(|IoError| Error::Io)
}

/// Part of a disk, `[start, start + size)` in bytes, as a disk of its own:
/// one partition's volume.
pub struct Window<D> {
    disk: D,
    start: u64,
    size: u64,
}

impl<D: Disk> Window<D> {
    /// `None` if the range is not whole sectors inside the disk.
    pub fn new(disk: D, start: u64, size: u64) -> Option<Self> {
        let fits = start
            .checked_add(size)
            .is_some_and(|end| end <= disk.size());
        (fits && start.is_multiple_of(SECTOR) && size.is_multiple_of(SECTOR)).then_some(Self {
            disk,
            start,
            size,
        })
    }

    pub fn into_inner(self) -> D {
        self.disk
    }

    fn inside(&self, offset: u64, len: usize) -> Result<u64, IoError> {
        offset
            .checked_add(len as u64)
            .filter(|&end| end <= self.size)
            .map(|_| self.start + offset)
            .ok_or(IoError)
    }
}

impl<D: Disk> Disk for Window<D> {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        let at = self.inside(offset, out.len())?;
        self.disk.read_at(at, out)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        let at = self.inside(offset, data.len())?;
        self.disk.write_at(at, data)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.disk.flush()
    }

    fn writable(&self) -> bool {
        self.disk.writable()
    }

    fn size(&self) -> u64 {
        self.size
    }
}
