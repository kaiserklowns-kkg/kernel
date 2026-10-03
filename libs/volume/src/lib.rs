//! OceansFS (ADR-0022): the on-disk volume behind the filesystem service.
//!
//! # Format
//!
//! The disk is an array of 4 KiB blocks.
//!
//! - Blocks 0 and 1 are **superblocks**, written alternately: generation
//!   *g* goes to block *g* mod 2. Each holds the generation, the volume
//!   size, the metadata's length, CRC32C and block list, and its own
//!   CRC32C.
//! - **Metadata** is the whole directory tree, serialized in preorder:
//!   - per node: kind, name, then
//!     - for a file: size and block list, each block with the CRC-32C of
//!       its contents (format 2, ADR-0027; block 0 = a hole that reads as
//!       zeros);
//!     - for a directory: child count.
//! - Every data block read from the disk is checked against its CRC, so
//!   silent corruption is reported (`Corrupt`) instead of returned as
//!   data. Format 1 volumes (no data CRCs) still mount: their CRCs are
//!   computed at mount and the volume is written as format 2 at the next
//!   commit.
//! - Everything else is data blocks. There is no allocation bitmap: the
//!   free blocks are whatever the metadata does not reference, recomputed
//!   at mount.
//!
//! # Crash consistency
//!
//! Copy-on-write: a block referenced by the last durable generation is
//! never overwritten. File writes go to newly allocated blocks, and blocks
//! the durable state still references are freed only once a newer
//! generation is durable. A **commit** works in this order:
//!
//! 1. write the metadata to free blocks;
//! 2. flush;
//! 3. write the next superblock;
//! 4. flush.
//!
//! A power cut at any point leaves either the previous generation or the
//! new one, never a mix. A torn superblock fails its checksum, and the
//! other slot holds the previous generation, still intact.
//!
//! # Volatile nodes
//!
//! Nodes marked volatile (the read-only `/bin` published at every boot)
//! live only in memory and are never written. Without a device the whole
//! volume is volatile (an in-memory filesystem).
//!
//! Disk contents are untrusted input. Mount validates every field, block
//! number, name and count, and refuses a volume it cannot fully verify
//! instead of guessing.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const BLOCK_SIZE: usize = 4096;
pub type BlockBuf = [u8; BLOCK_SIZE];

/// Longest entry name, in bytes.
pub const MAX_NAME: usize = 128;
/// Most nodes (files and directories) in a volume.
pub const MAX_NODES: usize = 4096;
/// Deepest directory nesting (the root is depth 0).
pub const MAX_DEPTH: usize = 32;
pub const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;
/// Bytes of volatile (in-memory) file contents.
pub const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// Smallest volume `format` accepts.
pub const MIN_BLOCKS: u64 = 16;

const MAGIC: &[u8; 8] = b"OCEANSFS";
/// The format written; 1 (no data checksums) is still read.
const VERSION: u32 = 2;
const SUPERBLOCKS: u64 = 2;
/// Superblock layout.
const SB_META_LIST: usize = 48;
const SB_CRC: usize = BLOCK_SIZE - 4;
const MAX_META_BLOCKS: usize = (SB_CRC - SB_META_LIST) / 4;

const KIND_FILE: u8 = 1;
const KIND_DIRECTORY: u8 = 2;

/// Block states.
const FREE: u8 = 0;
/// Referenced by the durable state (or by live nodes since).
const USED: u8 = 1;
/// Allocated since the last commit: may be overwritten in place.
const FRESH: u8 = 2;
/// No longer referenced, but the durable state still does: freed at the
/// next commit.
const PENDING: u8 = 3;

const CACHE_SLOTS: usize = 64;

/// A device failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoError;

/// What the volume needs from a disk.
pub trait BlockDevice {
    fn block_count(&self) -> u64;
    fn read_block(&mut self, block: u64, out: &mut BlockBuf) -> Result<(), IoError>;
    fn write_block(&mut self, block: u64, data: &BlockBuf) -> Result<(), IoError>;
    /// Makes completed writes durable.
    fn flush(&mut self) -> Result<(), IoError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    Exists,
    NotADirectory,
    IsADirectory,
    NotEmpty,
    /// The node or its directory is read-only.
    ReadOnly,
    /// A data block does not match its checksum: the disk corrupted it.
    Corrupt,
    InvalidName,
    /// Quota, disk space or depth limit reached.
    NoSpace,
    Io,
}

impl From<IoError> for FsError {
    fn from(_: IoError) -> Self {
        Self::Io
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountError {
    /// All zeros where the superblocks go: never formatted.
    Blank,
    /// Not an OceansFS volume (and not blank): never touched.
    UnknownContents,
    /// An OceansFS volume that fails validation.
    Corrupt(&'static str),
    TooSmall,
    Io,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opened {
    Mounted,
    Formatted,
}

pub type NodeId = usize;
pub const ROOT: NodeId = 0;

/// Whether `name` may name a directory entry.
pub fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&0)
        && core::str::from_utf8(name).is_ok()
}

/// CRC-32C (Castagnoli).
pub fn crc32c(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0x82f6_3b78
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    !bytes.iter().fold(!0u32, |c, &b| {
        TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8)
    })
}

/// A file block on disk and the CRC-32C of its contents (block 0: a hole).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Extent {
    block: u32,
    crc: u32,
}

impl Extent {
    const HOLE: Self = Self { block: 0, crc: 0 };

    fn is_hole(self) -> bool {
        self.block == 0
    }
}

enum Data {
    Memory(Vec<u8>),
    Disk { size: u64, blocks: Vec<Extent> },
}

impl Data {
    fn size(&self) -> u64 {
        match self {
            Self::Memory(bytes) => bytes.len() as u64,
            Self::Disk { size, .. } => *size,
        }
    }
}

enum Content {
    File(Data),
    Directory(BTreeMap<String, NodeId>),
}

struct Node {
    content: Content,
    read_only: bool,
    /// Never written to disk (nor is anything below it).
    volatile: bool,
    /// A directory entry refers to the node (the root always counts).
    linked: bool,
    /// Open handles referring to the node.
    opens: u32,
    depth: u8,
}

impl Node {
    fn kind(&self) -> Kind {
        match self.content {
            Content::File(_) => Kind::File,
            Content::Directory(_) => Kind::Directory,
        }
    }
}

struct Superblock {
    version: u32,
    total_blocks: u64,
    generation: u64,
    meta_len: u64,
    meta_crc: u32,
    meta_blocks: Vec<u32>,
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

impl Superblock {
    fn encode(&self) -> Box<BlockBuf> {
        let mut out = Box::new([0u8; BLOCK_SIZE]);
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.total_blocks.to_le_bytes());
        out[24..32].copy_from_slice(&self.generation.to_le_bytes());
        out[32..40].copy_from_slice(&self.meta_len.to_le_bytes());
        out[40..44].copy_from_slice(&self.meta_crc.to_le_bytes());
        out[44..48].copy_from_slice(&(self.meta_blocks.len() as u32).to_le_bytes());
        for (i, block) in self.meta_blocks.iter().enumerate() {
            let at = SB_META_LIST + 4 * i;
            out[at..at + 4].copy_from_slice(&block.to_le_bytes());
        }
        let crc = crc32c(&out[..SB_CRC]);
        out[SB_CRC..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// `None`: no OceansFS magic. `Some(Err)`: magic, but invalid.
    fn decode(block: &BlockBuf) -> Option<Result<Self, &'static str>> {
        if &block[..8] != MAGIC {
            return None;
        }
        Some((|| {
            if crc32c(&block[..SB_CRC]) != u32_at(block, SB_CRC) {
                return Err("superblock checksum mismatch");
            }
            let version = u32_at(block, 8);
            if !(1..=VERSION).contains(&version) {
                return Err("unsupported format version");
            }
            if u32_at(block, 12) as usize != BLOCK_SIZE {
                return Err("unsupported block size");
            }
            let count = u32_at(block, 44) as usize;
            if count == 0 || count > MAX_META_BLOCKS {
                return Err("bad metadata block count");
            }
            let meta_len = u64_at(block, 32);
            if meta_len == 0 || meta_len > (count * BLOCK_SIZE) as u64 {
                return Err("bad metadata length");
            }
            Ok(Self {
                version,
                total_blocks: u64_at(block, 16),
                generation: u64_at(block, 24),
                meta_len,
                meta_crc: u32_at(block, 40),
                meta_blocks: (0..count)
                    .map(|i| u32_at(block, SB_META_LIST + 4 * i))
                    .collect(),
            })
        })())
    }
}

/// Reads untrusted metadata.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8], &'static str> {
        let end = self.at.checked_add(len).ok_or("metadata truncated")?;
        let bytes = self.bytes.get(self.at..end).ok_or("metadata truncated")?;
        self.at = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32_at(self.take(4)?, 0))
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64_at(self.take(8)?, 0))
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }
}

pub struct Volume<D> {
    /// `None`: an in-memory volume.
    device: Option<D>,
    nodes: Vec<Option<Node>>,
    free_slots: Vec<NodeId>,
    live_nodes: usize,
    memory_bytes: usize,
    states: Vec<u8>,
    free_blocks: u64,
    cursor: usize,
    generation: u64,
    meta_blocks: Vec<u32>,
    dirty: bool,
    cache: Vec<Option<(u32, Box<BlockBuf>)>>,
    /// The format commits write (always [`VERSION`]; tests write older
    /// ones to check upgrades).
    format: u32,
}

impl<D: BlockDevice> Volume<D> {
    fn empty(device: Option<D>, total_blocks: u64) -> Self {
        let root = Node {
            content: Content::Directory(BTreeMap::new()),
            read_only: false,
            volatile: device.is_none(),
            linked: true,
            opens: 0,
            depth: 0,
        };
        let mut states = vec![FREE; total_blocks as usize];
        for state in states.iter_mut().take(SUPERBLOCKS as usize) {
            *state = USED;
        }
        Self {
            device,
            nodes: vec![Some(root)],
            free_slots: Vec::new(),
            live_nodes: 1,
            memory_bytes: 0,
            states,
            free_blocks: total_blocks.saturating_sub(SUPERBLOCKS),
            cursor: SUPERBLOCKS as usize,
            generation: 0,
            meta_blocks: Vec::new(),
            dirty: false,
            cache: (0..CACHE_SLOTS).map(|_| None).collect(),
            format: VERSION,
        }
    }

    /// A volume that lives only in memory.
    pub fn memory() -> Self {
        Self::empty(None, 0)
    }

    /// Mounts the volume on `device`. A blank device (zeros where the
    /// superblocks go) is formatted if `format_blank`; anything that is not
    /// a valid OceansFS volume is left untouched.
    pub fn open(mut device: D, format_blank: bool) -> Result<(Self, Opened), MountError> {
        let total = device.block_count();
        if total < MIN_BLOCKS || total > u64::from(u32::MAX) {
            return Err(MountError::TooSmall);
        }
        let mut slots = [Box::new([0u8; BLOCK_SIZE]), Box::new([0u8; BLOCK_SIZE])];
        for (block, slot) in slots.iter_mut().enumerate() {
            device
                .read_block(block as u64, slot)
                .map_err(|_| MountError::Io)?;
        }
        let decoded: Vec<Option<Result<Superblock, &'static str>>> =
            slots.iter().map(|slot| Superblock::decode(slot)).collect();
        // The newest valid superblock is the last durable generation; an
        // invalid slot is a torn write of a newer one that never became
        // durable. If the newest valid generation does not load, the volume
        // is corrupt: an older one may reference blocks reused since, so it
        // is never used instead.
        let newest = decoded
            .iter()
            .filter_map(|d| d.as_ref().and_then(|r| r.as_ref().ok()))
            .max_by_key(|sb| sb.generation);
        if let Some(superblock) = newest {
            if superblock.total_blocks < MIN_BLOCKS || superblock.total_blocks > total {
                return Err(MountError::Corrupt("volume larger than the device"));
            }
            let mut volume = Self::empty(Some(device), superblock.total_blocks);
            return match volume.load(superblock) {
                Ok(()) => Ok((volume, Opened::Mounted)),
                Err(why) => Err(MountError::Corrupt(why)),
            };
        }
        if let Some(Some(Err(why))) = decoded.iter().find(|d| matches!(d, Some(Err(_)))) {
            return Err(MountError::Corrupt(why));
        }
        if slots.iter().any(|slot| slot.iter().any(|&b| b != 0)) {
            return Err(MountError::UnknownContents);
        }
        if !format_blank {
            return Err(MountError::Blank);
        }
        let mut volume = Self::empty(Some(device), total);
        volume.dirty = true;
        volume.commit().map_err(|error| match error {
            FsError::Io => MountError::Io,
            _ => MountError::TooSmall,
        })?;
        Ok((volume, Opened::Formatted))
    }

    fn load(&mut self, superblock: &Superblock) -> Result<(), &'static str> {
        let mut meta = Vec::new();
        let mut block = Box::new([0u8; BLOCK_SIZE]);
        for &number in &superblock.meta_blocks {
            self.claim(number)?;
            self.device
                .as_mut()
                .expect("device")
                .read_block(u64::from(number), &mut block)
                .map_err(|_| "cannot read metadata")?;
            meta.extend_from_slice(&block[..]);
        }
        meta.truncate(superblock.meta_len as usize);
        if crc32c(&meta) != superblock.meta_crc {
            return Err("metadata checksum mismatch");
        }
        self.parse(&meta, superblock.version)?;
        self.generation = superblock.generation;
        self.meta_blocks = superblock.meta_blocks.clone();
        if superblock.version < VERSION {
            self.compute_checksums()?;
            // Written as the current format at the next commit.
            self.dirty = true;
        }
        Ok(())
    }

    /// Format 1 has no data checksums: compute them from the disk.
    fn compute_checksums(&mut self) -> Result<(), &'static str> {
        let mut block = Box::new([0u8; BLOCK_SIZE]);
        for id in 0..self.nodes.len() {
            let Some(Node {
                content: Content::File(Data::Disk { blocks, .. }),
                ..
            }) = self.nodes[id].as_mut()
            else {
                continue;
            };
            let device = self.device.as_mut().expect("mounted");
            for extent in blocks.iter_mut().filter(|e| !e.is_hole()) {
                device
                    .read_block(u64::from(extent.block), &mut block)
                    .map_err(|_| "cannot read data to checksum it")?;
                extent.crc = crc32c(&block[..]);
            }
        }
        Ok(())
    }

    /// Marks a block referenced by the metadata as used, exactly once.
    fn claim(&mut self, block: u32) -> Result<(), &'static str> {
        let state = self
            .states
            .get_mut(block as usize)
            .filter(|_| u64::from(block) >= SUPERBLOCKS)
            .ok_or("block number out of range")?;
        if *state != FREE {
            return Err("block referenced twice");
        }
        *state = USED;
        self.free_blocks -= 1;
        Ok(())
    }

    fn parse(&mut self, bytes: &[u8], version: u32) -> Result<(), &'static str> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.u8()? != KIND_DIRECTORY || reader.u8()? != 0 {
            return Err("bad root record");
        }
        let root_children = reader.u32()?;
        // (directory, children still to read)
        let mut stack: Vec<(NodeId, u32)> = vec![(ROOT, root_children)];
        while let Some(top) = stack.last_mut() {
            if top.1 == 0 {
                stack.pop();
                continue;
            }
            top.1 -= 1;
            let parent = top.0;
            let depth = stack.len();
            if depth > MAX_DEPTH {
                return Err("directories nested too deep");
            }
            if self.live_nodes >= MAX_NODES {
                return Err("too many nodes");
            }
            let kind = reader.u8()?;
            let name_len = usize::from(reader.u8()?);
            let name = reader.take(name_len)?;
            if !valid_name(name) {
                return Err("invalid name");
            }
            let name = String::from(core::str::from_utf8(name).map_err(|_| "invalid name")?);
            let (content, children) = match kind {
                KIND_FILE => {
                    let size = reader.u64()?;
                    let count = reader.u32()? as usize;
                    if size > MAX_FILE_SIZE || count as u64 != size.div_ceil(BLOCK_SIZE as u64) {
                        return Err("bad file size");
                    }
                    let mut blocks = Vec::with_capacity(count);
                    for _ in 0..count {
                        let block = reader.u32()?;
                        let crc = if version >= 2 { reader.u32()? } else { 0 };
                        if block != 0 {
                            self.claim(block)?;
                        }
                        blocks.push(Extent { block, crc });
                    }
                    (Content::File(Data::Disk { size, blocks }), 0)
                }
                KIND_DIRECTORY => {
                    let children = reader.u32()?;
                    // Each child takes at least two bytes.
                    if children as usize > reader.remaining() / 2 {
                        return Err("bad child count");
                    }
                    (Content::Directory(BTreeMap::new()), children)
                }
                _ => return Err("unknown node kind"),
            };
            let id = self.insert_node(Node {
                content,
                read_only: false,
                volatile: false,
                linked: true,
                opens: 0,
                depth: depth as u8,
            });
            let Some(Content::Directory(entries)) =
                self.nodes[parent].as_mut().map(|n| &mut n.content)
            else {
                return Err("child of a file");
            };
            if entries.insert(name, id).is_some() {
                return Err("duplicate name");
            }
            if children > 0 {
                stack.push((id, children));
            }
        }
        if reader.remaining() != 0 {
            return Err("trailing metadata");
        }
        Ok(())
    }

    fn insert_node(&mut self, node: Node) -> NodeId {
        self.live_nodes += 1;
        match self.free_slots.pop() {
            Some(slot) => {
                self.nodes[slot] = Some(node);
                slot
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        }
    }

    // ---- Queries -----------------------------------------------------------

    pub fn is_persistent(&self) -> bool {
        self.device.is_some()
    }

    /// Changes not yet committed.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// (used, total) blocks, superblocks and metadata included.
    pub fn usage(&self) -> (u64, u64) {
        let total = self.states.len() as u64;
        (total - self.free_blocks, total)
    }

    pub fn device(&self) -> Option<&D> {
        self.device.as_ref()
    }

    pub fn device_mut(&mut self) -> Option<&mut D> {
        self.device.as_mut()
    }

    fn node(&self, id: NodeId) -> Result<&Node, FsError> {
        self.nodes
            .get(id)
            .and_then(Option::as_ref)
            .ok_or(FsError::NotFound)
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, FsError> {
        self.nodes
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(FsError::NotFound)
    }

    fn entries(&self, dir: NodeId) -> Result<&BTreeMap<String, NodeId>, FsError> {
        match &self.node(dir)?.content {
            Content::Directory(entries) => Ok(entries),
            Content::File(_) => Err(FsError::NotADirectory),
        }
    }

    pub fn kind(&self, id: NodeId) -> Result<Kind, FsError> {
        Ok(self.node(id)?.kind())
    }

    pub fn is_read_only(&self, id: NodeId) -> Result<bool, FsError> {
        Ok(self.node(id)?.read_only)
    }

    /// File size in bytes, or a directory's entry count.
    pub fn size(&self, id: NodeId) -> Result<u64, FsError> {
        Ok(match &self.node(id)?.content {
            Content::File(data) => data.size(),
            Content::Directory(entries) => entries.len() as u64,
        })
    }

    pub fn lookup(&self, dir: NodeId, name: &str) -> Result<NodeId, FsError> {
        self.entries(dir)?
            .get(name)
            .copied()
            .ok_or(FsError::NotFound)
    }

    /// Entry `index` of a directory, in name order.
    pub fn entry(&self, dir: NodeId, index: usize) -> Result<(&str, Kind), FsError> {
        let (name, &child) = self
            .entries(dir)?
            .iter()
            .nth(index)
            .ok_or(FsError::NotFound)?;
        Ok((name.as_str(), self.node(child)?.kind()))
    }

    // ---- Open references ---------------------------------------------------

    /// An open handle now refers to `id`.
    pub fn retain(&mut self, id: NodeId) -> Result<(), FsError> {
        self.node_mut(id)?.opens += 1;
        Ok(())
    }

    /// An open handle to `id` closed; frees it if it was also unlinked.
    pub fn release(&mut self, id: NodeId) {
        if let Ok(node) = self.node_mut(id) {
            node.opens = node.opens.saturating_sub(1);
        }
        self.free_if_unused(id);
    }

    fn free_if_unused(&mut self, id: NodeId) {
        let Ok(node) = self.node(id) else {
            return;
        };
        if id == ROOT || node.linked || node.opens > 0 {
            return;
        }
        if let Some(node) = self.nodes[id].take() {
            match node.content {
                Content::File(Data::Memory(bytes)) => self.memory_bytes -= bytes.len(),
                Content::File(Data::Disk { blocks, .. }) => {
                    for extent in blocks.into_iter().filter(|e| !e.is_hole()) {
                        self.release_block(extent.block);
                    }
                }
                Content::Directory(_) => {}
            }
        }
        self.free_slots.push(id);
        self.live_nodes -= 1;
    }

    // ---- Namespace ---------------------------------------------------------

    /// Creates an empty file or directory `name` in `dir`. Below a volatile
    /// directory, or on an in-memory volume, the node is volatile too.
    pub fn create(&mut self, dir: NodeId, name: &str, kind: Kind) -> Result<NodeId, FsError> {
        let parent = self.node(dir)?;
        if parent.read_only {
            return Err(FsError::ReadOnly);
        }
        let volatile = parent.volatile;
        self.create_node(dir, name, kind, volatile, false, None)
    }

    /// Creates a volatile directory (never written to disk), e.g. `/bin`.
    pub fn create_volatile_directory(
        &mut self,
        dir: NodeId,
        name: &str,
        read_only: bool,
    ) -> Result<NodeId, FsError> {
        self.create_node(dir, name, Kind::Directory, true, read_only, None)
    }

    /// Publishes `bytes` as a volatile, read-only file.
    pub fn publish(&mut self, dir: NodeId, name: &str, bytes: Vec<u8>) -> Result<NodeId, FsError> {
        self.create_node(dir, name, Kind::File, true, true, Some(bytes))
    }

    fn create_node(
        &mut self,
        dir: NodeId,
        name: &str,
        kind: Kind,
        volatile: bool,
        read_only: bool,
        bytes: Option<Vec<u8>>,
    ) -> Result<NodeId, FsError> {
        if !valid_name(name.as_bytes()) {
            return Err(FsError::InvalidName);
        }
        let parent = self.node(dir)?;
        let depth = usize::from(parent.depth) + 1;
        if self.entries(dir)?.contains_key(name) {
            return Err(FsError::Exists);
        }
        if self.live_nodes >= MAX_NODES || depth > MAX_DEPTH {
            return Err(FsError::NoSpace);
        }
        if !volatile {
            // The node's metadata record must fit at the next commit.
            self.ensure_room(2 + name.len() + 12, 0)?;
        }
        let content = match kind {
            Kind::Directory => Content::Directory(BTreeMap::new()),
            Kind::File if volatile => {
                let bytes = bytes.unwrap_or_default();
                if self.memory_bytes + bytes.len() > MAX_MEMORY_BYTES {
                    return Err(FsError::NoSpace);
                }
                self.memory_bytes += bytes.len();
                Content::File(Data::Memory(bytes))
            }
            Kind::File => Content::File(Data::Disk {
                size: 0,
                blocks: Vec::new(),
            }),
        };
        let id = self.insert_node(Node {
            content,
            read_only,
            volatile,
            linked: true,
            opens: 0,
            depth: depth as u8,
        });
        if let Ok(Node {
            content: Content::Directory(entries),
            ..
        }) = self.node_mut(dir)
        {
            entries.insert(String::from(name), id);
        }
        self.dirty |= !volatile;
        Ok(id)
    }

    /// Unlinks `name` from `dir` (directories must be empty). The node
    /// itself goes once no handle refers to it.
    pub fn remove(&mut self, dir: NodeId, name: &str) -> Result<(), FsError> {
        let child = self.lookup(dir, name)?;
        let node = self.node(child)?;
        if node.read_only || self.node(dir)?.read_only {
            return Err(FsError::ReadOnly);
        }
        if let Content::Directory(entries) = &node.content
            && !entries.is_empty()
        {
            return Err(FsError::NotEmpty);
        }
        let volatile = node.volatile;
        if let Content::Directory(entries) = &mut self.node_mut(dir)?.content {
            entries.remove(name);
        }
        self.node_mut(child)?.linked = false;
        self.free_if_unused(child);
        self.dirty |= !volatile;
        Ok(())
    }

    // ---- File contents -----------------------------------------------------

    /// Reads up to `out.len()` bytes at `offset`; returns how many.
    pub fn read(&mut self, id: NodeId, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        let (size, blocks) = match &self.node(id)?.content {
            Content::Directory(_) => return Err(FsError::IsADirectory),
            Content::File(Data::Memory(bytes)) => {
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let len = out.len().min(bytes.len() - start);
                out[..len].copy_from_slice(&bytes[start..start + len]);
                return Ok(len);
            }
            Content::File(Data::Disk { size, blocks }) => (*size, blocks.clone()),
        };
        let start = offset.min(size);
        let len = (out.len() as u64).min(size - start) as usize;
        let mut done = 0;
        while done < len {
            let position = start + done as u64;
            let index = (position / BLOCK_SIZE as u64) as usize;
            let in_block = (position % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - in_block).min(len - done);
            let extent = blocks[index];
            if extent.is_hole() {
                out[done..done + chunk].fill(0);
            } else {
                let data = self.read_cached(extent)?;
                out[done..done + chunk].copy_from_slice(&data[in_block..in_block + chunk]);
            }
            done += chunk;
        }
        Ok(len)
    }

    /// Writes `data` at `offset`, extending the file as needed.
    pub fn write(&mut self, id: NodeId, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|&end| end <= MAX_FILE_SIZE)
            .ok_or(FsError::NoSpace)?;
        let node = self.node(id)?;
        if node.read_only {
            return Err(FsError::ReadOnly);
        }
        match &node.content {
            Content::Directory(_) => Err(FsError::IsADirectory),
            Content::File(Data::Memory(bytes)) => {
                let growth = (end as usize).saturating_sub(bytes.len());
                if self.memory_bytes + growth > MAX_MEMORY_BYTES {
                    return Err(FsError::NoSpace);
                }
                self.memory_bytes += growth;
                let Ok(Node {
                    content: Content::File(Data::Memory(bytes)),
                    ..
                }) = self.node_mut(id)
                else {
                    unreachable!()
                };
                if end as usize > bytes.len() {
                    bytes.resize(end as usize, 0);
                }
                bytes[offset as usize..end as usize].copy_from_slice(data);
                Ok(data.len())
            }
            Content::File(Data::Disk { size, .. }) => {
                if data.is_empty() {
                    return Ok(0);
                }
                let size = *size;
                if offset > size {
                    self.zero_tail(id, size, offset)?;
                }
                self.write_disk(id, offset, data)?;
                Ok(data.len())
            }
        }
    }

    /// Takes a disk file's block list out of its node (and puts it back
    /// with `put_blocks`), so blocks can be changed while the device is
    /// borrowed.
    fn take_blocks(&mut self, id: NodeId) -> (u64, Vec<Extent>) {
        match self.node_mut(id) {
            Ok(Node {
                content: Content::File(Data::Disk { size, blocks }),
                ..
            }) => (*size, core::mem::take(blocks)),
            _ => unreachable!("checked by callers"),
        }
    }

    fn put_blocks(&mut self, id: NodeId, new_size: u64, new_blocks: Vec<Extent>) {
        if let Ok(Node {
            content: Content::File(Data::Disk { size, blocks }),
            ..
        }) = self.node_mut(id)
        {
            *size = new_size;
            *blocks = new_blocks;
        }
    }

    fn write_disk(&mut self, id: NodeId, offset: u64, data: &[u8]) -> Result<(), FsError> {
        let end = offset + data.len() as u64;
        let first = (offset / BLOCK_SIZE as u64) as usize;
        let last = ((end - 1) / BLOCK_SIZE as u64) as usize;
        let Content::File(Data::Disk { blocks, .. }) = &self.node(id)?.content else {
            unreachable!("checked by callers");
        };
        // Every block not written since the last commit is copied.
        let copies = (first..=last)
            .filter(|&i| {
                blocks
                    .get(i)
                    .is_none_or(|e| e.is_hole() || self.states[e.block as usize] != FRESH)
            })
            .count();
        let new_entries = (last + 1).saturating_sub(blocks.len());
        self.ensure_room(8 * new_entries, copies as u64)?;
        let (size, mut blocks) = self.take_blocks(id);
        if blocks.len() <= last {
            blocks.resize(last + 1, Extent::HOLE);
        }
        let mut result = Ok(());
        for (index, slot) in blocks.iter_mut().enumerate().take(last + 1).skip(first) {
            let block_start = index as u64 * BLOCK_SIZE as u64;
            let from = offset.max(block_start);
            let to = end.min(block_start + BLOCK_SIZE as u64);
            let old = *slot;
            let mut buffer: Box<BlockBuf> = if old.is_hole() {
                Box::new([0u8; BLOCK_SIZE])
            } else {
                match self.read_cached(old) {
                    Ok(data) => Box::new(*data),
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
            };
            buffer[(from - block_start) as usize..(to - block_start) as usize]
                .copy_from_slice(&data[(from - offset) as usize..(to - offset) as usize]);
            let crc = crc32c(&buffer[..]);
            if !old.is_hole() && self.states[old.block as usize] == FRESH {
                if let Err(error) = self.write_cached(old.block, buffer) {
                    result = Err(error);
                    break;
                }
                slot.crc = crc;
            } else {
                let new = self.allocate_block().expect("room ensured");
                if let Err(error) = self.write_cached(new, buffer) {
                    self.release_block(new);
                    result = Err(error);
                    break;
                }
                *slot = Extent { block: new, crc };
                if !old.is_hole() {
                    self.release_block(old.block);
                }
            }
        }
        // On success the file grows to `end`; on failure, blocks past the
        // old size are dropped so size and block list always agree.
        let new_size = if result.is_ok() { size.max(end) } else { size };
        let keep = new_size.div_ceil(BLOCK_SIZE as u64) as usize;
        for extent in blocks.drain(keep.min(blocks.len())..) {
            if !extent.is_hole() {
                self.release_block(extent.block);
            }
        }
        self.put_blocks(id, new_size, blocks);
        self.dirty = true;
        result
    }

    /// Sets a file's size: shrinking discards data, growing adds zeros.
    pub fn truncate(&mut self, id: NodeId, new_size: u64) -> Result<(), FsError> {
        if new_size > MAX_FILE_SIZE {
            return Err(FsError::NoSpace);
        }
        let node = self.node(id)?;
        if node.read_only {
            return Err(FsError::ReadOnly);
        }
        match &node.content {
            Content::Directory(_) => Err(FsError::IsADirectory),
            Content::File(Data::Memory(bytes)) => {
                let new_size = new_size as usize;
                let growth = new_size.saturating_sub(bytes.len());
                if self.memory_bytes + growth > MAX_MEMORY_BYTES {
                    return Err(FsError::NoSpace);
                }
                self.memory_bytes =
                    self.memory_bytes + growth - bytes.len().saturating_sub(new_size);
                if let Ok(Node {
                    content: Content::File(Data::Memory(bytes)),
                    ..
                }) = self.node_mut(id)
                {
                    bytes.resize(new_size, 0);
                }
                Ok(())
            }
            Content::File(Data::Disk { size, blocks }) => {
                let size = *size;
                let count = new_size.div_ceil(BLOCK_SIZE as u64) as usize;
                if count > blocks.len() {
                    self.ensure_room(8 * (count - blocks.len()), 0)?;
                }
                if new_size > size {
                    self.zero_tail(id, size, new_size)?;
                }
                let (_, mut blocks) = self.take_blocks(id);
                for extent in blocks.drain(count.min(blocks.len())..) {
                    if !extent.is_hole() {
                        self.release_block(extent.block);
                    }
                }
                blocks.resize(count, Extent::HOLE);
                self.put_blocks(id, new_size, blocks);
                self.dirty = true;
                Ok(())
            }
        }
    }

    /// Bytes past the end of a file's partial last block are stale (a
    /// shrink does no I/O, so it works on a full disk). Before the file
    /// grows from `size` toward `to`, they are zeroed.
    fn zero_tail(&mut self, id: NodeId, size: u64, to: u64) -> Result<(), FsError> {
        let end = to.min(size.next_multiple_of(BLOCK_SIZE as u64));
        if end > size {
            let zeros = vec![0u8; (end - size) as usize];
            self.write_disk(id, size, &zeros)?;
        }
        Ok(())
    }

    // ---- Blocks ------------------------------------------------------------

    fn allocate_block(&mut self) -> Option<u32> {
        let total = self.states.len();
        for step in 0..total {
            let index = (self.cursor + step) % total;
            if self.states[index] == FREE {
                self.states[index] = FRESH;
                self.free_blocks -= 1;
                self.cursor = index + 1;
                return Some(index as u32);
            }
        }
        None
    }

    fn release_block(&mut self, block: u32) {
        let state = &mut self.states[block as usize];
        match *state {
            FRESH => {
                *state = FREE;
                self.free_blocks += 1;
            }
            USED => *state = PENDING,
            other => debug_assert!(false, "releasing block {block} in state {other}"),
        }
    }

    /// Bytes the metadata would take if committed now.
    fn meta_size(&self) -> usize {
        let mut size = 0;
        let mut stack = vec![ROOT];
        while let Some(id) = stack.pop() {
            let Some(node) = self.nodes[id].as_ref() else {
                continue;
            };
            match &node.content {
                Content::File(Data::Disk { blocks, .. }) => size += 12 + 8 * blocks.len(),
                Content::File(Data::Memory(_)) => {}
                Content::Directory(entries) => {
                    size += 4;
                    for (name, &child) in entries {
                        if self.nodes[child].as_ref().is_some_and(|c| !c.volatile) {
                            size += 2 + name.len();
                            stack.push(child);
                        }
                    }
                }
            }
        }
        size + 2
    }

    /// Fails with `NoSpace` unless `blocks` data blocks can be allocated
    /// and the metadata, grown by `extra_meta` bytes, still fits at the next
    /// commit (it needs fresh blocks: the durable copy stays intact).
    fn ensure_room(&self, extra_meta: usize, blocks: u64) -> Result<(), FsError> {
        if self.device.is_none() {
            return Ok(());
        }
        let meta_blocks = (self.meta_size() + extra_meta).div_ceil(BLOCK_SIZE);
        if meta_blocks > MAX_META_BLOCKS || blocks + meta_blocks as u64 > self.free_blocks {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// A data block's contents, verified against its checksum when read
    /// from the disk (cached blocks were verified or written by us).
    fn read_cached(&mut self, extent: Extent) -> Result<&BlockBuf, FsError> {
        let block = extent.block;
        let slot = block as usize % CACHE_SLOTS;
        let hit = matches!(&self.cache[slot], Some((b, _)) if *b == block);
        if !hit {
            let mut data = Box::new([0u8; BLOCK_SIZE]);
            self.device
                .as_mut()
                .ok_or(FsError::Io)?
                .read_block(u64::from(block), &mut data)?;
            if crc32c(&data[..]) != extent.crc {
                return Err(FsError::Corrupt);
            }
            self.cache[slot] = Some((block, data));
        }
        Ok(&self.cache[slot].as_ref().expect("filled").1)
    }

    /// Writes through to the device and keeps the block cached.
    fn write_cached(&mut self, block: u32, data: Box<BlockBuf>) -> Result<(), FsError> {
        let slot = block as usize % CACHE_SLOTS;
        let device = self.device.as_mut().ok_or(FsError::Io)?;
        match device.write_block(u64::from(block), &data) {
            Ok(()) => {
                self.cache[slot] = Some((block, data));
                Ok(())
            }
            Err(error) => {
                if matches!(&self.cache[slot], Some((b, _)) if *b == block) {
                    self.cache[slot] = None;
                }
                Err(error.into())
            }
        }
    }

    // ---- Commit ------------------------------------------------------------

    fn serialize(&self) -> Vec<u8> {
        fn record<D>(volume: &Volume<D>, id: NodeId, name: &str, out: &mut Vec<u8>) {
            let node = volume.nodes[id].as_ref().expect("linked nodes are live");
            match &node.content {
                Content::File(data) => {
                    let Data::Disk { size, blocks } = data else {
                        unreachable!("persistent files live on disk");
                    };
                    out.push(KIND_FILE);
                    out.push(name.len() as u8);
                    out.extend_from_slice(name.as_bytes());
                    out.extend_from_slice(&size.to_le_bytes());
                    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
                    for extent in blocks {
                        out.extend_from_slice(&extent.block.to_le_bytes());
                        if volume.format >= 2 {
                            out.extend_from_slice(&extent.crc.to_le_bytes());
                        }
                    }
                }
                Content::Directory(entries) => {
                    out.push(KIND_DIRECTORY);
                    out.push(name.len() as u8);
                    out.extend_from_slice(name.as_bytes());
                    let persistent: Vec<(&String, NodeId)> = entries
                        .iter()
                        .map(|(name, &child)| (name, child))
                        .filter(|&(_, child)| {
                            volume.nodes[child].as_ref().is_some_and(|c| !c.volatile)
                        })
                        .collect();
                    out.extend_from_slice(&(persistent.len() as u32).to_le_bytes());
                    for (name, child) in persistent {
                        record(volume, child, name, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        record(self, ROOT, "", &mut out);
        out
    }

    /// Makes every change so far durable, atomically. Nothing to do if
    /// nothing changed, or on an in-memory volume.
    pub fn commit(&mut self) -> Result<(), FsError> {
        if !self.dirty || self.device.is_none() {
            return Ok(());
        }
        let meta = self.serialize();
        let count = meta.len().div_ceil(BLOCK_SIZE).max(1);
        if count > MAX_META_BLOCKS {
            return Err(FsError::NoSpace);
        }
        let mut new_blocks = Vec::with_capacity(count);
        for _ in 0..count {
            match self.allocate_block() {
                Some(block) => new_blocks.push(block),
                None => {
                    for block in new_blocks {
                        self.release_block(block);
                    }
                    return Err(FsError::NoSpace);
                }
            }
        }
        let superblock = Superblock {
            version: self.format,
            total_blocks: self.states.len() as u64,
            generation: self.generation + 1,
            meta_len: meta.len() as u64,
            meta_crc: crc32c(&meta),
            meta_blocks: new_blocks.clone(),
        };
        let written = (|| {
            let device = self.device.as_mut().expect("checked");
            for (chunk, &block) in meta.chunks(BLOCK_SIZE).zip(&new_blocks) {
                let mut buffer = [0u8; BLOCK_SIZE];
                buffer[..chunk.len()].copy_from_slice(chunk);
                device.write_block(u64::from(block), &buffer)?;
            }
            device.flush()?;
            device.write_block(superblock.generation % SUPERBLOCKS, &superblock.encode())?;
            device.flush()
        })();
        if let Err(error) = written {
            for block in new_blocks {
                self.release_block(block);
            }
            return Err(error.into());
        }
        // The new generation is durable: what it references is used, what
        // only the old one referenced is free.
        for state in &mut self.states {
            match *state {
                FRESH => *state = USED,
                PENDING => {
                    *state = FREE;
                    self.free_blocks += 1;
                }
                _ => {}
            }
        }
        for block in core::mem::replace(&mut self.meta_blocks, new_blocks) {
            self.states[block as usize] = FREE;
            self.free_blocks += 1;
        }
        self.generation = superblock.generation;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;
    use std::string::ToString;
    use std::vec::Vec as StdVec;

    /// An in-memory disk that can log writes, to replay a crash at any
    /// point.
    #[derive(Clone)]
    struct MemDisk {
        image: StdVec<u8>,
        log: Option<StdVec<(u64, Box<BlockBuf>)>>,
        fail_writes: bool,
    }

    impl MemDisk {
        fn new(blocks: u64) -> Self {
            Self {
                image: vec![0; blocks as usize * BLOCK_SIZE],
                log: None,
                fail_writes: false,
            }
        }
    }

    impl BlockDevice for MemDisk {
        fn block_count(&self) -> u64 {
            (self.image.len() / BLOCK_SIZE) as u64
        }

        fn read_block(&mut self, block: u64, out: &mut BlockBuf) -> Result<(), IoError> {
            let at = block as usize * BLOCK_SIZE;
            out.copy_from_slice(self.image.get(at..at + BLOCK_SIZE).ok_or(IoError)?);
            Ok(())
        }

        fn write_block(&mut self, block: u64, data: &BlockBuf) -> Result<(), IoError> {
            if self.fail_writes {
                return Err(IoError);
            }
            let at = block as usize * BLOCK_SIZE;
            self.image
                .get_mut(at..at + BLOCK_SIZE)
                .ok_or(IoError)?
                .copy_from_slice(data);
            if let Some(log) = &mut self.log {
                log.push((block, Box::new(*data)));
            }
            Ok(())
        }

        fn flush(&mut self) -> Result<(), IoError> {
            Ok(())
        }
    }

    /// Every file and directory as `path = contents` lines, sorted.
    fn snapshot<D: BlockDevice>(volume: &mut Volume<D>) -> StdVec<std::string::String> {
        let mut lines = StdVec::new();
        let mut stack = vec![(ROOT, std::string::String::new())];
        while let Some((dir, path)) = stack.pop() {
            let mut index = 0;
            while let Ok((name, kind)) = volume
                .entry(dir, index)
                .map(|(name, kind)| (name.to_string(), kind))
            {
                let child_path = format!("{path}/{name}");
                let child = volume.lookup(dir, &name).unwrap();
                match kind {
                    Kind::Directory => {
                        lines.push(format!("{child_path}/"));
                        stack.push((child, child_path));
                    }
                    Kind::File => {
                        let size = volume.size(child).unwrap() as usize;
                        let mut bytes = vec![0u8; size];
                        assert_eq!(volume.read(child, 0, &mut bytes).unwrap(), size);
                        lines.push(format!(
                            "{child_path} = {}",
                            std::string::String::from_utf8_lossy(&bytes)
                        ));
                    }
                }
                index += 1;
            }
        }
        lines.sort();
        lines
    }

    fn file(volume: &mut Volume<MemDisk>, dir: NodeId, name: &str, text: &[u8]) -> NodeId {
        let id = volume.create(dir, name, Kind::File).unwrap();
        assert_eq!(volume.write(id, 0, text).unwrap(), text.len());
        id
    }

    fn reopen(disk: &MemDisk) -> Volume<MemDisk> {
        let mut disk = disk.clone();
        disk.log = None;
        let (volume, opened) = Volume::open(disk, false).expect("mount");
        assert_eq!(opened, Opened::Mounted);
        volume
    }

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn formats_blank_disks_only() {
        assert_eq!(
            Volume::open(MemDisk::new(64), false).err(),
            Some(MountError::Blank)
        );
        let (volume, opened) = Volume::open(MemDisk::new(64), true).unwrap();
        assert_eq!(opened, Opened::Formatted);
        assert_eq!(volume.generation(), 1);
        assert!(!volume.is_dirty());

        let mut foreign = MemDisk::new(64);
        foreign.image[..16].copy_from_slice(b"Oceans data disk");
        assert_eq!(
            Volume::open(foreign, true).err(),
            Some(MountError::UnknownContents)
        );
        assert_eq!(
            Volume::open(MemDisk::new(8), true).err(),
            Some(MountError::TooSmall)
        );
    }

    #[test]
    fn contents_survive_remount() {
        let (mut volume, _) = Volume::open(MemDisk::new(256), true).unwrap();
        let docs = volume.create(ROOT, "docs", Kind::Directory).unwrap();
        file(&mut volume, docs, "a.txt", b"hello disk");
        let big: StdVec<u8> = (0..3 * BLOCK_SIZE + 100).map(|i| (i % 251) as u8).collect();
        file(&mut volume, ROOT, "big", &big);
        volume.commit().unwrap();
        let before = snapshot(&mut volume);

        let mut again = reopen(volume.device().unwrap());
        assert_eq!(snapshot(&mut again), before);
        assert_eq!(again.generation(), volume.generation());
        assert_eq!(
            again.usage(),
            volume.usage(),
            "free space recomputed exactly"
        );
        let id = again.lookup(ROOT, "big").unwrap();
        let mut read = vec![0u8; big.len()];
        again.read(id, 0, &mut read).unwrap();
        assert_eq!(read, big);
    }

    #[test]
    fn uncommitted_changes_are_not_durable() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        file(&mut volume, ROOT, "kept", b"1");
        volume.commit().unwrap();
        file(&mut volume, ROOT, "lost", b"2");
        let kept = volume.lookup(ROOT, "kept").unwrap();
        volume.write(kept, 0, b"changed").unwrap();
        assert!(volume.is_dirty());
        let mut again = reopen(volume.device().unwrap());
        assert_eq!(snapshot(&mut again), ["/kept = 1"]);
    }

    #[test]
    fn a_crash_at_any_write_leaves_the_old_or_the_new_state() {
        let (mut volume, _) = Volume::open(MemDisk::new(128), true).unwrap();
        let docs = volume.create(ROOT, "docs", Kind::Directory).unwrap();
        file(&mut volume, docs, "a", &vec![b'a'; 5000]);
        file(&mut volume, ROOT, "b", b"bee");
        volume.commit().unwrap();
        let old = snapshot(&mut volume);
        let base = volume.device().unwrap().image.clone();

        volume.device_mut().unwrap().log = Some(StdVec::new());
        // Overwrite committed data, delete, create, nest, then commit.
        let a = volume.lookup(docs, "a").unwrap();
        volume.write(a, 4090, b"XXXXXXXXXXXX").unwrap();
        volume.remove(ROOT, "b").unwrap();
        let more = volume.create(docs, "more", Kind::Directory).unwrap();
        file(&mut volume, more, "c", b"sea");
        volume.commit().unwrap();
        let new = snapshot(&mut volume);
        let log = volume.device_mut().unwrap().log.take().unwrap();
        assert!(log.len() >= 4);

        for crash_after in 0..=log.len() {
            for torn in [false, true] {
                let mut disk = MemDisk {
                    image: base.clone(),
                    log: None,
                    fail_writes: false,
                };
                for (i, (block, data)) in
                    log.iter().take(crash_after + usize::from(torn)).enumerate()
                {
                    let at = *block as usize * BLOCK_SIZE;
                    // A torn write lands only partly.
                    let len = if torn && i == crash_after {
                        512
                    } else {
                        BLOCK_SIZE
                    };
                    disk.image[at..at + len].copy_from_slice(&data[..len]);
                }
                let mut mounted = reopen(&disk);
                let state = snapshot(&mut mounted);
                assert!(
                    state == old || state == new,
                    "crash after {crash_after} writes (torn: {torn}) gave {state:?}"
                );
                if crash_after == log.len() {
                    assert_eq!(state, new);
                }
            }
        }
    }

    #[test]
    fn copy_on_write_never_touches_committed_blocks() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let id = file(&mut volume, ROOT, "f", b"first version");
        volume.commit().unwrap();
        let committed: StdVec<u64> = (0..64)
            .filter(|&b| volume.states[b as usize] == USED)
            .collect();
        volume.device_mut().unwrap().log = Some(StdVec::new());
        volume.write(id, 0, b"second").unwrap();
        volume.write(id, 0, b"third!").unwrap();
        let log = volume.device_mut().unwrap().log.take().unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|(block, _)| !committed.contains(block)));
        assert_eq!(log[0].0, log[1].0, "a fresh block is rewritten in place");
    }

    #[test]
    fn holes_truncation_and_zeroed_tails() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let id = file(&mut volume, ROOT, "f", b"0123456789");
        volume.write(id, 3 * BLOCK_SIZE as u64, b"end").unwrap();
        let mut buffer = vec![1u8; 3 * BLOCK_SIZE + 3];
        volume.read(id, 0, &mut buffer).unwrap();
        assert_eq!(&buffer[..10], b"0123456789");
        assert!(
            buffer[10..3 * BLOCK_SIZE].iter().all(|&b| b == 0),
            "holes read as zeros"
        );
        let (used_with_holes, _) = volume.usage();

        volume.truncate(id, 4).unwrap();
        volume.truncate(id, 10).unwrap();
        let mut buffer = [9u8; 10];
        volume.read(id, 0, &mut buffer).unwrap();
        assert_eq!(&buffer, b"0123\0\0\0\0\0\0", "a shrink zeroes the tail");
        volume.commit().unwrap();
        assert!(volume.usage().0 < used_with_holes);
        let mut again = reopen(volume.device().unwrap());
        assert_eq!(snapshot(&mut again), ["/f = 0123\0\0\0\0\0\0"]);
    }

    #[test]
    fn running_out_of_space_keeps_the_volume_committable() {
        let (mut volume, _) = Volume::open(MemDisk::new(32), true).unwrap();
        let id = volume.create(ROOT, "fill", Kind::File).unwrap();
        let chunk = [7u8; BLOCK_SIZE];
        let mut offset = 0;
        loop {
            match volume.write(id, offset, &chunk) {
                Ok(_) => offset += BLOCK_SIZE as u64,
                Err(error) => {
                    assert_eq!(error, FsError::NoSpace);
                    break;
                }
            }
        }
        assert!(offset > 0);
        volume.commit().expect("room was kept for the metadata");
        let again = reopen(volume.device().unwrap());
        let id = again.lookup(ROOT, "fill").unwrap();
        assert_eq!(again.size(id).unwrap(), offset);
    }

    #[test]
    fn deleting_frees_blocks_after_commit() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let empty = volume.usage().0;
        file(&mut volume, ROOT, "f", &[1u8; 3 * BLOCK_SIZE]);
        volume.commit().unwrap();
        volume.remove(ROOT, "f").unwrap();
        volume.commit().unwrap();
        assert_eq!(volume.usage().0, empty);
    }

    #[test]
    fn open_unlinked_files_stay_readable() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let id = file(&mut volume, ROOT, "f", b"still here");
        volume.retain(id).unwrap();
        volume.remove(ROOT, "f").unwrap();
        volume.commit().unwrap();
        let mut buffer = [0u8; 10];
        volume.read(id, 0, &mut buffer).unwrap();
        assert_eq!(&buffer, b"still here");
        volume.release(id);
        assert_eq!(volume.kind(id).err(), Some(FsError::NotFound));
    }

    #[test]
    fn volatile_nodes_are_never_written() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let bin = volume.create_volatile_directory(ROOT, "bin", true).unwrap();
        volume.publish(bin, "ps", b"\x7fELF".to_vec()).unwrap();
        assert_eq!(
            volume.create(bin, "x", Kind::File).err(),
            Some(FsError::ReadOnly)
        );
        assert!(!volume.is_dirty(), "volatile changes need no commit");
        let ps = volume.lookup(bin, "ps").unwrap();
        assert_eq!(volume.write(ps, 0, b"x").err(), Some(FsError::ReadOnly));
        assert_eq!(volume.remove(bin, "ps").err(), Some(FsError::ReadOnly));
        file(&mut volume, ROOT, "real", b"1");
        volume.commit().unwrap();
        let mut again = reopen(volume.device().unwrap());
        assert_eq!(snapshot(&mut again), ["/real = 1"]);
    }

    #[test]
    fn memory_volumes_work_without_a_device() {
        let mut volume: Volume<MemDisk> = Volume::memory();
        let id = volume.create(ROOT, "f", Kind::File).unwrap();
        volume.write(id, 2, b"hi").unwrap();
        let mut buffer = [9u8; 4];
        assert_eq!(volume.read(id, 0, &mut buffer).unwrap(), 4);
        assert_eq!(&buffer, b"\0\0hi");
        volume.commit().unwrap();
        assert!(!volume.is_persistent());
    }

    #[test]
    fn limits_are_enforced_at_creation() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let mut dir = ROOT;
        for depth in 1..=MAX_DEPTH {
            dir = volume
                .create(dir, &format!("d{depth}"), Kind::Directory)
                .unwrap();
        }
        assert_eq!(
            volume.create(dir, "deeper", Kind::Directory).err(),
            Some(FsError::NoSpace)
        );
        assert_eq!(
            volume.create(ROOT, "d1", Kind::File).err(),
            Some(FsError::Exists)
        );
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            assert_eq!(
                volume.create(ROOT, bad, Kind::File).err(),
                Some(FsError::InvalidName)
            );
        }
        let long = "x".repeat(MAX_NAME + 1);
        assert_eq!(
            volume.create(ROOT, &long, Kind::File).err(),
            Some(FsError::InvalidName)
        );
        volume.commit().unwrap();
        // The deepest tree the volume creates also mounts.
        let mut again = reopen(volume.device().unwrap());
        assert_eq!(snapshot(&mut again).len(), MAX_DEPTH);
    }

    #[test]
    fn io_errors_leave_a_consistent_volume() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let id = file(&mut volume, ROOT, "f", b"safe");
        volume.commit().unwrap();
        volume.device_mut().unwrap().fail_writes = true;
        assert_eq!(
            volume.write(id, 2, &[1u8; 2 * BLOCK_SIZE]).err(),
            Some(FsError::Io)
        );
        assert_eq!(volume.size(id).unwrap(), 4, "the size did not grow");
        assert_eq!(volume.commit().err(), Some(FsError::Io));
        volume.device_mut().unwrap().fail_writes = false;
        volume.commit().unwrap();
        let mut again = reopen(volume.device().unwrap());
        assert_eq!(snapshot(&mut again), ["/f = safe"]);
    }

    #[test]
    fn corrupt_metadata_is_refused_not_trusted() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let docs = volume.create(ROOT, "docs", Kind::Directory).unwrap();
        file(&mut volume, docs, "a", b"x");
        volume.commit().unwrap();
        let disk = volume.device().unwrap().clone();
        let meta_block = volume.meta_blocks[0] as usize;

        // A flipped bit anywhere in the metadata fails its checksum.
        let mut flipped = disk.clone();
        flipped.image[meta_block * BLOCK_SIZE + 5] ^= 1;
        assert!(matches!(
            Volume::open(flipped, true).err(),
            Some(MountError::Corrupt(_))
        ));

        // Both superblocks damaged: corrupt, never reformatted.
        let mut damaged = disk.clone();
        damaged.image[100] ^= 1;
        damaged.image[BLOCK_SIZE + 100] ^= 1;
        assert!(matches!(
            Volume::open(damaged, true).err(),
            Some(MountError::Corrupt(_))
        ));

        // The parser itself survives arbitrary input (checksums bypassed).
        let meta = volume.serialize();
        for i in 0..meta.len() {
            for value in [0u8, 1, 2, 0x7f, 0xff] {
                let mut mutated = meta.clone();
                mutated[i] = value;
                let mut fresh = Volume::empty(Some(MemDisk::new(64)), 64);
                let _ = fresh.parse(&mutated, VERSION);
            }
        }
        let mut fresh = Volume::empty(Some(MemDisk::new(64)), 64);
        assert!(fresh.parse(&meta[..meta.len() - 1], VERSION).is_err());
    }

    #[test]
    fn corrupted_data_is_reported_not_returned() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        let id = file(&mut volume, ROOT, "f", &[b'x'; 5000]);
        volume.commit().unwrap();
        let Some(Content::File(Data::Disk { blocks, .. })) =
            volume.nodes[id].as_ref().map(|n| &n.content)
        else {
            panic!("a disk file");
        };
        let second = blocks[1].block as usize;
        let mut disk = volume.device().unwrap().clone();
        disk.image[second * BLOCK_SIZE + 10] ^= 0x40; // bit rot in block 1
        let mut again = reopen(&disk);
        let id = again.lookup(ROOT, "f").unwrap();
        let mut buffer = [0u8; 100];
        assert_eq!(again.read(id, 0, &mut buffer), Ok(100), "block 0 is intact");
        assert_eq!(again.read(id, 4096, &mut buffer), Err(FsError::Corrupt));
        assert_eq!(
            again.write(id, 4100, b"y"),
            Err(FsError::Corrupt),
            "a damaged block is not silently rewritten"
        );
    }

    #[test]
    fn format_1_volumes_are_upgraded() {
        let (mut volume, _) = Volume::open(MemDisk::new(64), true).unwrap();
        volume.format = 1;
        volume.dirty = true;
        volume.commit().unwrap();
        file(&mut volume, ROOT, "old", &[b'o'; 6000]);
        volume.commit().unwrap();
        let disk = volume.device().unwrap().clone();
        let slot = (volume.generation() % 2) as usize * BLOCK_SIZE;
        assert_eq!(u32_at(&disk.image[slot..], 8), 1, "written as format 1");

        let mut upgraded = reopen(&disk);
        assert!(upgraded.is_dirty(), "the upgrade is pending");
        assert_eq!(
            snapshot(&mut upgraded),
            ["/old = ".to_string() + &"o".repeat(6000)]
        );
        upgraded.commit().unwrap();
        let disk = upgraded.device().unwrap().clone();
        let slot = (upgraded.generation() % 2) as usize * BLOCK_SIZE;
        assert_eq!(u32_at(&disk.image[slot..], 8), VERSION, "now format 2");
        let mut again = reopen(&disk);
        assert_eq!(snapshot(&mut again).len(), 1);
    }

    #[test]
    fn duplicate_and_out_of_range_blocks_are_rejected() {
        let mut meta = StdVec::new();
        meta.extend_from_slice(&[KIND_DIRECTORY, 0]);
        meta.extend_from_slice(&2u32.to_le_bytes());
        for name in [b"a", b"b"] {
            meta.extend_from_slice(&[KIND_FILE, 1]);
            meta.extend_from_slice(name);
            meta.extend_from_slice(&10u64.to_le_bytes());
            meta.extend_from_slice(&1u32.to_le_bytes());
            meta.extend_from_slice(&5u32.to_le_bytes());
        }
        let mut volume = Volume::empty(Some(MemDisk::new(64)), 64);
        assert_eq!(volume.parse(&meta, 1), Err("block referenced twice"));
        let mut volume = Volume::empty(Some(MemDisk::new(64)), 64);
        let end = meta.len();
        meta[end - 4..].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(volume.parse(&meta, 1), Err("block number out of range"));
    }
}
