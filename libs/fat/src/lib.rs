//! Read-only FAT for Oceans (ADR-0036): FAT12, FAT16 and FAT32 volumes
//! with long file names (VFAT), found on a whole disk ("superfloppy"),
//! in an MBR partition or in a GPT partition.
//!
//! Disks are untrusted: every field is range-checked, cluster chains are
//! bounded by the cluster count (no loops), and a malformed structure
//! gives [`Error::Corrupt`], never a panic. Nothing is ever written.
//!
//! Nodes are numbered like `oceans-volume`'s, so the filesystem service
//! can serve either: [`ROOT`] is the root directory; [`Fat::lookup`]
//! hands out numbers, [`Fat::retain`] and [`Fat::release`] keep them.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// A failed disk read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoError;

/// Byte-addressed read access to a disk.
pub trait Disk {
    /// Fills `out` from `offset`; fails past the end or on I/O errors.
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError>;
    fn size(&self) -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io,
    /// No FAT volume on this disk.
    NotFat,
    /// The volume's structures are inconsistent.
    Corrupt,
    NotFound,
    NotADirectory,
    IsADirectory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

impl FatType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fat12 => "FAT12",
            Self::Fat16 => "FAT16",
            Self::Fat32 => "FAT32",
        }
    }
}

pub type NodeId = usize;
pub const ROOT: NodeId = 0;

/// Longest name served, in bytes (as `oceans-fs-proto`).
pub const MAX_NAME: usize = 128;

const SECTOR: u64 = 512;
const ENTRY: usize = 32;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LONG_NAME: u8 = 0x0f;
const DELETED: u8 = 0xe5;
/// Partition types that hold FAT (MBR).
const MBR_FAT: [u8; 7] = [0x01, 0x04, 0x06, 0x0b, 0x0c, 0x0e, 0xef];
const MBR_GPT: u8 = 0xee;
/// Microsoft basic data, as stored (mixed-endian GUID).
const GPT_BASIC_DATA: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];
/// EFI system partitions are FAT too.
const GPT_EFI_SYSTEM: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | u64::from(u32_at(bytes, at + 4)) << 32
}

/// Where the volume's structures are, in bytes from the start of the disk.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    kind: FatType,
    cluster_bytes: u64,
    fat: u64,
    fat_bytes: u64,
    data: u64,
    /// FAT12/16: the fixed root directory region and its entry count.
    root_region: u64,
    root_entries: u32,
    /// FAT32: the root directory's first cluster.
    root_cluster: u32,
    /// Data clusters: numbers 2 ..= clusters + 1 exist.
    clusters: u32,
    label: [u8; 11],
}

/// Parses a FAT boot sector at `start` (bytes into the disk).
fn geometry(sector: &[u8; 512], start: u64, disk_size: u64) -> Option<Geometry> {
    if sector[510] != 0x55 || sector[511] != 0xaa || !matches!(sector[0], 0xeb | 0xe9) {
        return None;
    }
    let bytes_per_sector = u64::from(u16_at(sector, 11));
    let sectors_per_cluster = u64::from(sector[13]);
    let reserved = u64::from(u16_at(sector, 14));
    let fats = u64::from(sector[16]);
    let root_entries = u32::from(u16_at(sector, 17));
    let total = match u16_at(sector, 19) {
        0 => u64::from(u32_at(sector, 32)),
        small => u64::from(small),
    };
    let fat_sectors = match u16_at(sector, 22) {
        0 => u64::from(u32_at(sector, 36)),
        small => u64::from(small),
    };
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
        || !sectors_per_cluster.is_power_of_two()
        || sectors_per_cluster > 128
        || reserved == 0
        || !(1..=2).contains(&fats)
        || fat_sectors == 0
        || total == 0
    {
        return None;
    }
    let root_sectors = (u64::from(root_entries) * ENTRY as u64).div_ceil(bytes_per_sector);
    let meta = reserved + fats * fat_sectors + root_sectors;
    if meta >= total || start + total * bytes_per_sector > disk_size {
        return None;
    }
    let clusters = (total - meta) / sectors_per_cluster;
    // The FAT type follows from the cluster count alone (FAT spec §3.5).
    let kind = if clusters < 4085 {
        FatType::Fat12
    } else if clusters < 65525 {
        FatType::Fat16
    } else {
        FatType::Fat32
    };
    let clusters = u32::try_from(clusters).ok()?;
    let entry_bits = match kind {
        FatType::Fat12 => 12,
        FatType::Fat16 => 16,
        FatType::Fat32 => 32,
    };
    let fat_bytes = fat_sectors * bytes_per_sector;
    // The FAT must have an entry for every cluster.
    if (u64::from(clusters) + 2) * entry_bits / 8 > fat_bytes {
        return None;
    }
    let (root_cluster, label_at) = match kind {
        FatType::Fat32 => {
            if root_entries != 0 {
                return None;
            }
            (u32_at(sector, 44), 71)
        }
        _ => {
            if root_entries == 0 {
                return None;
            }
            (0, 43)
        }
    };
    let mut label = [b' '; 11];
    label.copy_from_slice(&sector[label_at..label_at + 11]);
    Some(Geometry {
        kind,
        cluster_bytes: sectors_per_cluster * bytes_per_sector,
        fat: start + reserved * bytes_per_sector,
        fat_bytes,
        data: start + meta * bytes_per_sector,
        root_region: start + (reserved + fats * fat_sectors) * bytes_per_sector,
        root_entries,
        root_cluster,
        clusters,
        label,
    })
}

/// The byte offset of a FAT volume: the whole disk, an MBR partition of a
/// FAT type, or a GPT basic data (or EFI system) partition.
fn find_volume<D: Disk>(disk: &mut D) -> Result<Geometry, Error> {
    let size = disk.size();
    if size < 2 * SECTOR {
        return Err(Error::NotFat);
    }
    let mut sector = [0u8; 512];
    disk.read_at(0, &mut sector).map_err(|IoError| Error::Io)?;
    if let Some(found) = geometry(&sector, 0, size) {
        return Ok(found);
    }
    if sector[510] != 0x55 || sector[511] != 0xaa {
        return Err(Error::NotFat);
    }
    let entries: Vec<(u8, u64)> = (0..4)
        .map(|i| {
            let at = 446 + i * 16;
            (sector[at + 4], u64::from(u32_at(&sector, at + 8)))
        })
        .collect();
    let mut starts = Vec::new();
    if entries.iter().any(|&(kind, _)| kind == MBR_GPT) {
        let mut header = [0u8; 512];
        disk.read_at(SECTOR, &mut header)
            .map_err(|IoError| Error::Io)?;
        if &header[..8] != b"EFI PART" {
            return Err(Error::NotFat);
        }
        let table = u64_at(&header, 72);
        let count = u32_at(&header, 80).min(128);
        let entry_size = u64::from(u32_at(&header, 84));
        if !(128..=1024).contains(&entry_size) {
            return Err(Error::Corrupt);
        }
        for i in 0..u64::from(count) {
            let mut entry = [0u8; 128];
            let at = table
                .checked_mul(SECTOR)
                .and_then(|t| t.checked_add(i * entry_size))
                .ok_or(Error::Corrupt)?;
            disk.read_at(at, &mut entry).map_err(|IoError| Error::Io)?;
            let kind: [u8; 16] = entry[..16].try_into().expect("16 bytes");
            if kind == GPT_BASIC_DATA || kind == GPT_EFI_SYSTEM {
                starts.push(u64_at(&entry, 32));
            }
        }
    } else {
        starts.extend(
            entries
                .iter()
                .filter(|(kind, start)| MBR_FAT.contains(kind) && *start != 0)
                .map(|&(_, start)| start),
        );
    }
    for start in starts {
        let Some(offset) = start.checked_mul(SECTOR) else {
            continue;
        };
        if disk.read_at(offset, &mut sector).is_ok()
            && let Some(found) = geometry(&sector, offset, size)
        {
            return Ok(found);
        }
    }
    Err(Error::NotFat)
}

#[derive(Clone, Copy, Debug)]
struct Node {
    first: u32,
    size: u32,
    directory: bool,
    refs: u32,
    /// The last cluster reached while reading, by index in the chain.
    cursor: (u32, u32),
}

/// One directory entry, decoded.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    short: String,
    first: u32,
    size: u32,
    directory: bool,
}

/// A read-only FAT volume.
pub struct Fat<D> {
    disk: D,
    g: Geometry,
    nodes: Vec<Option<Node>>,
    /// The name `entry` last returned.
    name: String,
    /// One FAT sector, cached.
    fat_cache: Option<(u64, [u8; 512])>,
}

impl<D: Disk> Fat<D> {
    /// Finds and checks the volume; reads nothing else.
    pub fn open(mut disk: D) -> Result<Self, Error> {
        let g = find_volume(&mut disk)?;
        let root = Node {
            first: g.root_cluster,
            size: 0,
            directory: true,
            refs: 1,
            cursor: (0, g.root_cluster),
        };
        if g.kind == FatType::Fat32 && !(2..g.clusters + 2).contains(&g.root_cluster) {
            return Err(Error::Corrupt);
        }
        Ok(Self {
            disk,
            g,
            nodes: vec![Some(root)],
            name: String::new(),
            fat_cache: None,
        })
    }

    pub fn kind(&self) -> FatType {
        self.g.kind
    }

    /// The volume label from the boot sector, trimmed.
    pub fn label(&self) -> &str {
        core::str::from_utf8(&self.g.label).unwrap_or("").trim_end()
    }

    /// Total size of the data area, in bytes.
    pub fn capacity(&self) -> u64 {
        u64::from(self.g.clusters) * self.g.cluster_bytes
    }

    pub fn disk(&self) -> &D {
        &self.disk
    }

    // ---- Clusters ----------------------------------------------------------

    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        self.disk.read_at(offset, out).map_err(|IoError| Error::Io)
    }

    fn fat_byte(&mut self, offset: u64) -> Result<u8, Error> {
        if offset >= self.g.fat_bytes {
            return Err(Error::Corrupt);
        }
        let at = self.g.fat + offset;
        let sector = at / SECTOR * SECTOR;
        if self.fat_cache.is_none_or(|(cached, _)| cached != sector) {
            let mut bytes = [0u8; 512];
            self.read(sector, &mut bytes)?;
            self.fat_cache = Some((sector, bytes));
        }
        Ok(self.fat_cache.expect("filled").1[(at - sector) as usize])
    }

    /// The cluster after `cluster`, or `None` at the end of the chain.
    fn next(&mut self, cluster: u32) -> Result<Option<u32>, Error> {
        let value = match self.g.kind {
            FatType::Fat12 => {
                let at = u64::from(cluster) * 3 / 2;
                let pair = u16::from(self.fat_byte(at)?) | u16::from(self.fat_byte(at + 1)?) << 8;
                let value = if cluster & 1 == 0 {
                    pair & 0xfff
                } else {
                    pair >> 4
                };
                if value >= 0xff8 {
                    return Ok(None);
                }
                u32::from(value)
            }
            FatType::Fat16 => {
                let at = u64::from(cluster) * 2;
                let value = u16::from(self.fat_byte(at)?) | u16::from(self.fat_byte(at + 1)?) << 8;
                if value >= 0xfff8 {
                    return Ok(None);
                }
                u32::from(value)
            }
            FatType::Fat32 => {
                let at = u64::from(cluster) * 4;
                let mut value = 0u32;
                for i in 0..4 {
                    value |= u32::from(self.fat_byte(at + i)?) << (8 * i);
                }
                let value = value & 0x0fff_ffff;
                if value >= 0x0fff_fff8 {
                    return Ok(None);
                }
                value
            }
        };
        // Free, reserved, bad or out-of-range links are corruption.
        if !(2..self.g.clusters + 2).contains(&value) {
            return Err(Error::Corrupt);
        }
        Ok(Some(value))
    }

    fn cluster_offset(&self, cluster: u32) -> Result<u64, Error> {
        if !(2..self.g.clusters + 2).contains(&cluster) {
            return Err(Error::Corrupt);
        }
        Ok(self.g.data + u64::from(cluster - 2) * self.g.cluster_bytes)
    }

    // ---- Directories ---------------------------------------------------------

    /// Calls `visit` with each live entry of directory `first` (0: the
    /// FAT12/16 root region) until it returns `true`.
    fn walk(&mut self, first: u32, mut visit: impl FnMut(Entry) -> bool) -> Result<(), Error> {
        let fixed_root = first == 0 && self.g.kind != FatType::Fat32;
        let mut long = LongName::new();
        let mut cluster = first;
        let mut steps = 0u32;
        loop {
            let (base, count) = if fixed_root {
                (self.g.root_region, self.g.root_entries as usize)
            } else {
                (
                    self.cluster_offset(cluster)?,
                    self.g.cluster_bytes as usize / ENTRY,
                )
            };
            let mut raw = [0u8; ENTRY];
            for i in 0..count {
                self.read(base + (i * ENTRY) as u64, &mut raw)?;
                match raw[0] {
                    0 => return Ok(()),
                    DELETED => {
                        long.clear();
                        continue;
                    }
                    _ => {}
                }
                let attributes = raw[11];
                if attributes & 0x3f == ATTR_LONG_NAME {
                    long.add(&raw);
                    continue;
                }
                let short = short_name(&raw);
                let name = long.take(checksum(&raw)).unwrap_or_else(|| short.clone());
                if attributes & ATTR_VOLUME != 0 || short == "." || short == ".." {
                    continue;
                }
                let first = u32::from(u16_at(&raw, 20)) << 16 | u32::from(u16_at(&raw, 26));
                let entry = Entry {
                    name,
                    short,
                    first: if self.g.kind == FatType::Fat32 {
                        first
                    } else {
                        first & 0xffff
                    },
                    size: u32_at(&raw, 28),
                    directory: attributes & ATTR_DIRECTORY != 0,
                };
                if visit(entry) {
                    return Ok(());
                }
            }
            if fixed_root {
                return Ok(());
            }
            steps += 1;
            if steps > self.g.clusters {
                return Err(Error::Corrupt);
            }
            match self.next(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
        }
    }

    fn node(&self, id: NodeId) -> Result<Node, Error> {
        self.nodes.get(id).copied().flatten().ok_or(Error::NotFound)
    }

    fn directory(&self, id: NodeId) -> Result<u32, Error> {
        let node = self.node(id)?;
        if !node.directory {
            return Err(Error::NotADirectory);
        }
        Ok(node.first)
    }

    /// The child `name` of directory `dir`, by its long or short name,
    /// ignoring ASCII case as FAT does.
    pub fn lookup(&mut self, dir: NodeId, name: &str) -> Result<NodeId, Error> {
        let first = self.directory(dir)?;
        let mut found = None;
        self.walk(first, |entry| {
            let matched =
                entry.name.eq_ignore_ascii_case(name) || entry.short.eq_ignore_ascii_case(name);
            if matched {
                found = Some(entry);
            }
            matched
        })?;
        let entry = found.ok_or(Error::NotFound)?;
        if entry.directory && entry.first == 0 {
            // ".." of a first-level directory; never a real child.
            return Err(Error::Corrupt);
        }
        Ok(self.insert(Node {
            first: entry.first,
            size: if entry.directory { 0 } else { entry.size },
            directory: entry.directory,
            refs: 0,
            cursor: (0, entry.first),
        }))
    }

    /// A node for `node`: an existing one with the same contents, or a
    /// slot no handle holds.
    fn insert(&mut self, node: Node) -> NodeId {
        if node.first != 0
            && let Some(id) = self.nodes.iter().position(|n| {
                n.is_some_and(|n| {
                    n.first == node.first && n.directory == node.directory && n.size == node.size
                })
            })
        {
            return id;
        }
        let free = self
            .nodes
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, n)| n.is_none_or(|n| n.refs == 0))
            .map(|(id, _)| id);
        match free {
            Some(id) => {
                self.nodes[id] = Some(node);
                id
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        }
    }

    /// The `index`th entry of directory `dir`: its name and whether it is
    /// a directory. `NotFound` past the end.
    pub fn entry(&mut self, dir: NodeId, index: usize) -> Result<(&str, bool), Error> {
        let first = self.directory(dir)?;
        let mut seen = 0;
        let mut found = None;
        self.walk(first, |entry| {
            if seen == index {
                found = Some(entry);
                return true;
            }
            seen += 1;
            false
        })?;
        let entry = found.ok_or(Error::NotFound)?;
        self.name = entry.name;
        Ok((&self.name, entry.directory))
    }

    pub fn is_directory(&self, id: NodeId) -> Result<bool, Error> {
        Ok(self.node(id)?.directory)
    }

    pub fn size(&self, id: NodeId) -> Result<u64, Error> {
        Ok(u64::from(self.node(id)?.size))
    }

    pub fn retain(&mut self, id: NodeId) -> Result<(), Error> {
        let node = self
            .nodes
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(Error::NotFound)?;
        node.refs += 1;
        Ok(())
    }

    pub fn release(&mut self, id: NodeId) {
        if id != ROOT
            && let Some(Some(node)) = self.nodes.get_mut(id)
        {
            node.refs = node.refs.saturating_sub(1);
        }
    }

    /// Reads file contents at `offset`; returns the bytes read (0 at the
    /// end).
    pub fn read_file(&mut self, id: NodeId, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let node = self.node(id)?;
        if node.directory {
            return Err(Error::IsADirectory);
        }
        let size = u64::from(node.size);
        if offset >= size || out.is_empty() {
            return Ok(0);
        }
        let len = out.len().min((size - offset) as usize);
        let cluster_bytes = self.g.cluster_bytes;
        let mut done = 0;
        while done < len {
            let at = offset + done as u64;
            let index = u32::try_from(at / cluster_bytes).map_err(|_| Error::Corrupt)?;
            let cluster = self.cluster_at(id, index)?;
            let within = at % cluster_bytes;
            let take = ((cluster_bytes - within) as usize).min(len - done);
            let base = self.cluster_offset(cluster)?;
            self.read(base + within, &mut out[done..done + take])?;
            done += take;
        }
        Ok(len)
    }

    /// The `index`th cluster of a file, continuing from where the last
    /// read stopped when it can.
    fn cluster_at(&mut self, id: NodeId, index: u32) -> Result<u32, Error> {
        let node = self.node(id)?;
        let (mut at, mut cluster) = if node.cursor.0 <= index {
            node.cursor
        } else {
            (0, node.first)
        };
        if index > self.g.clusters {
            return Err(Error::Corrupt);
        }
        while at < index {
            cluster = self.next(cluster)?.ok_or(Error::Corrupt)?;
            at += 1;
        }
        if let Some(Some(node)) = self.nodes.get_mut(id) {
            node.cursor = (at, cluster);
        }
        Ok(cluster)
    }
}

/// The 8.3 name of an entry, lowercased where the entry says (NT flags).
fn short_name(raw: &[u8; ENTRY]) -> String {
    let lower_base = raw[12] & 0x08 != 0;
    let lower_extension = raw[12] & 0x10 != 0;
    let mut name = String::new();
    let decode = |byte: u8, lower: bool| -> char {
        let byte = if byte == 0x05 { DELETED } else { byte };
        match byte {
            0x20..=0x7e if lower => char::from(byte.to_ascii_lowercase()),
            0x20..=0x7e => char::from(byte),
            // OEM code page characters are not translated.
            _ => '_',
        }
    };
    for &byte in raw[..8].iter().take_while(|&&b| b != b' ') {
        name.push(decode(byte, lower_base));
    }
    let extension: Vec<u8> = raw[8..11]
        .iter()
        .copied()
        .take_while(|&b| b != b' ')
        .collect();
    if !extension.is_empty() {
        name.push('.');
        for byte in extension {
            name.push(decode(byte, lower_extension));
        }
    }
    name
}

/// The checksum long-name entries carry of their short name.
fn checksum(raw: &[u8; ENTRY]) -> u8 {
    raw[..11]
        .iter()
        .fold(0u8, |sum, &byte| sum.rotate_right(1).wrapping_add(byte))
}

/// Long-name entries collected before their short entry.
struct LongName {
    units: [u16; 20 * 13],
    /// Entries seen, as a bit set by sequence number.
    seen: u32,
    count: u8,
    checksum: u8,
}

impl LongName {
    fn new() -> Self {
        Self {
            units: [0xffff; 20 * 13],
            seen: 0,
            count: 0,
            checksum: 0,
        }
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    fn add(&mut self, raw: &[u8; ENTRY]) {
        let order = raw[0];
        let sequence = order & 0x1f;
        if !(1..=20).contains(&sequence) {
            self.clear();
            return;
        }
        if order & 0x40 != 0 {
            // The last entry of a name comes first.
            self.clear();
            self.count = sequence;
            self.checksum = raw[13];
        } else if self.count == 0 || raw[13] != self.checksum {
            self.clear();
            return;
        }
        let at = usize::from(sequence - 1) * 13;
        let pieces = [(1, 5), (14, 6), (28, 2)];
        let mut unit = at;
        for (start, units) in pieces {
            for i in 0..units {
                self.units[unit] = u16_at(raw, start + 2 * i);
                unit += 1;
            }
        }
        self.seen |= 1 << sequence;
    }

    /// The name, if every entry arrived and they belong to this short
    /// entry; clears the collection.
    fn take(&mut self, short_checksum: u8) -> Option<String> {
        let complete = self.count > 0
            && self.checksum == short_checksum
            && (1..=self.count).all(|sequence| self.seen & (1 << sequence) != 0);
        let units = &self.units[..usize::from(self.count) * 13];
        let result = complete.then(|| {
            let end = units
                .iter()
                .position(|&u| u == 0 || u == 0xffff)
                .unwrap_or(units.len());
            char::decode_utf16(units[..end].iter().copied())
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect::<String>()
        });
        self.clear();
        // Names the file service cannot carry fall back to the short name.
        result.filter(|name| {
            !name.is_empty()
                && name.len() <= MAX_NAME
                && !name.contains(['/', '\0'])
                && name != "."
                && name != ".."
        })
    }
}
