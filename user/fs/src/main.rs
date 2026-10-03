//! The Oceans filesystem service (ADR-0019, ADR-0022): speaks
//! `oceans-fs-proto` and keeps files on disk in an OceansFS volume
//! (`oceans-volume`).
//!
//! - Every open node is a badged client end minted by this service; the
//!   badge indexes the open-handle table, which records the node and the
//!   handle's access (read-only or read-write). Unbadged ends, the ones init
//!   hands out with `use = fs`, are read-write handles to the root.
//! - When a handle is closed anywhere, the kernel sends a close event, and
//!   its entry is dropped. A node is freed once it is unlinked and no handle
//!   refers to it.
//! - **Storage.** With `use = block` the volume lives on that disk. A blank
//!   disk is formatted; a disk holding anything else, or a damaged volume,
//!   is left untouched and files stay in memory. Durability:
//!   - creating and removing entries is committed before the reply;
//!   - file contents are committed when the handle that wrote them closes,
//!     or on `SYNC`.
//!
//!   Each commit is atomic, so a crash loses at most uncommitted changes and
//!   never corrupts the volume.
//! - Program images granted with `grant = module:NAME` are published
//!   read-only under `/bin`, in memory only: they come from the boot image.
//!
//! Manifest grants: `log`, `provide = fs`, `use = block` (optional), any
//! `module:NAME`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_block_proto::{Disk, SECTOR_SIZE};
use oceans_fs_proto::{Kind, MAX_DATA, MAX_NAME, MAX_SHARED, MIN_SHARED, Status, flags, op};
use oceans_rt::{Buffer, Directory, Error, Handle, Start, prot};
use oceans_volume::{
    BLOCK_SIZE, BlockBuf, BlockDevice, FsError, IoError, MountError, NodeId, Opened, ROOT, Volume,
};

oceans_rt::entry!(main);

const SECTORS_PER_BLOCK: u32 = (BLOCK_SIZE / SECTOR_SIZE) as u32;

/// The volume's disk: a block-service session with a one-block buffer.
struct DiskDevice {
    disk: Disk,
    blocks: u64,
}

impl BlockDevice for DiskDevice {
    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_block(&mut self, block: u64, out: &mut BlockBuf) -> Result<(), IoError> {
        let sector = block * u64::from(SECTORS_PER_BLOCK);
        self.disk
            .read(sector, SECTORS_PER_BLOCK, 0)
            .map_err(|_| IoError)?;
        out.copy_from_slice(&self.disk.buffer()[..BLOCK_SIZE]);
        Ok(())
    }

    fn write_block(&mut self, block: u64, data: &BlockBuf) -> Result<(), IoError> {
        self.disk.buffer()[..BLOCK_SIZE].copy_from_slice(data);
        let sector = block * u64::from(SECTORS_PER_BLOCK);
        self.disk
            .write(sector, SECTORS_PER_BLOCK, 0)
            .map_err(|_| IoError)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.disk.flush().map_err(|_| IoError)
    }
}

#[derive(Clone, Copy)]
struct Open {
    node: NodeId,
    writable: bool,
}

struct Fs {
    server: Handle,
    log: Handle,
    volume: Volume<DiskDevice>,
    handles: BTreeMap<u64, Open>,
    /// Shared buffers of open handles (ADR-0030), mapped here.
    buffers: BTreeMap<u64, (*mut u8, usize)>,
    next_badge: u64,
}

/// A reply: status, data, and at most one handle to move to the caller.
struct Reply {
    status: Status,
    data: [u8; MAX_DATA],
    len: usize,
    handle: Option<Handle>,
}

impl Reply {
    fn status(status: Status) -> Self {
        Self {
            status,
            data: [0; MAX_DATA],
            len: 0,
            handle: None,
        }
    }

    fn ok(bytes: &[u8]) -> Self {
        let mut reply = Self::status(Status::Ok);
        reply.data[..bytes.len()].copy_from_slice(bytes);
        reply.len = bytes.len();
        reply
    }
}

fn status(error: FsError) -> Status {
    match error {
        FsError::NotFound => Status::NotFound,
        FsError::Exists => Status::Exists,
        FsError::NotADirectory => Status::NotADirectory,
        FsError::IsADirectory => Status::IsADirectory,
        FsError::NotEmpty => Status::NotEmpty,
        FsError::ReadOnly => Status::PermissionDenied,
        FsError::InvalidName => Status::InvalidName,
        FsError::NoSpace => Status::NoSpace,
        FsError::Io => Status::IoError,
        FsError::Corrupt => Status::Corrupt,
    }
}

fn kind(kind: oceans_volume::Kind) -> Kind {
    match kind {
        oceans_volume::Kind::File => Kind::File,
        oceans_volume::Kind::Directory => Kind::Directory,
    }
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_str("fs: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(server)) = (directory.find("log", "log"), directory.find_kind("provide"))
    else {
        return 3;
    };

    let volume = mount(log, directory.find("use", "block"));
    let mut fs = Fs {
        server,
        log,
        volume,
        handles: BTreeMap::new(),
        buffers: BTreeMap::new(),
        next_badge: 1,
    };
    let published = fs.publish_programs(&directory);
    say(log, format_args!("ready, {published} programs in /bin"));

    let mut request = [0u8; 256];
    let mut received_handles = [Handle(0); 4];
    loop {
        let got = match oceans_rt::ipc_receive_msg(server, &mut request, &mut received_handles) {
            Ok(got) => got,
            // Every client end is gone (init keeps one, so: shutdown).
            Err(Error::PeerClosed) => {
                let _ = fs.commit("shutdown");
                return 0;
            }
            Err(_) => return 4,
        };
        if got.closed {
            fs.close_handle(got.badge);
            continue;
        }
        let received = &received_handles[..got.handles_len];
        let reply = if got.label == op::ATTACH {
            fs.attach(got.badge, received)
        } else {
            fs.handle(got.badge, got.label, &request[..got.data_len])
        };
        // Capabilities sent with requests are never kept (a shared buffer
        // stays mapped without its handle).
        for &handle in received {
            let _ = oceans_rt::close(handle);
        }
        let handles = reply.handle.as_slice();
        if oceans_rt::ipc_reply_msg(reply.status as u64, &reply.data[..reply.len], handles).is_err()
        {
            // The caller vanished; its new handle (if any) is closed with it.
            if let Some(handle) = reply.handle {
                let _ = oceans_rt::close(handle);
            }
        }
    }
}

/// The volume on the granted disk, or an in-memory one (logged why).
fn mount(log: Handle, block: Option<Handle>) -> Volume<DiskDevice> {
    let Some(block) = block else {
        say(
            log,
            format_args!("no disk granted; files are kept in memory only"),
        );
        return Volume::memory();
    };
    let disk = match Disk::open(block, BLOCK_SIZE) {
        Ok(disk) => disk,
        Err(error) => {
            say(
                log,
                format_args!(
                    "disk unavailable ({}); files are kept in memory only",
                    error.message()
                ),
            );
            return Volume::memory();
        }
    };
    let info = disk.info;
    if info.read_only() || info.sector_size as usize != SECTOR_SIZE {
        say(
            log,
            format_args!("disk is read-only or has unusual sectors; files are kept in memory only"),
        );
        return Volume::memory();
    }
    let device = DiskDevice {
        disk,
        blocks: info.sectors / u64::from(SECTORS_PER_BLOCK),
    };
    match Volume::open(device, true) {
        Ok((volume, opened)) => {
            let (used, total) = volume.usage();
            let kib = |blocks: u64| blocks * BLOCK_SIZE as u64 / 1024;
            match opened {
                Opened::Formatted => say(
                    log,
                    format_args!("formatted a blank disk: {} KiB", kib(total)),
                ),
                Opened::Mounted => say(
                    log,
                    format_args!(
                        "mounted the disk: generation {}, {} of {} KiB used",
                        volume.generation(),
                        kib(used),
                        kib(total)
                    ),
                ),
            }
            volume
        }
        Err(error) => {
            let why = match error {
                MountError::UnknownContents => "it holds something other than an Oceans volume",
                MountError::Corrupt(why) => why,
                MountError::TooSmall => "it is too small",
                MountError::Io | MountError::Blank => "it cannot be read",
            };
            say(
                log,
                format_args!(
                    "disk not mounted ({why}), left untouched; files are kept in memory only"
                ),
            );
            Volume::memory()
        }
    }
}

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// A copy of a memory object's contents.
fn read_memory(memory: Handle) -> Option<Vec<u8>> {
    let size = usize::try_from(oceans_rt::memory_size(memory).ok()?).ok()?;
    let base = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: the whole object is mapped readable at `base`.
    let bytes = unsafe { core::slice::from_raw_parts(base, size) }.to_vec();
    let _ = oceans_rt::memory_unmap(base);
    Some(bytes)
}

impl Fs {
    /// `/bin`: the granted program images, read-only and in memory only.
    fn publish_programs(&mut self, directory: &Directory) -> usize {
        let bin = match self.volume.create_volatile_directory(ROOT, "bin", true) {
            Ok(bin) => bin,
            Err(error) => {
                say(self.log, format_args!("cannot create /bin: {error:?}"));
                return 0;
            }
        };
        let mut published = 0;
        for line in directory.lines() {
            let mut words = line.split_whitespace();
            let (Some(_), Some("module"), Some(name)) = (words.next(), words.next(), words.next())
            else {
                continue;
            };
            let Some(memory) = directory.find("module", name) else {
                continue;
            };
            if let Some(bytes) = read_memory(memory)
                && self.volume.publish(bin, name, bytes).is_ok()
            {
                published += 1;
            }
            let _ = oceans_rt::close(memory);
        }
        published
    }

    /// Commits, logging (not failing) on error: the changes stay pending
    /// and the next commit retries them.
    fn commit(&mut self, why: &str) -> Result<(), FsError> {
        let result = self.volume.commit();
        if let Err(error) = result {
            say(
                self.log,
                format_args!("commit after {why} failed: {error:?}"),
            );
        }
        result
    }

    fn close_handle(&mut self, badge: u64) {
        if let Some((base, _)) = self.buffers.remove(&badge) {
            let _ = oceans_rt::memory_unmap(base);
        }
        if let Some(open) = self.handles.remove(&badge) {
            self.volume.release(open.node);
            if open.writable {
                let _ = self.commit("close");
            }
        }
    }

    fn handle(&mut self, badge: u64, operation: u64, data: &[u8]) -> Reply {
        let open = if badge == 0 {
            Open {
                node: ROOT,
                writable: true,
            }
        } else {
            match self.handles.get(&badge) {
                Some(&open) => open,
                None => return Reply::status(Status::BadRequest),
            }
        };
        let result = match operation {
            op::OPEN => self.open(open, data),
            op::READ => self.read(open, data),
            op::WRITE => self.write(open, data),
            op::STAT => self.stat(open),
            op::LIST => self.list(open, data),
            op::REMOVE => self.remove(open, data),
            op::TRUNCATE => self.truncate(open, data),
            op::SYNC => self.commit("sync").map(|()| Reply::ok(&[])).map_err(status),
            op::WRITE_BUF | op::READ_BUF => self.bulk(badge, open, operation, data),
            _ => Err(Status::BadRequest),
        };
        result.unwrap_or_else(Reply::status)
    }

    /// Maps the shared buffer sent for an opened handle (ADR-0030).
    fn attach(&mut self, badge: u64, received: &[Handle]) -> Reply {
        if badge == 0 || !self.handles.contains_key(&badge) || received.len() != 1 {
            return Reply::status(Status::BadRequest);
        }
        let Ok(size) = oceans_rt::memory_size(received[0]).map(|s| s as usize) else {
            return Reply::status(Status::BadRequest);
        };
        if !(MIN_SHARED..=MAX_SHARED).contains(&size) {
            return Reply::status(Status::BadRequest);
        }
        let Ok(base) = oceans_rt::memory_map(received[0], 0, prot::READ | prot::WRITE) else {
            return Reply::status(Status::BadRequest);
        };
        if let Some((old, _)) = self.buffers.insert(badge, (base, size)) {
            let _ = oceans_rt::memory_unmap(old);
        }
        Reply::ok(&[])
    }

    /// `WRITE_BUF` / `READ_BUF`: file data through the shared buffer.
    fn bulk(
        &mut self,
        badge: u64,
        open: Open,
        operation: u64,
        data: &[u8],
    ) -> Result<Reply, Status> {
        let &(base, size) = self.buffers.get(&badge).ok_or(Status::BadRequest)?;
        let offset = u64_at(data, 0).ok_or(Status::BadRequest)?;
        let at = u32_at(data, 8).ok_or(Status::BadRequest)? as usize;
        let len = u32_at(data, 12).ok_or(Status::BadRequest)? as usize;
        if at.checked_add(len).is_none_or(|end| end > size) {
            return Err(Status::BadRequest);
        }
        // SAFETY: `at..at + len` lies inside the handle's shared buffer
        // (checked), mapped read-write here; the client waits in its call.
        let window = unsafe { core::slice::from_raw_parts_mut(base.add(at), len) };
        let moved = if operation == op::WRITE_BUF {
            if !open.writable {
                return Err(Status::PermissionDenied);
            }
            self.volume
                .write(open.node, offset, window)
                .map_err(status)?
        } else {
            self.volume
                .read(open.node, offset, window)
                .map_err(status)?
        };
        Ok(Reply::ok(&(moved as u32).to_le_bytes()))
    }

    fn name(data: &[u8]) -> Result<&str, Status> {
        if !oceans_volume::valid_name(data) {
            return Err(Status::InvalidName);
        }
        core::str::from_utf8(data).map_err(|_| Status::InvalidName)
    }

    fn open(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let (&open_flags, name) = data.split_first().ok_or(Status::BadRequest)?;
        let name = Self::name(name)?;
        let index = match self.volume.lookup(open.node, name) {
            Ok(index) => index,
            Err(FsError::NotFound) => {
                let new_kind = if open_flags & flags::CREATE_DIRECTORY != 0 {
                    oceans_volume::Kind::Directory
                } else if open_flags & flags::CREATE_FILE != 0 {
                    oceans_volume::Kind::File
                } else {
                    return Err(Status::NotFound);
                };
                if !open.writable {
                    return Err(Status::PermissionDenied);
                }
                let index = self
                    .volume
                    .create(open.node, name, new_kind)
                    .map_err(status)?;
                // The new entry is durable before anyone is told it exists.
                let _ = self.commit("create");
                index
            }
            Err(error) => return Err(status(error)),
        };
        // Write access is granted only through a writable parent handle and
        // to a writable node; never silently downgraded.
        let writable = open_flags & flags::WRITE != 0;
        if writable && (!open.writable || self.volume.is_read_only(index).map_err(status)?) {
            return Err(Status::PermissionDenied);
        }
        let badge = self.next_badge;
        let handle = oceans_rt::endpoint_mint(self.server, badge).map_err(|_| Status::NoSpace)?;
        self.next_badge += 1;
        self.handles.insert(
            badge,
            Open {
                node: index,
                writable,
            },
        );
        self.volume.retain(index).map_err(status)?;
        let node_kind = self.volume.kind(index).map_err(status)?;
        let mut reply = Reply::ok(&[kind(node_kind) as u8]);
        reply.handle = Some(handle);
        Ok(reply)
    }

    fn read(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let offset = u64_at(data, 0).ok_or(Status::BadRequest)?;
        let len = u32_at(data, 8).ok_or(Status::BadRequest)? as usize;
        let mut reply = Reply::status(Status::Ok);
        reply.len = self
            .volume
            .read(open.node, offset, &mut reply.data[..len.min(MAX_DATA)])
            .map_err(status)?;
        Ok(reply)
    }

    fn write(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let offset = u64_at(data, 0).ok_or(Status::BadRequest)?;
        if !open.writable {
            return Err(Status::PermissionDenied);
        }
        let written = self
            .volume
            .write(open.node, offset, &data[8..])
            .map_err(status)?;
        Ok(Reply::ok(&(written as u32).to_le_bytes()))
    }

    fn stat(&self, open: Open) -> Result<Reply, Status> {
        let node_kind = self.volume.kind(open.node).map_err(status)?;
        let size = self.volume.size(open.node).map_err(status)?;
        let mut data = [0u8; 10];
        data[0] = kind(node_kind) as u8;
        data[1..9].copy_from_slice(&size.to_le_bytes());
        data[9] = u8::from(open.writable);
        Ok(Reply::ok(&data))
    }

    fn list(&self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let index = u32_at(data, 0).ok_or(Status::BadRequest)? as usize;
        let (name, node_kind) = self.volume.entry(open.node, index).map_err(status)?;
        let mut entry = [0u8; 1 + MAX_NAME];
        entry[0] = kind(node_kind) as u8;
        entry[1..1 + name.len()].copy_from_slice(name.as_bytes());
        Ok(Reply::ok(&entry[..1 + name.len()]))
    }

    fn remove(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let name = Self::name(data)?;
        if !open.writable {
            return Err(Status::PermissionDenied);
        }
        self.volume.remove(open.node, name).map_err(status)?;
        let _ = self.commit("remove");
        Ok(Reply::ok(&[]))
    }

    fn truncate(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let size = u64_at(data, 0).ok_or(Status::BadRequest)?;
        if !open.writable {
            return Err(Status::PermissionDenied);
        }
        self.volume.truncate(open.node, size).map_err(status)?;
        Ok(Reply::ok(&[]))
    }
}
