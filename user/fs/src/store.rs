//! What the service serves: an Oceans volume, or (removable media only)
//! a FAT volume (ADR-0036, written crash-safely from ADR-0037). Both
//! number nodes the same way, so the request handlers do not care which.

use oceans_block_proto::{Disk, SECTOR_SIZE};
use oceans_fat::Fat;
use oceans_volume::{BLOCK_SIZE, FsError, Kind, NodeId, Volume};

use crate::DiskDevice;

/// A FAT volume's disk: byte access through a block session with a
/// one-block buffer.
pub struct FatDisk {
    pub disk: Disk,
    pub sectors: u64,
    pub read_only: bool,
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

    /// Whole sectors only (the FAT library writes nothing smaller).
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), oceans_fat::IoError> {
        let sector_size = SECTOR_SIZE as u64;
        if !offset.is_multiple_of(sector_size) || !(data.len() as u64).is_multiple_of(sector_size) {
            return Err(oceans_fat::IoError);
        }
        let first = offset / sector_size;
        if first + data.len() as u64 / sector_size > self.sectors {
            return Err(oceans_fat::IoError);
        }
        for (i, chunk) in data.chunks(BLOCK_SIZE).enumerate() {
            let sector = first + (i * BLOCK_SIZE / SECTOR_SIZE) as u64;
            self.disk.buffer()[..chunk.len()].copy_from_slice(chunk);
            self.disk
                .write(sector, (chunk.len() / SECTOR_SIZE) as u32, 0)
                .map_err(|_| oceans_fat::IoError)?;
        }
        Ok(())
    }

    /// The barrier: the stick's cache flushed (SYNCHRONIZE CACHE).
    fn flush(&mut self) -> Result<(), oceans_fat::IoError> {
        self.disk.flush().map_err(|_| oceans_fat::IoError)
    }

    fn writable(&self) -> bool {
        !self.read_only
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
        oceans_fat::Error::Exists => FsError::Exists,
        oceans_fat::Error::InvalidName => FsError::InvalidName,
        oceans_fat::Error::NoSpace => FsError::NoSpace,
        oceans_fat::Error::NotEmpty => FsError::NotEmpty,
        oceans_fat::Error::ReadOnly => FsError::ReadOnly,
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
            Self::Fat(fat) => Ok(!fat.writable()),
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
            Self::Fat(fat) => fat.write_file(id, offset, data).map_err(fat_error),
        }
    }

    pub fn create(&mut self, dir: NodeId, name: &str, kind: Kind) -> Result<NodeId, FsError> {
        match self {
            Self::Oceans(volume) => volume.create(dir, name, kind),
            Self::Fat(fat) => fat
                .create(dir, name, kind == Kind::Directory)
                .map_err(fat_error),
        }
    }

    pub fn remove(&mut self, dir: NodeId, name: &str) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.remove(dir, name),
            Self::Fat(fat) => fat.remove(dir, name).map_err(fat_error),
        }
    }

    pub fn rename(
        &mut self,
        dir: NodeId,
        name: &str,
        new_dir: NodeId,
        new_name: &str,
    ) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.rename(dir, name, new_dir, new_name),
            Self::Fat(fat) => fat.rename(dir, name, new_dir, new_name).map_err(fat_error),
        }
    }

    pub fn truncate(&mut self, id: NodeId, size: u64) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.truncate(id, size),
            Self::Fat(fat) => fat.truncate(id, size).map_err(fat_error),
        }
    }

    pub fn commit(&mut self) -> Result<(), FsError> {
        match self {
            Self::Oceans(volume) => volume.commit(),
            // Everything is already written in order; this makes it
            // durable and marks the volume clean.
            Self::Fat(fat) => fat.sync().map_err(fat_error),
        }
    }
}
