//! The Oceans block device protocol (ADR-0021), shared by block drivers
//! (`virtio-blk`) and their clients.
//!
//! Data moves through shared memory, not IPC payloads: a client opens a
//! **session** by sending a memory object it has mapped (its transfer
//! buffer) and gets back a session handle, a badged client end of the
//! driver's endpoint. Requests on the session name sectors and an offset in
//! that buffer; the driver copies between the buffer and its own DMA memory,
//! so clients never learn or choose physical addresses. Closing the session
//! handle releases the driver's mapping.
//!
//! Requests are IPC calls; labels select the operation and replies carry a
//! [`Status`] label.

#![no_std]

use oceans_rt::{Error, Handle, prot, rights};

/// Operations (request labels).
pub mod op {
    /// On the driver endpoint or a session: → `[sectors u64][sector size
    /// u32][flags u32]`.
    pub const INFO: u64 = 1;
    /// On the driver endpoint, carrying one memory object (`READ`, `WRITE`,
    /// `MAP`, `TRANSFER`): → a session handle.
    pub const OPEN: u64 = 2;
    /// On a session: data = `[sector u64][count u32][offset u32]`; the
    /// sectors land in the buffer at `offset`.
    pub const READ: u64 = 3;
    /// On a session: same request; the sectors come from the buffer.
    pub const WRITE: u64 = 4;
    /// On a session: makes completed writes durable.
    pub const FLUSH: u64 = 5;
}

/// `INFO` flags.
pub mod info_flags {
    pub const READ_ONLY: u32 = 1 << 0;
}

/// Bytes per sector (logical block) of the protocol.
pub const SECTOR_SIZE: usize = 512;
/// Largest session buffer a driver accepts.
pub const MAX_BUFFER: usize = 1 << 20;

/// Reply status (reply label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    /// Malformed request, or an operation on the wrong handle.
    BadRequest = 1,
    /// Sectors beyond the end of the disk, or beyond the buffer.
    OutOfRange = 2,
    ReadOnly = 3,
    /// The device reported an error.
    IoError = 4,
    /// The device does not support the operation.
    Unsupported = 5,
    /// No more sessions.
    NoSpace = 6,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            2 => Self::OutOfRange,
            3 => Self::ReadOnly,
            4 => Self::IoError,
            5 => Self::Unsupported,
            6 => Self::NoSpace,
            _ => Self::BadRequest,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::BadRequest => "bad request",
            Self::OutOfRange => "out of range",
            Self::ReadOnly => "read-only disk",
            Self::IoError => "I/O error",
            Self::Unsupported => "not supported by the device",
            Self::NoSpace => "too many sessions",
        }
    }
}

/// A disk's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub sectors: u64,
    pub sector_size: u32,
    pub flags: u32,
}

impl Info {
    pub const SIZE: usize = 16;

    pub fn read_only(&self) -> bool {
        self.flags & info_flags::READ_ONLY != 0
    }

    pub fn bytes(&self) -> u64 {
        self.sectors * u64::from(self.sector_size)
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[..8].copy_from_slice(&self.sectors.to_le_bytes());
        out[8..12].copy_from_slice(&self.sector_size.to_le_bytes());
        out[12..].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::SIZE] = bytes.get(..Self::SIZE)?.try_into().ok()?;
        Some(Self {
            sectors: u64::from_le_bytes(bytes[..8].try_into().ok()?),
            sector_size: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            flags: u32::from_le_bytes(bytes[12..].try_into().ok()?),
        })
    }
}

/// A `READ` or `WRITE` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub sector: u64,
    pub count: u32,
    /// Byte offset in the session buffer.
    pub offset: u32,
}

impl Transfer {
    pub const SIZE: usize = 16;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[..8].copy_from_slice(&self.sector.to_le_bytes());
        out[8..12].copy_from_slice(&self.count.to_le_bytes());
        out[12..].copy_from_slice(&self.offset.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        Some(Self {
            sector: u64::from_le_bytes(bytes[..8].try_into().ok()?),
            count: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            offset: u32::from_le_bytes(bytes[12..].try_into().ok()?),
        })
    }

    /// Bytes moved, if the request is non-empty and stays inside both a
    /// disk of `sectors` and a buffer of `buffer` bytes.
    pub fn checked_len(&self, sectors: u64, buffer: usize) -> Option<usize> {
        let end = self.sector.checked_add(u64::from(self.count))?;
        let len = usize::try_from(self.count).ok()?.checked_mul(SECTOR_SIZE)?;
        let buffer_end = usize::try_from(self.offset).ok()?.checked_add(len)?;
        (self.count > 0 && end <= sectors && buffer_end <= buffer).then_some(len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    /// The driver answered with this status.
    Status(Status),
    /// The IPC itself failed (e.g. the driver is gone).
    Ipc(Error),
}

impl BlockError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Status(status) => status.message(),
            Self::Ipc(Error::PeerClosed) => "block service unavailable",
            Self::Ipc(_) => "block request failed",
        }
    }
}

fn request(
    handle: Handle,
    op: u64,
    data: &[u8],
    send: &[Handle],
    reply: &mut [u8],
    reply_handles: &mut [Handle],
) -> Result<(usize, usize), BlockError> {
    let got = oceans_rt::ipc_call_msg(handle, op, data, send, reply, reply_handles)
        .map_err(BlockError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok((got.data_len, got.handles_len)),
        status => Err(BlockError::Status(status)),
    }
}

/// The geometry of the disk behind `handle` (driver endpoint or session).
pub fn info(handle: Handle) -> Result<Info, BlockError> {
    let mut reply = [0u8; Info::SIZE];
    let (len, _) = request(handle, op::INFO, &[], &[], &mut reply, &mut [])?;
    Info::decode(&reply[..len]).ok_or(BlockError::Status(Status::BadRequest))
}

/// A session: a transfer buffer shared with the driver.
pub struct Disk {
    session: Handle,
    buffer: *mut u8,
    size: usize,
    pub info: Info,
}

impl Disk {
    /// Opens a session with a `buffer_size`-byte transfer buffer (a
    /// multiple of [`SECTOR_SIZE`], at most [`MAX_BUFFER`]).
    pub fn open(driver: Handle, buffer_size: usize) -> Result<Self, BlockError> {
        let ipc = BlockError::Ipc;
        let memory = oceans_rt::memory_create(buffer_size as u64).map_err(ipc)?;
        let mapped = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let shared = oceans_rt::duplicate(
            memory,
            rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
        );
        // The mapping keeps the object alive; the driver gets `shared`.
        let _ = oceans_rt::close(memory);
        let buffer = mapped.map_err(ipc)?;
        let fail = |error| {
            let _ = oceans_rt::memory_unmap(buffer);
            error
        };
        let shared = shared.map_err(ipc).map_err(fail)?;
        let mut session = [Handle(0); 1];
        let (_, count) =
            request(driver, op::OPEN, &[], &[shared], &mut [], &mut session).map_err(fail)?;
        if count != 1 {
            return Err(fail(BlockError::Status(Status::BadRequest)));
        }
        let info = info(session[0]).map_err(|error| {
            let _ = oceans_rt::close(session[0]);
            fail(error)
        })?;
        Ok(Self {
            session: session[0],
            buffer,
            size: buffer_size,
            info,
        })
    }

    /// The transfer buffer.
    pub fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: `buffer` maps `size` bytes read-write for as long as
        // `self` lives; the driver only accesses it during our calls.
        unsafe { core::slice::from_raw_parts_mut(self.buffer, self.size) }
    }

    fn transfer(&self, op: u64, sector: u64, count: u32, offset: u32) -> Result<(), BlockError> {
        let data = Transfer {
            sector,
            count,
            offset,
        }
        .encode();
        request(self.session, op, &data, &[], &mut [], &mut []).map(drop)
    }

    /// Reads `count` sectors into the buffer at `offset`.
    pub fn read(&self, sector: u64, count: u32, offset: u32) -> Result<(), BlockError> {
        self.transfer(op::READ, sector, count, offset)
    }

    /// Writes `count` sectors from the buffer at `offset`.
    pub fn write(&self, sector: u64, count: u32, offset: u32) -> Result<(), BlockError> {
        self.transfer(op::WRITE, sector, count, offset)
    }

    /// Whether the session still reaches its disk: drivers of removable
    /// media end the sessions of a disk that goes away (ADR-0035).
    pub fn alive(&self) -> bool {
        info(self.session).is_ok()
    }

    pub fn flush(&self) -> Result<(), BlockError> {
        request(self.session, op::FLUSH, &[], &[], &mut [], &mut []).map(drop)
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.session);
        let _ = oceans_rt::memory_unmap(self.buffer);
    }
}
