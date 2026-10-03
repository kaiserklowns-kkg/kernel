//! What the service serves: an Oceans volume, or (removable media only)
//! a read-only FAT volume (ADR-0036). Both number nodes the same way, so
//! the request handlers do not care which.

use oceans_block_proto::{Disk, SECTOR_SIZE};
use oceans_fat::Fat;
use oceans_volume::{BLOCK_SIZE, FsError, Kind, NodeId, Volume};

use crate::DiskDevice;

/// A FAT volume's disk: byte reads through a block session with a
/// one-block buffer.
pub struct FatDisk {
    pub disk: Disk,
    pub sectors: u64,
}

impl oceans_fat::Disk for FatDisk {
    fn read_at(&mut self, mut offset: u64, out: &mut [u8]) -> Result<(), oceans_fat::IoError> {
        let per_read = (BLOCK_SIZE / SECTOR_SIZE) as u64;
        let mut done = 0;
        while done < out.len() {
            let sector = offset / SECTOR_SIZE as u64;
            if sector >= self.sectors {
                return Err(oceans_fat::IoError);
            }
            let count = per_read.min(self.sectors - sector);
            self.disk
                .read(sector, count as u32, 0)
                .map_err(|_| oceans_fat::IoError)?;
            let skip = (offset % SECTOR_SIZE as u64) as usize;
            let available = count as usize * SECTOR_SIZE - skip;
            let take = available.min(out.len() - done);
            out[done..done + take].copy_from_slice(&self.disk.buffer()[skip..skip + take]);
            done += take;
            offset += take as u64;
        }
        Ok(())
    }

    fn size(&self) -> u64 {
        self.sectors * SECTOR_SIZE as u64
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "one store per service, replaced only on (un)mount"
)]
pub enum Store {
    Oceans(Volume<DiskDevice>),
    Fat(Fat<FatDisk>),
}

fn fat_error(error: oceans_fat::Error) -> FsError {
    match error {
        oceans_fat::Error::Io => FsError::Io,
        oceans_fat::Error::NotFat | oceans_fat::Error::Corrupt => FsError::Corrupt,
        oceans_fat::Error::NotFound => FsError::NotFound,
        oceans_fat::Error::NotADirectory => FsError::NotADirectory,
        oceans_fat::Error::IsADirectory => FsError::IsADirectory,
    }
}

fn fat_kind(directory: bool) -> Kind {
    if directory {
        Kind::Directory
    } else {
        Kind::File
    }
}

impl Store {
    pub fn memory() -> Self {
        Self::Oceans(Volume::memory())
    }

    /// The Oceans volume, for what only it supports (`/bin`, usage).
    pub fn oceans(&mut self) -> Option<&mut Volume<DiskDevice>> {
        match self {
            Self::Oceans(volume) => Some(volume),
            Self::Fat(_) => None,
        }
    }

    /// Whether the disk behind the store still answers.
    pub fn alive(&self) -> bool {
        match self {
            Self::Oceans(volume) => volume.device().is_some_and(|d| d.disk.alive()),
            Self::Fat(fat) => fat.disk().disk.alive(),
        }
    }

    pub fn lookup(&mut self, dir: NodeId, name: &str) -> Result<NodeId, FsError> {
        match self {
            Self::Oceans(volume) => volume.lookup(dir, name),
            Self::Fat(fat) => fat.lookup(dir, name).map_err(fat_error),
        }
    }

    pub fn entry(&mut self, dir: NodeId, index: usize) -> Result<(&str, Kind), FsError> {
        match self {
            Self::Oceans(volume) => volume.entry(dir, index),
            Self::Fat(fat) => fat
                .entry(dir, index)
                .map(|(name, directory)| (name, fat_kind(directory)))
                .map_err(fat_error),
        }
    }

    pub fn kind(&self, id: NodeId) -> Result<Kind, FsError> {
        match self {
            Self::Oceans(volume) => volume.kind(id),
            Self::Fat(fat) => fat.is_directory(id).map(fat_kind).map_err(fat_error),
        }
    }

    pub fn size(&self, id: NodeId) -> Result<u64, FsError> {
        match self {
            Self::Oceans(volume) => volume.size(id),
            Self::Fat(fat) => fat.size(id).map_err(fat_error),
        }
    }

    pub fn is_read_only(&self, id: NodeId) -> Result<bool, FsError> {
        match self {
            Self::Oceans(volume) => volume.is_read_only(id),
            Self::Fat(_) => Ok(true),
        }
    }

    pub fn retain(&mut self, id: NodeId) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.retain(id),
            Self::Fat(fat) => fat.retain(id).map_err(fat_error),
        }
    }

    pub fn release(&mut self, id: NodeId) {
        match self {
            Self::Oceans(volume) => volume.release(id),
            Self::Fat(fat) => fat.release(id),
        }
    }

    pub fn read(&mut self, id: NodeId, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        match self {
            Self::Oceans(volume) => volume.read(id, offset, out),
            Self::Fat(fat) => fat.read_file(id, offset, out).map_err(fat_error),
        }
    }

    pub fn write(&mut self, id: NodeId, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        match self {
            Self::Oceans(volume) => volume.write(id, offset, data),
            Self::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    pub fn create(&mut self, dir: NodeId, name: &str, kind: Kind) -> Result<NodeId, FsError> {
        match self {
            Self::Oceans(volume) => volume.create(dir, name, kind),
            Self::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    pub fn remove(&mut self, dir: NodeId, name: &str) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.remove(dir, name),
            Self::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    pub fn truncate(&mut self, id: NodeId, size: u64) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.truncate(id, size),
            Self::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    pub fn commit(&mut self) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.commit(),
            // Nothing is ever changed.
            Self::Fat(_) => Ok(()),
        }
    }
}
