//! NVMe for Oceans (ADR-0040): the parts of the NVM Express base
//! specification a driver needs, as plain data, without allocation.
//!
//! - [`Capabilities`] decodes the controller's `CAP` register; [`reg`],
//!   [`cc`] and [`csts`] name the registers and bits used to bring a
//!   controller up.
//! - [`Command`] builds the 64-byte submission queue entries the driver
//!   issues, [`Completion`] decodes the 16-byte completion entries.
//! - [`ControllerInfo`] and [`NamespaceInfo`] parse `IDENTIFY` data.
//! - [`prp`] says how a transfer from a contiguous buffer is described,
//!   and [`next_chunk`] splits a request in 512-byte sectors into
//!   transfers of the namespace's logical blocks.
//!
//! Everything a device reports is bounds-checked here: a controller is not
//! trusted to keep within the sizes it announced.

#![no_std]

#[cfg(test)]
mod tests;

/// The memory page size the driver programs (`CC.MPS = 0`).
pub const PAGE_SIZE: usize = 4096;
/// Bytes per submission queue entry (`CC.IOSQES = 6`).
pub const COMMAND_SIZE: usize = 64;
/// Bytes per completion queue entry (`CC.IOCQES = 4`).
pub const COMPLETION_SIZE: usize = 16;
/// Bytes of `IDENTIFY` data.
pub const IDENTIFY_SIZE: usize = 4096;
/// The sector size of the Oceans block protocol.
pub const SECTOR_SIZE: u32 = 512;

/// Controller register offsets (BAR 0).
pub mod reg {
    pub const CAP: usize = 0x00;
    pub const VS: usize = 0x08;
    pub const INTMS: usize = 0x0c;
    pub const CC: usize = 0x14;
    pub const CSTS: usize = 0x1c;
    pub const AQA: usize = 0x24;
    pub const ASQ: usize = 0x28;
    pub const ACQ: usize = 0x30;
    /// The first doorbell.
    pub const DOORBELLS: usize = 0x1000;
}

/// Controller configuration (`CC`) bits.
pub mod cc {
    pub const ENABLE: u32 = 1 << 0;
    /// The NVM command set (`CSS = 0`), 4 KiB pages (`MPS = 0`), round
    /// robin arbitration (`AMS = 0`) are all zero.
    pub const NVM_COMMAND_SET: u32 = 0;
    pub const SHUTDOWN_NORMAL: u32 = 1 << 14;
    pub const SHUTDOWN_MASK: u32 = 3 << 14;
    /// 64-byte submission entries (2^6).
    pub const IOSQES: u32 = 6 << 16;
    /// 16-byte completion entries (2^4).
    pub const IOCQES: u32 = 4 << 20;
}

/// Controller status (`CSTS`) bits.
pub mod csts {
    pub const READY: u32 = 1 << 0;
    pub const FATAL: u32 = 1 << 1;
    pub const SHUTDOWN_MASK: u32 = 3 << 2;
    pub const SHUTDOWN_COMPLETE: u32 = 2 << 2;
}

/// Command opcodes.
pub mod opcode {
    // Admin commands.
    pub const CREATE_IO_SQ: u8 = 0x01;
    pub const CREATE_IO_CQ: u8 = 0x05;
    pub const IDENTIFY: u8 = 0x06;
    pub const SET_FEATURES: u8 = 0x09;
    // NVM I/O commands.
    pub const FLUSH: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const READ: u8 = 0x02;
}

/// `IDENTIFY` data structures (`CNS`).
pub mod cns {
    pub const NAMESPACE: u8 = 0x00;
    pub const CONTROLLER: u8 = 0x01;
    pub const ACTIVE_NAMESPACES: u8 = 0x02;
}

/// The feature `SET_FEATURES` sets for queue counts.
const FEATURE_QUEUES: u32 = 0x07;

/// What the `CAP` register says about the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Most entries an I/O queue may have.
    pub max_queue_entries: u32,
    /// Bytes between doorbells.
    pub doorbell_stride: usize,
    /// Longest the controller may take to become ready, or not ready.
    pub timeout_ms: u64,
    /// Whether it implements the NVM command set.
    pub nvm_command_set: bool,
    /// Smallest and largest memory page size (log2 of bytes).
    pub min_page_shift: u32,
    pub max_page_shift: u32,
}

impl Capabilities {
    pub fn decode(cap: u64) -> Self {
        Self {
            max_queue_entries: (cap & 0xffff) as u32 + 1,
            timeout_ms: ((cap >> 24) & 0xff) * 500,
            doorbell_stride: 4 << ((cap >> 32) & 0xf),
            nvm_command_set: cap & (1 << 37) != 0,
            min_page_shift: 12 + ((cap >> 48) & 0xf) as u32,
            max_page_shift: 12 + ((cap >> 52) & 0xf) as u32,
        }
    }

    /// Whether 4 KiB memory pages (the only size used here) are allowed.
    pub fn supports_4k_pages(&self) -> bool {
        self.min_page_shift <= 12 && self.max_page_shift >= 12
    }

    /// The submission queue tail doorbell of queue `queue`.
    pub fn submission_doorbell(&self, queue: u16) -> usize {
        reg::DOORBELLS + 2 * usize::from(queue) * self.doorbell_stride
    }

    /// The completion queue head doorbell of queue `queue`.
    pub fn completion_doorbell(&self, queue: u16) -> usize {
        reg::DOORBELLS + (2 * usize::from(queue) + 1) * self.doorbell_stride
    }
}

/// The `VS` register as (major, minor).
pub fn version(vs: u32) -> (u16, u8) {
    ((vs >> 16) as u16, (vs >> 8) as u8)
}

/// A submission queue entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    /// Chosen by the driver, echoed in the completion.
    pub id: u16,
    pub namespace: u32,
    pub prp1: u64,
    pub prp2: u64,
    /// Command dwords 10 to 15.
    pub dwords: [u32; 6],
}

impl Command {
    pub fn encode(&self) -> [u8; COMMAND_SIZE] {
        let mut out = [0u8; COMMAND_SIZE];
        let dword0 = u32::from(self.opcode) | (u32::from(self.id) << 16);
        out[0..4].copy_from_slice(&dword0.to_le_bytes());
        out[4..8].copy_from_slice(&self.namespace.to_le_bytes());
        out[24..32].copy_from_slice(&self.prp1.to_le_bytes());
        out[32..40].copy_from_slice(&self.prp2.to_le_bytes());
        for (i, dword) in self.dwords.iter().enumerate() {
            out[40 + 4 * i..44 + 4 * i].copy_from_slice(&dword.to_le_bytes());
        }
        out
    }

    /// `IDENTIFY` into the page at `buffer`.
    pub fn identify(cns: u8, namespace: u32, buffer: u64) -> Self {
        Self {
            opcode: opcode::IDENTIFY,
            namespace,
            prp1: buffer,
            dwords: [u32::from(cns), 0, 0, 0, 0, 0],
            ..Self::default()
        }
    }

    /// Asks for `queues` (at least 1) I/O submission and completion queues.
    pub fn set_queue_count(queues: u16) -> Self {
        let count = u32::from(queues.max(1) - 1);
        Self {
            opcode: opcode::SET_FEATURES,
            dwords: [FEATURE_QUEUES, (count << 16) | count, 0, 0, 0, 0],
            ..Self::default()
        }
    }

    /// Creates I/O completion queue `id` of `entries` entries in the
    /// physically contiguous memory at `buffer`, interrupting on MSI-X
    /// `vector` if given.
    pub fn create_completion_queue(
        id: u16,
        entries: u16,
        buffer: u64,
        vector: Option<u16>,
    ) -> Self {
        let interrupts = match vector {
            Some(vector) => (u32::from(vector) << 16) | 1 << 1,
            None => 0,
        };
        Self {
            opcode: opcode::CREATE_IO_CQ,
            prp1: buffer,
            // Physically contiguous (bit 0).
            dwords: [queue_size(id, entries), interrupts | 1, 0, 0, 0, 0],
            ..Self::default()
        }
    }

    /// Creates I/O submission queue `id` feeding completion queue
    /// `completion_queue`.
    pub fn create_submission_queue(
        id: u16,
        entries: u16,
        buffer: u64,
        completion_queue: u16,
    ) -> Self {
        Self {
            opcode: opcode::CREATE_IO_SQ,
            prp1: buffer,
            dwords: [
                queue_size(id, entries),
                (u32::from(completion_queue) << 16) | 1,
                0,
                0,
                0,
                0,
            ],
            ..Self::default()
        }
    }

    /// Reads `blocks` (1 to 65 536) logical blocks from `lba`.
    pub fn read(namespace: u32, lba: u64, blocks: u32, prp: (u64, u64)) -> Self {
        Self::transfer(opcode::READ, namespace, lba, blocks, prp)
    }

    /// Writes `blocks` (1 to 65 536) logical blocks at `lba`.
    pub fn write(namespace: u32, lba: u64, blocks: u32, prp: (u64, u64)) -> Self {
        Self::transfer(opcode::WRITE, namespace, lba, blocks, prp)
    }

    /// Makes completed writes durable (with a volatile write cache).
    pub fn flush(namespace: u32) -> Self {
        Self {
            opcode: opcode::FLUSH,
            namespace,
            ..Self::default()
        }
    }

    fn transfer(opcode: u8, namespace: u32, lba: u64, blocks: u32, prp: (u64, u64)) -> Self {
        debug_assert!((1..=1 << 16).contains(&blocks));
        Self {
            opcode,
            namespace,
            prp1: prp.0,
            prp2: prp.1,
            // The block count is zero-based.
            dwords: [lba as u32, (lba >> 32) as u32, blocks - 1, 0, 0, 0],
            ..Self::default()
        }
    }
}

/// `CDW10` of the queue creation commands: zero-based size, then the id.
fn queue_size(id: u16, entries: u16) -> u32 {
    (u32::from(entries.max(2) - 1) << 16) | u32::from(id)
}

/// The status field of a completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusCode(pub u16);

impl StatusCode {
    pub fn is_success(self) -> bool {
        self.code_type() == 0 && self.code() == 0
    }

    /// Status code type: 0 generic, 1 command specific, 2 media errors.
    pub fn code_type(self) -> u8 {
        ((self.0 >> 8) & 0x7) as u8
    }

    pub fn code(self) -> u8 {
        self.0 as u8
    }

    /// The controller says a retry would fail too.
    pub fn do_not_retry(self) -> bool {
        self.0 & (1 << 14) != 0
    }

    pub fn message(self) -> &'static str {
        match (self.code_type(), self.code()) {
            (0, 0x00) => "success",
            (0, 0x01) => "invalid command opcode",
            (0, 0x02) => "invalid field in command",
            (0, 0x04) => "data transfer error",
            (0, 0x05) => "aborted: power loss",
            (0, 0x06) => "internal error",
            (0, 0x0b) => "invalid namespace",
            (0, 0x20) => "namespace is write protected",
            (0, 0x80) => "LBA out of range",
            (0, 0x81) => "capacity exceeded",
            (0, 0x82) => "namespace not ready",
            (1, 0x01) => "invalid queue identifier",
            (1, 0x02) => "invalid queue size",
            (1, 0x08) => "invalid interrupt vector",
            (2, 0x80) => "write fault",
            (2, 0x81) => "unrecovered read error",
            (2, 0x86) => "access denied",
            (2, _) => "media error",
            _ => "command failed",
        }
    }
}

/// A completion queue entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Command specific result (dword 0).
    pub result: u32,
    pub sq_head: u16,
    pub sq_id: u16,
    pub id: u16,
    /// The phase tag: flips each time the controller wraps the queue.
    pub phase: bool,
    pub status: StatusCode,
}

impl Completion {
    pub fn decode(entry: &[u8; COMPLETION_SIZE]) -> Self {
        let dword = |i: usize| u32::from_le_bytes(entry[4 * i..4 * i + 4].try_into().unwrap());
        let (dword2, dword3) = (dword(2), dword(3));
        Self {
            result: dword(0),
            sq_head: dword2 as u16,
            sq_id: (dword2 >> 16) as u16,
            id: dword3 as u16,
            phase: dword3 & (1 << 16) != 0,
            status: StatusCode((dword3 >> 17) as u16),
        }
    }
}

/// From `IDENTIFY` controller data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    serial: [u8; 20],
    model: [u8; 40],
    firmware: [u8; 8],
    /// Largest transfer as a power of two of the minimum page size (0:
    /// no limit).
    pub max_transfer_shift: u8,
    /// Highest namespace identifier.
    pub namespaces: u32,
    /// Writes may sit in a volatile cache until a flush.
    pub volatile_write_cache: bool,
}

impl ControllerInfo {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < IDENTIFY_SIZE {
            return None;
        }
        Some(Self {
            serial: data[4..24].try_into().ok()?,
            model: data[24..64].try_into().ok()?,
            firmware: data[64..72].try_into().ok()?,
            max_transfer_shift: data[77],
            namespaces: u32::from_le_bytes(data[516..520].try_into().ok()?),
            volatile_write_cache: data[525] & 1 != 0,
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

    /// The largest transfer in bytes, given the minimum page size; `None`
    /// for no limit.
    pub fn max_transfer(&self, min_page_shift: u32) -> Option<u64> {
        let shift = u32::from(self.max_transfer_shift) + min_page_shift;
        (self.max_transfer_shift != 0).then(|| 1u64.checked_shl(shift).unwrap_or(u64::MAX))
    }
}

/// An ASCII identify field without its padding (spaces or NULs); `?` if it
/// is not printable ASCII.
fn text(field: &[u8]) -> &str {
    let end = field
        .iter()
        .rposition(|&b| b != b' ' && b != 0)
        .map_or(0, |i| i + 1);
    let field = &field[..end];
    if field.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        core::str::from_utf8(field).unwrap_or("?")
    } else {
        "?"
    }
}

/// From `IDENTIFY` namespace data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceInfo {
    /// Size in logical blocks.
    pub blocks: u64,
    /// Bytes per logical block in the current format.
    pub block_size: u32,
    /// Metadata bytes per block in the current format.
    pub metadata_size: u16,
    pub write_protected: bool,
}

impl NamespaceInfo {
    /// `None` if the data is short or names a format that does not exist.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < IDENTIFY_SIZE {
            return None;
        }
        let formats = usize::from(data[25]) + 1;
        let index = usize::from(data[26] & 0xf) | (usize::from(data[26] >> 5 & 0x3) << 4);
        if index >= formats || index >= 64 {
            return None;
        }
        let format = &data[128 + 4 * index..132 + 4 * index];
        let shift = u32::from(format[2]);
        Some(Self {
            blocks: u64::from_le_bytes(data[0..8].try_into().ok()?),
            block_size: 1u32.checked_shl(shift).filter(|_| shift >= 9)?,
            metadata_size: u16::from_le_bytes([format[0], format[1]]),
            write_protected: data[99] & 1 != 0,
        })
    }
}

/// The first namespace in an active namespace list (`CNS 2`).
pub fn first_namespace(list: &[u8]) -> Option<u32> {
    let (ids, _) = list.as_chunks::<4>();
    ids.first()
        .map(|&id| u32::from_le_bytes(id))
        .filter(|&id| id != 0)
}

/// The PRP entries for `len` bytes of the page-aligned, physically
/// contiguous buffer at `address`. Beyond two pages the second entry is
/// `list`: a page holding the addresses of the buffer's pages after the
/// first ([`prp_list_entry`]).
pub fn prp(address: u64, len: usize, list: u64) -> (u64, u64) {
    match len.div_ceil(PAGE_SIZE) {
        0 | 1 => (address, 0),
        2 => (address, address + PAGE_SIZE as u64),
        _ => (address, list),
    }
}

/// Entry `index` of the PRP list of the buffer at `address`.
pub fn prp_list_entry(address: u64, index: usize) -> u64 {
    address + ((index + 1) * PAGE_SIZE) as u64
}

/// One device transfer serving part of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// The logical blocks transferred.
    pub lba: u64,
    pub blocks: u32,
    /// Where the request's bytes are in those blocks, and how many.
    pub offset: usize,
    pub len: usize,
    /// The blocks hold bytes outside the request: a write must read them
    /// first.
    pub partial: bool,
}

/// The next transfer of a request for disk bytes `position..end`, in
/// logical blocks of `block_size` bytes, at most `max_blocks` (at least 1)
/// at a time. Requests come in 512-byte sectors; a namespace formatted
/// with larger blocks is served by whole blocks around them.
pub fn next_chunk(position: u64, end: u64, block_size: u32, max_blocks: u32) -> Chunk {
    debug_assert!(position < end && max_blocks >= 1 && block_size >= SECTOR_SIZE);
    let size = u64::from(block_size);
    let lba = position / size;
    let start = lba * size;
    let stop = end.min(start + u64::from(max_blocks) * size);
    Chunk {
        lba,
        blocks: (stop - start).div_ceil(size) as u32,
        offset: (position - start) as usize,
        len: (stop - position) as usize,
        partial: position != start || !stop.is_multiple_of(size),
    }
}

/// Queue bookkeeping: a submission queue's tail, or a completion queue's
/// head and expected phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    pub entries: u16,
    pub index: u16,
    /// For completion queues: the phase tag of new entries.
    pub phase: bool,
}

impl Ring {
    pub fn new(entries: u16) -> Self {
        Self {
            entries,
            index: 0,
            phase: true,
        }
    }

    /// Moves to the next slot; the phase flips on wrapping.
    pub fn advance(&mut self) {
        self.index += 1;
        if self.index == self.entries {
            self.index = 0;
            self.phase = !self.phase;
        }
    }
}
