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
//! - **Removable media** (ADR-0035). With `use = X as media` instead of
//!   `block`, the service serves a disk that may come and go (a USB stick):
//!   it mounts it when a request arrives (formatting a blank one, leaving
//!   any other contents untouched), checks before each request that the
//!   disk is still there, and unmounts when it is gone. A disk holding a
//!   FAT volume (ADR-0036) is served read-only. Handles opened on
//!   a removed disk answer `NoMedium`; nothing is ever written to a disk
//!   other than the one the volume was read from.
//! - **Mount points** (ADR-0035). Each `use = X as mount:NAME` grant is
//!   another filesystem service shown as the directory `/NAME`: requests
//!   on handles below it are forwarded there and the replies passed back,
//!   so every client sees one tree. `SYNC` reaches the mounts too.
//!
//! Manifest grants: `log`, `provide = fs`, `use = block` or `use = X as
//! media` (optional), `use = X as mount:NAME` (any), any `module:NAME`.

#![no_std]
#![no_main]

extern crate alloc;

mod store;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use oceans_block_proto::{Disk, SECTOR_SIZE};
use oceans_fat::Fat;
use oceans_fs_proto::{Kind, MAX_DATA, MAX_NAME, MAX_SHARED, MIN_SHARED, Status, flags, op};
use oceans_rt::{Buffer, Directory, Error, Handle, Start, prot, rights};
use oceans_volume::{
    BLOCK_SIZE, BlockBuf, BlockDevice, FsError, IoError, MountError, NodeId, Opened, ROOT, Volume,
};
use store::{FatDisk, Store};

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

/// A handle below a mount point: the mounted service's handle for it.
#[derive(Clone, Copy)]
struct Remote {
    handle: Handle,
    writable: bool,
}

struct Fs {
    server: Handle,
    log: Handle,
    volume: Store,
    handles: BTreeMap<u64, Open>,
    /// Shared buffers of open handles (ADR-0030), mapped here.
    buffers: BTreeMap<u64, (*mut u8, usize)>,
    next_badge: u64,
    /// Removable media: the block service, and whether a volume from it is
    /// mounted (otherwise `volume` is an empty stand-in, never served).
    media: Option<Handle>,
    mounted: bool,
    /// A refusal (foreign contents) is logged once per disk.
    refusal_logged: bool,
    /// Filesystems shown as `/NAME`, by name.
    mounts: Vec<(&'static str, Handle)>,
    remote: BTreeMap<u64, Remote>,
}

/// A reply: status, data, and at most one handle to move to the caller.
struct Reply {
    status: Status,
    data: [u8; MAX_DATA],
    len: usize,
    handle: Option<Handle>,
    /// The request's capabilities were passed on (to a mount).
    consumed: bool,
}

impl Reply {
    fn status(status: Status) -> Self {
        Self {
            status,
            data: [0; MAX_DATA],
            len: 0,
            handle: None,
            consumed: false,
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

/// Serving removable media (log lines say so).
static MEDIA: AtomicBool = AtomicBool::new(false);

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_str(if MEDIA.load(Ordering::Relaxed) {
        "fs (media): "
    } else {
        "fs: "
    });
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

    let media = directory.find("use", "media");
    MEDIA.store(media.is_some(), Ordering::Relaxed);
    let volume = match media {
        Some(_) => Store::memory(),
        None => Store::Oceans(mount(log, directory.find("use", "block"))),
    };
    let mut mounts = Vec::new();
    for line in directory.lines() {
        let mut words = line.split_whitespace();
        if let (Some(_), Some("use"), Some(name)) = (words.next(), words.next(), words.next())
            && let Some(point) = name.strip_prefix("mount:")
            && oceans_volume::valid_name(point.as_bytes())
            && let Some(handle) = directory.find("use", name)
        {
            mounts.push((point, handle));
        }
    }
    let mut fs = Fs {
        server,
        log,
        volume,
        handles: BTreeMap::new(),
        buffers: BTreeMap::new(),
        next_badge: 1,
        media,
        mounted: false,
        refusal_logged: false,
        mounts,
        remote: BTreeMap::new(),
    };
    if fs.media.is_some() {
        say(log, format_args!("ready for removable media"));
    } else {
        let published = fs.publish_programs(&directory);
        say(log, format_args!("ready, {published} programs in /bin"));
        for (point, _) in &fs.mounts {
            say(log, format_args!("/{point}: a mounted filesystem"));
        }
    }

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
        let reply = fs.request(got.badge, got.label, &request[..got.data_len], received);
        // Capabilities sent with requests are never kept (a shared buffer
        // stays mapped without its handle) unless passed on to a mount.
        if !reply.consumed {
            for &handle in received {
                let _ = oceans_rt::close(handle);
            }
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
        let Some(volume) = self.volume.oceans() else {
            return 0;
        };
        let bin = match volume.create_volatile_directory(ROOT, "bin", true) {
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
                && self
                    .volume
                    .oceans()
                    .is_some_and(|v| v.publish(bin, name, bytes).is_ok())
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

    /// One request: forwarded below a mount point, checked against
    /// removable media, or served here.
    fn request(&mut self, badge: u64, operation: u64, data: &[u8], received: &[Handle]) -> Reply {
        if let Some(&remote) = self.remote.get(&badge) {
            return self.forward(remote, operation, data, received);
        }
        if self.media.is_some() {
            if let Err(status) = self.check_media() {
                return Reply::status(status);
            }
            // A handle from a disk that has since gone (or been replaced).
            if badge != 0 && !self.handles.contains_key(&badge) {
                return Reply::status(Status::NoMedium);
            }
        }
        let reply = if operation == op::ATTACH {
            self.attach(badge, received)
        } else {
            self.handle(badge, operation, data)
        };
        if self.media.is_some() && reply.status == Status::IoError {
            self.unmount("I/O error");
        }
        reply
    }

    // ---- Removable media (ADR-0035) ---------------------------------------

    /// Ensures a volume is mounted from the media: the mounted one if its
    /// disk is still there, otherwise whatever disk is there now.
    fn check_media(&mut self) -> Result<(), Status> {
        if self.mounted {
            if self.volume.alive() {
                return Ok(());
            }
            self.unmount("disk removed");
        }
        let block = self.media.ok_or(Status::NoMedium)?;
        let Ok(disk) = Disk::open(block, BLOCK_SIZE) else {
            self.refusal_logged = false;
            return Err(Status::NoMedium);
        };
        let info = disk.info;
        if info.read_only() || info.sector_size as usize != SECTOR_SIZE {
            return Err(self.refuse("it is read-only or has unusual sectors"));
        }
        let device = DiskDevice {
            disk,
            blocks: info.sectors / u64::from(SECTORS_PER_BLOCK),
        };
        match Volume::open(device, true) {
            Ok((volume, opened)) => {
                let (used, total) = volume.usage();
                self.volume = Store::Oceans(volume);
                self.mounted = true;
                let kib = |blocks: u64| blocks * BLOCK_SIZE as u64 / 1024;
                match opened {
                    Opened::Formatted => say(
                        self.log,
                        format_args!("formatted a blank disk: {} KiB", kib(total)),
                    ),
                    Opened::Mounted => say(
                        self.log,
                        format_args!("mounted the disk: {} of {} KiB used", kib(used), kib(total)),
                    ),
                }
                Ok(())
            }
            Err(MountError::UnknownContents) => self.mount_fat(block),
            Err(MountError::Corrupt(why)) => Err(self.refuse(why)),
            Err(MountError::TooSmall) => Err(self.refuse("it is too small")),
            Err(MountError::Io | MountError::Blank) => Err(Status::NoMedium),
        }
    }

    /// A disk that is not an Oceans volume: a FAT volume on it is served
    /// read-only (ADR-0036); anything else is refused.
    fn mount_fat(&mut self, block: Handle) -> Result<(), Status> {
        let disk = Disk::open(block, BLOCK_SIZE).map_err(|_| Status::NoMedium)?;
        let sectors = disk.info.sectors;
        let read_only = disk.info.read_only();
        match Fat::open(FatDisk {
            disk,
            sectors,
            read_only,
        }) {
            Ok(mut fat) => {
                fat.set_clock(|| oceans_rt::unix_time_ms().map(|ms| ms / 1000));
                // Writing needs a sound volume; after an unclean session
                // it repairs what a crash can leave (ADR-0037).
                let access = match fat.enable_writes() {
                    Ok(recovery) => {
                        if recovery.unclean {
                            say(
                                self.log,
                                format_args!(
                                    "the FAT volume was not synced: {} lost clusters freed, {} orphaned names removed, {} FAT sectors mirrored",
                                    recovery.reclaimed, recovery.orphans, recovery.mirrored
                                ),
                            );
                        }
                        "read-write"
                    }
                    Err(oceans_fat::Error::ReadOnly) => "read-only (the disk is)",
                    Err(_) => "read-only (damaged: check it on another system)",
                };
                say(
                    self.log,
                    format_args!(
                        "mounted a {} volume \"{}\", {access}: {} KiB",
                        fat.kind().name(),
                        fat.label(),
                        fat.capacity() / 1024
                    ),
                );
                self.volume = Store::Fat(fat);
                self.mounted = true;
                Ok(())
            }
            Err(oceans_fat::Error::Io) => Err(Status::NoMedium),
            Err(oceans_fat::Error::NotFat) => {
                Err(self.refuse("it holds neither an Oceans nor a FAT volume"))
            }
            Err(_) => Err(self.refuse("its FAT volume is damaged")),
        }
    }

    fn refuse(&mut self, why: &str) -> Status {
        if !self.refusal_logged {
            say(
                self.log,
                format_args!("disk not mounted ({why}), left untouched"),
            );
            self.refusal_logged = true;
        }
        Status::Unsupported
    }

    /// Forgets the volume: its handles answer `NoMedium` from now on.
    fn unmount(&mut self, why: &str) {
        if !self.mounted {
            return;
        }
        for (_, (base, _)) in core::mem::take(&mut self.buffers) {
            let _ = oceans_rt::memory_unmap(base);
        }
        self.handles.clear();
        self.volume = Store::memory();
        self.mounted = false;
        self.refusal_logged = false;
        say(self.log, format_args!("unmounted the disk ({why})"));
    }

    // ---- Mount points (ADR-0035) ------------------------------------------

    /// Passes a request on a handle below a mount point to the mounted
    /// service; a new handle in its reply is wrapped in one of ours.
    fn forward(
        &mut self,
        remote: Remote,
        operation: u64,
        data: &[u8],
        received: &[Handle],
    ) -> Reply {
        let changes = match operation {
            op::WRITE | op::REMOVE | op::TRUNCATE | op::WRITE_BUF => true,
            op::OPEN => data.first().is_some_and(|f| {
                f & (flags::WRITE | flags::CREATE_FILE | flags::CREATE_DIRECTORY) != 0
            }),
            _ => false,
        };
        // Write access below a mount point comes through a writable handle,
        // as everywhere else.
        if changes && !remote.writable {
            return Reply::status(Status::PermissionDenied);
        }
        let mut reply = Reply::status(Status::Ok);
        let mut handles = [Handle(0); 1];
        let got = match oceans_rt::ipc_call_msg(
            remote.handle,
            operation,
            data,
            received,
            &mut reply.data,
            &mut handles,
        ) {
            Ok(got) => got,
            Err(_) => return Reply::status(Status::IoError),
        };
        reply.consumed = true;
        reply.status = Status::from_label(got.label);
        reply.len = got.data_len.min(MAX_DATA);
        if got.handles_len == 1 {
            let writable =
                operation == op::OPEN && data.first().is_some_and(|f| f & flags::WRITE != 0);
            match self.wrap(handles[0], writable) {
                Ok(handle) => reply.handle = Some(handle),
                Err(status) => {
                    let _ = oceans_rt::close(handles[0]);
                    return Reply::status(status);
                }
            }
        }
        reply
    }

    /// A handle of ours standing for `remote` (a mounted service's).
    fn wrap(&mut self, remote: Handle, writable: bool) -> Result<Handle, Status> {
        let badge = self.next_badge;
        let handle = oceans_rt::endpoint_mint(self.server, badge).map_err(|_| Status::NoSpace)?;
        self.next_badge += 1;
        self.remote.insert(
            badge,
            Remote {
                handle: remote,
                writable,
            },
        );
        Ok(handle)
    }

    /// Opens mount point `name` of the root, if there is one.
    fn open_mount(
        &mut self,
        parent: Open,
        open_flags: u8,
        name: &str,
    ) -> Option<Result<Reply, Status>> {
        if parent.node != ROOT || self.media.is_some() {
            return None;
        }
        let &(_, root) = self.mounts.iter().find(|(point, _)| *point == name)?;
        let writable = open_flags & flags::WRITE != 0;
        if writable && !parent.writable {
            return Some(Err(Status::PermissionDenied));
        }
        Some((|| {
            let remote =
                oceans_rt::duplicate(root, rights::SEND | rights::DUPLICATE | rights::TRANSFER)
                    .map_err(|_| Status::NoSpace)?;
            let handle = self.wrap(remote, writable).inspect_err(|_| {
                let _ = oceans_rt::close(remote);
            })?;
            let mut reply = Reply::ok(&[Kind::Directory as u8]);
            reply.handle = Some(handle);
            Ok(reply)
        })())
    }

    /// `SYNC` also makes the mounted filesystems durable.
    fn sync_mounts(&self) {
        for &(_, root) in &self.mounts {
            let _ = oceans_rt::ipc_call_msg(root, op::SYNC, &[], &[], &mut [], &mut []);
        }
    }

    fn close_handle(&mut self, badge: u64) {
        if let Some(remote) = self.remote.remove(&badge) {
            let _ = oceans_rt::close(remote.handle);
            return;
        }
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
            op::SYNC => {
                self.sync_mounts();
                self.commit("sync").map(|()| Reply::ok(&[])).map_err(status)
            }
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
        if let Some(result) = self.open_mount(open, open_flags, name) {
            return result;
        }
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

    fn list(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let mut index = u32_at(data, 0).ok_or(Status::BadRequest)? as usize;
        // Mount points come first in the root.
        if open.node == ROOT && self.media.is_none() {
            if let Some((point, _)) = self.mounts.get(index) {
                let mut entry = [0u8; 1 + MAX_NAME];
                entry[0] = Kind::Directory as u8;
                entry[1..1 + point.len()].copy_from_slice(point.as_bytes());
                return Ok(Reply::ok(&entry[..1 + point.len()]));
            }
            index -= self.mounts.len();
        }
        let (name, node_kind) = self.volume.entry(open.node, index).map_err(status)?;
        let mut entry = [0u8; 1 + MAX_NAME];
        entry[0] = kind(node_kind) as u8;
        entry[1..1 + name.len()].copy_from_slice(name.as_bytes());
        Ok(Reply::ok(&entry[..1 + name.len()]))
    }

    fn remove(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let name = Self::name(data)?;
        let mount_point = open.node == ROOT && self.mounts.iter().any(|(point, _)| *point == name);
        if !open.writable || mount_point {
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
