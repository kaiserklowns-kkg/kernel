//! The Oceans filesystem protocol (ADR-0019), shared by the `fs` service
//! and its clients.
//!
//! **A node handle is a capability.** Every open file or directory is a
//! badged client end of the fs endpoint. Holding a directory handle grants
//! that directory and what is below it, nothing else: there is no `..` and
//! no global path. Paths are resolved by the client, one component at a time
//! from a directory it holds ([`Node::walk`]). Each handle also carries
//! read/write access, decided when it is opened and never widened.
//!
//! Requests are IPC calls on the node handle. Labels select the operation;
//! replies carry a [`Status`] label. Data is inline (at most
//! [`MAX_DATA`] bytes per call).
//!
//! Durability (ADR-0022): creating and removing entries is durable when the
//! call returns; file contents when the handle that changed them closes, or
//! at [`op::SYNC`].

#![no_std]

use oceans_rt::{Error, Handle};

/// Operations (request labels).
pub mod op {
    /// data = `[flags u8][name]` → handle to the child + `[kind u8]`.
    pub const OPEN: u64 = 1;
    /// data = `[offset u64][len u32]` → the bytes read.
    pub const READ: u64 = 2;
    /// data = `[offset u64][bytes]` → `[written u32]`.
    pub const WRITE: u64 = 3;
    /// → `[kind u8][size u64][writable u8]`.
    pub const STAT: u64 = 4;
    /// data = `[index u32]` → `[kind u8][name]`, or `NotFound` past the end.
    pub const LIST: u64 = 5;
    /// data = `[name]`: unlink a child (directories must be empty).
    pub const REMOVE: u64 = 6;
    /// data = `[size u64]`.
    pub const TRUNCATE: u64 = 7;
    /// Makes every change so far durable (ADR-0022).
    pub const SYNC: u64 = 8;
}

/// `OPEN` flags.
pub mod flags {
    /// Create a file if the name does not exist.
    pub const CREATE_FILE: u8 = 1 << 0;
    /// Create a directory if the name does not exist.
    pub const CREATE_DIRECTORY: u8 = 1 << 1;
    /// Request write access (granted only if the parent handle has it and
    /// the node is not read-only).
    pub const WRITE: u8 = 1 << 2;
}

/// Largest data payload of one request or reply.
pub const MAX_DATA: usize = 248;
/// Longest name of a directory entry.
pub const MAX_NAME: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    File = 1,
    Directory = 2,
}

impl Kind {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::File),
            2 => Some(Self::Directory),
            _ => None,
        }
    }
}

/// Reply status (reply label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    NotFound = 1,
    Exists = 2,
    NotADirectory = 3,
    IsADirectory = 4,
    NotEmpty = 5,
    /// The handle lacks write access, or the node is read-only.
    PermissionDenied = 6,
    /// Bad name (empty, too long, `.`, `..`, contains `/` or NUL).
    InvalidName = 7,
    /// Quota (file size, node count) reached.
    NoSpace = 8,
    /// Malformed request.
    BadRequest = 9,
    /// The disk failed (ADR-0022).
    IoError = 10,
    /// Data on the disk no longer matches its checksum (ADR-0027).
    Corrupt = 11,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            1 => Self::NotFound,
            2 => Self::Exists,
            3 => Self::NotADirectory,
            4 => Self::IsADirectory,
            5 => Self::NotEmpty,
            6 => Self::PermissionDenied,
            7 => Self::InvalidName,
            8 => Self::NoSpace,
            10 => Self::IoError,
            11 => Self::Corrupt,
            _ => Self::BadRequest,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotFound => "not found",
            Self::Exists => "already exists",
            Self::NotADirectory => "not a directory",
            Self::IsADirectory => "is a directory",
            Self::NotEmpty => "directory not empty",
            Self::PermissionDenied => "permission denied",
            Self::InvalidName => "invalid name",
            Self::NoSpace => "no space",
            Self::BadRequest => "bad request",
            Self::IoError => "I/O error",
            Self::Corrupt => "data corrupted on disk (checksum mismatch)",
        }
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    /// The fs service answered with this status.
    Status(Status),
    /// The IPC itself failed (e.g. the service is gone).
    Ipc(Error),
}

impl FsError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Status(status) => status.message(),
            Self::Ipc(Error::PeerClosed) => "filesystem service unavailable",
            Self::Ipc(_) => "filesystem request failed",
        }
    }
}

/// Metadata of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    pub writable: bool,
}

/// A handle to a file or directory.
#[derive(Debug, PartialEq, Eq)]
pub struct Node(pub Handle);

impl Node {
    fn request(
        &self,
        op: u64,
        data: &[u8],
        reply: &mut [u8],
        handles: &mut [Handle],
    ) -> Result<(usize, usize), FsError> {
        let got =
            oceans_rt::ipc_call_msg(self.0, op, data, &[], reply, handles).map_err(FsError::Ipc)?;
        match Status::from_label(got.label) {
            Status::Ok => Ok((got.data_len, got.handles_len)),
            status => Err(FsError::Status(status)),
        }
    }

    /// Opens child `name` of this directory.
    pub fn open(&self, name: &str, open_flags: u8) -> Result<(Node, Kind), FsError> {
        if !valid_name(name.as_bytes()) {
            return Err(FsError::Status(Status::InvalidName));
        }
        let mut data = [0u8; 1 + MAX_NAME];
        data[0] = open_flags;
        data[1..1 + name.len()].copy_from_slice(name.as_bytes());
        let mut reply = [0u8; 8];
        let mut handles = [Handle(0); 1];
        let (len, count) =
            self.request(op::OPEN, &data[..1 + name.len()], &mut reply, &mut handles)?;
        let kind = (len >= 1).then(|| Kind::from_byte(reply[0])).flatten();
        match (count, kind) {
            (1, Some(kind)) => Ok((Node(handles[0]), kind)),
            (1, None) => {
                let _ = oceans_rt::close(handles[0]);
                Err(FsError::Status(Status::BadRequest))
            }
            _ => Err(FsError::Status(Status::BadRequest)),
        }
    }

    /// Resolves `path` (components separated by `/`, relative to this
    /// directory). Intermediate components must be directories; `flags`
    /// apply to the last one. An empty path reopens nothing: use `self`.
    pub fn walk(&self, path: &str, open_flags: u8) -> Result<(Node, Kind), FsError> {
        let mut components = path.split('/').filter(|c| !c.is_empty()).peekable();
        let Some(first) = components.next() else {
            return Err(FsError::Status(Status::InvalidName));
        };
        let mut flags_now = if components.peek().is_none() {
            open_flags
        } else {
            0
        };
        let (mut node, mut kind) = self.open(first, flags_now)?;
        while let Some(name) = components.next() {
            if kind != Kind::Directory {
                node.close();
                return Err(FsError::Status(Status::NotADirectory));
            }
            flags_now = if components.peek().is_none() {
                open_flags
            } else {
                0
            };
            let next = node.open(name, flags_now);
            node.close();
            (node, kind) = next?;
        }
        Ok((node, kind))
    }

    /// Reads up to `buffer.len()` (at most [`MAX_DATA`]) bytes at `offset`.
    pub fn read(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        let len = buffer.len().min(MAX_DATA) as u32;
        let mut data = [0u8; 12];
        data[..8].copy_from_slice(&offset.to_le_bytes());
        data[8..].copy_from_slice(&len.to_le_bytes());
        self.request(op::READ, &data, buffer, &mut [])
            .map(|(n, _)| n)
    }

    /// Writes up to [`MAX_DATA`] - 8 bytes at `offset`; returns how many.
    pub fn write(&self, offset: u64, bytes: &[u8]) -> Result<usize, FsError> {
        let take = bytes.len().min(MAX_DATA - 8);
        let mut data = [0u8; MAX_DATA];
        data[..8].copy_from_slice(&offset.to_le_bytes());
        data[8..8 + take].copy_from_slice(&bytes[..take]);
        let mut reply = [0u8; 4];
        self.request(op::WRITE, &data[..8 + take], &mut reply, &mut [])?;
        Ok(u32::from_le_bytes(reply) as usize)
    }

    /// Writes all of `bytes` starting at `offset`.
    pub fn write_all(&self, mut offset: u64, mut bytes: &[u8]) -> Result<(), FsError> {
        while !bytes.is_empty() {
            let written = self.write(offset, bytes)?;
            if written == 0 {
                return Err(FsError::Status(Status::NoSpace));
            }
            offset += written as u64;
            bytes = &bytes[written..];
        }
        Ok(())
    }

    pub fn stat(&self) -> Result<Stat, FsError> {
        let mut reply = [0u8; 10];
        let (len, _) = self.request(op::STAT, &[], &mut reply, &mut [])?;
        let kind = Kind::from_byte(reply[0]).filter(|_| len == 10);
        let kind = kind.ok_or(FsError::Status(Status::BadRequest))?;
        let size = u64::from_le_bytes(reply[1..9].try_into().expect("8 bytes"));
        Ok(Stat {
            kind,
            size,
            writable: reply[9] != 0,
        })
    }

    /// Directory entry `index`, its name written into `name`; `None` past
    /// the last entry.
    pub fn entry(
        &self,
        index: u32,
        name: &mut [u8; MAX_NAME],
    ) -> Result<Option<(Kind, usize)>, FsError> {
        let mut reply = [0u8; 1 + MAX_NAME];
        match self.request(op::LIST, &index.to_le_bytes(), &mut reply, &mut []) {
            Ok((len, _)) if len >= 1 => {
                let kind = Kind::from_byte(reply[0]).ok_or(FsError::Status(Status::BadRequest))?;
                name[..len - 1].copy_from_slice(&reply[1..len]);
                Ok(Some((kind, len - 1)))
            }
            Ok(_) => Err(FsError::Status(Status::BadRequest)),
            Err(FsError::Status(Status::NotFound)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Removes child `name` of this directory.
    pub fn remove(&self, name: &str) -> Result<(), FsError> {
        if !valid_name(name.as_bytes()) {
            return Err(FsError::Status(Status::InvalidName));
        }
        self.request(op::REMOVE, name.as_bytes(), &mut [], &mut [])
            .map(drop)
    }

    pub fn truncate(&self, size: u64) -> Result<(), FsError> {
        self.request(op::TRUNCATE, &size.to_le_bytes(), &mut [], &mut [])
            .map(drop)
    }

    /// Makes every change in the filesystem durable now.
    pub fn sync(&self) -> Result<(), FsError> {
        self.request(op::SYNC, &[], &mut [], &mut []).map(drop)
    }

    /// Closes the handle (the service then forgets it).
    pub fn close(self) {
        let _ = oceans_rt::close(self.0);
    }
}
