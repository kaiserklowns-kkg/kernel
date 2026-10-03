//! The Oceans filesystem service (ADR-0019): an in-memory filesystem
//! speaking `oceans-fs-proto`.
//!
//! - Every open node is a badged client end minted by this service; the
//!   badge indexes the open-handle table, which records the node and the
//!   handle's access (read-only or read-write). Unbadged ends, the ones init
//!   hands out with `use = fs`, are read-write handles to the root.
//! - When a handle is closed anywhere, the kernel sends a close event, and
//!   its entry is dropped. A node is freed once it is unlinked and no handle
//!   refers to it.
//! - Program images granted with `grant = module:NAME` are published
//!   read-only under `/bin`.
//! - Quotas bound memory use: node count, file size, total bytes.
//!
//! Manifest grants: `log`, `provide = fs`, any `module:NAME`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_fs_proto::{Kind, MAX_DATA, MAX_NAME, Status, flags, op, valid_name};
use oceans_rt::{Buffer, Error, Handle, Start, prot};

oceans_rt::entry!(main);

const ROOT: usize = 0;
const MAX_NODES: usize = 4096;
const MAX_FILE_SIZE: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

enum Content {
    File(Vec<u8>),
    Directory(BTreeMap<String, usize>),
}

struct Node {
    content: Content,
    read_only: bool,
    /// Whether a directory entry refers to the node (the root always does).
    linked: bool,
    /// Open handles referring to the node.
    opens: u32,
}

impl Node {
    fn kind(&self) -> Kind {
        match self.content {
            Content::File(_) => Kind::File,
            Content::Directory(_) => Kind::Directory,
        }
    }
}

#[derive(Clone, Copy)]
struct Open {
    node: usize,
    writable: bool,
}

struct Fs {
    server: Handle,
    nodes: Vec<Option<Node>>,
    free_slots: Vec<usize>,
    live_nodes: usize,
    file_bytes: usize,
    handles: BTreeMap<u64, Open>,
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

fn main(start: Start) -> i64 {
    let Some(&directory) = start.handles.last() else {
        return 1;
    };
    let Some(grants) = map_text(directory) else {
        return 2;
    };
    let find = |kind: &str| {
        grants.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            let index: usize = words.next()?.parse().ok()?;
            (words.next()? == kind).then(|| start.handles.get(index).copied())?
        })
    };
    let (Some(log), Some(server)) = (find("log"), find("provide")) else {
        return 3;
    };

    let mut fs = Fs::new(server);
    let bin = fs.create(ROOT, "bin", Content::Directory(BTreeMap::new()), true);
    let mut published = 0;
    for line in grants.lines() {
        let mut words = line.split_whitespace();
        let (Some(index), Some("module"), Some(name)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        let Some(&memory) = index
            .parse::<usize>()
            .ok()
            .and_then(|i| start.handles.get(i))
        else {
            continue;
        };
        if let (Some(bin), Some(bytes)) = (bin, read_memory(memory))
            && fs.create(bin, name, Content::File(bytes), true).is_some()
        {
            published += 1;
        }
        let _ = oceans_rt::close(memory);
    }
    let mut line = Buffer::<128>::new();
    let _ = write!(line, "fs: ready, {published} programs in /bin");
    let _ = oceans_rt::debug_write(log, line.as_str());

    let mut request = [0u8; 256];
    let mut received_handles = [Handle(0); 4];
    loop {
        let got = match oceans_rt::ipc_receive_msg(server, &mut request, &mut received_handles) {
            Ok(got) => got,
            // Every client end is gone (init keeps one, so: shutdown).
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        if got.closed {
            fs.close_handle(got.badge);
            continue;
        }
        // The protocol never sends capabilities to the server.
        for &handle in &received_handles[..got.handles_len] {
            let _ = oceans_rt::close(handle);
        }
        let reply = fs.handle(got.badge, got.label, &request[..got.data_len]);
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

fn map_text(memory: Handle) -> Option<&'static str> {
    let base = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: mapped readable, at least one page, for our lifetime.
    let page = unsafe { core::slice::from_raw_parts(base, 4096) };
    let len = page.iter().position(|&b| b == 0)?;
    core::str::from_utf8(&page[..len]).ok()
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

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

impl Fs {
    fn new(server: Handle) -> Self {
        let root = Node {
            content: Content::Directory(BTreeMap::new()),
            read_only: false,
            linked: true,
            opens: 0,
        };
        Self {
            server,
            nodes: alloc::vec![Some(root)],
            free_slots: Vec::new(),
            live_nodes: 1,
            file_bytes: 0,
            handles: BTreeMap::new(),
            next_badge: 1,
        }
    }

    fn node(&self, index: usize) -> &Node {
        self.nodes[index].as_ref().expect("live node")
    }

    fn node_mut(&mut self, index: usize) -> &mut Node {
        self.nodes[index].as_mut().expect("live node")
    }

    /// Creates `name` in directory `parent`; `None` if it exists or quotas
    /// are reached.
    fn create(
        &mut self,
        parent: usize,
        name: &str,
        content: Content,
        read_only: bool,
    ) -> Option<usize> {
        if self.live_nodes >= MAX_NODES || !valid_name(name.as_bytes()) {
            return None;
        }
        if let Content::Directory(entries) = &self.node(parent).content
            && entries.contains_key(name)
        {
            return None;
        }
        // Accounted only once nothing can fail any more.
        if let Content::File(bytes) = &content {
            if self.file_bytes + bytes.len() > MAX_TOTAL_BYTES {
                return None;
            }
            self.file_bytes += bytes.len();
        }
        let node = Node {
            content,
            read_only,
            linked: true,
            opens: 0,
        };
        let index = match self.free_slots.pop() {
            Some(slot) => {
                self.nodes[slot] = Some(node);
                slot
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        };
        self.live_nodes += 1;
        match &mut self.node_mut(parent).content {
            Content::Directory(entries) => {
                entries.insert(String::from(name), index);
            }
            Content::File(_) => unreachable!("parent checked by callers"),
        }
        Some(index)
    }

    /// Frees `index` if nothing refers to it any more.
    fn release_if_unused(&mut self, index: usize) {
        let node = self.node(index);
        if index == ROOT || node.linked || node.opens > 0 {
            return;
        }
        if let Some(Node {
            content: Content::File(bytes),
            ..
        }) = self.nodes[index].take()
        {
            self.file_bytes -= bytes.len();
        }
        self.nodes[index] = None;
        self.free_slots.push(index);
        self.live_nodes -= 1;
    }

    fn close_handle(&mut self, badge: u64) {
        if let Some(open) = self.handles.remove(&badge) {
            self.node_mut(open.node).opens -= 1;
            self.release_if_unused(open.node);
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
            _ => Err(Status::BadRequest),
        };
        result.unwrap_or_else(Reply::status)
    }

    fn directory(&self, index: usize) -> Result<&BTreeMap<String, usize>, Status> {
        match &self.node(index).content {
            Content::Directory(entries) => Ok(entries),
            Content::File(_) => Err(Status::NotADirectory),
        }
    }

    fn open(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let (&open_flags, name) = data.split_first().ok_or(Status::BadRequest)?;
        if !valid_name(name) {
            return Err(Status::InvalidName);
        }
        let name = core::str::from_utf8(name).map_err(|_| Status::InvalidName)?;
        let existing = self.directory(open.node)?.get(name).copied();
        let index = match existing {
            Some(index) => index,
            None => {
                let content = if open_flags & flags::CREATE_DIRECTORY != 0 {
                    Content::Directory(BTreeMap::new())
                } else if open_flags & flags::CREATE_FILE != 0 {
                    Content::File(Vec::new())
                } else {
                    return Err(Status::NotFound);
                };
                if !open.writable || self.node(open.node).read_only {
                    return Err(Status::PermissionDenied);
                }
                self.create(open.node, name, content, false)
                    .ok_or(Status::NoSpace)?
            }
        };
        // Write access is granted only through a writable parent handle and
        // to a writable node; never silently downgraded.
        let writable = open_flags & flags::WRITE != 0;
        if writable && (!open.writable || self.node(index).read_only) {
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
        self.node_mut(index).opens += 1;
        let mut reply = Reply::ok(&[self.node(index).kind() as u8]);
        reply.handle = Some(handle);
        Ok(reply)
    }

    fn read(&self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let offset = u64_at(data, 0).ok_or(Status::BadRequest)?;
        let len = u32_at(data, 8).ok_or(Status::BadRequest)? as usize;
        let Content::File(bytes) = &self.node(open.node).content else {
            return Err(Status::IsADirectory);
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start + len.min(MAX_DATA).min(bytes.len() - start);
        Ok(Reply::ok(&bytes[start..end]))
    }

    fn write(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let offset = u64_at(data, 0).ok_or(Status::BadRequest)?;
        let payload = &data[8..];
        if !open.writable {
            return Err(Status::PermissionDenied);
        }
        let used = self.file_bytes;
        let Content::File(bytes) = &mut self.node_mut(open.node).content else {
            return Err(Status::IsADirectory);
        };
        let start = usize::try_from(offset).map_err(|_| Status::NoSpace)?;
        let end = start.checked_add(payload.len()).ok_or(Status::NoSpace)?;
        let growth = end.saturating_sub(bytes.len());
        if end > MAX_FILE_SIZE || used + growth > MAX_TOTAL_BYTES {
            return Err(Status::NoSpace);
        }
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(payload);
        self.file_bytes += growth;
        Ok(Reply::ok(&(payload.len() as u32).to_le_bytes()))
    }

    fn stat(&self, open: Open) -> Result<Reply, Status> {
        let node = self.node(open.node);
        let size = match &node.content {
            Content::File(bytes) => bytes.len() as u64,
            Content::Directory(entries) => entries.len() as u64,
        };
        let mut data = [0u8; 10];
        data[0] = node.kind() as u8;
        data[1..9].copy_from_slice(&size.to_le_bytes());
        data[9] = u8::from(open.writable);
        Ok(Reply::ok(&data))
    }

    fn list(&self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let index = u32_at(data, 0).ok_or(Status::BadRequest)? as usize;
        let (name, &child) = self
            .directory(open.node)?
            .iter()
            .nth(index)
            .ok_or(Status::NotFound)?;
        let mut entry = [0u8; 1 + MAX_NAME];
        entry[0] = self.node(child).kind() as u8;
        entry[1..1 + name.len()].copy_from_slice(name.as_bytes());
        Ok(Reply::ok(&entry[..1 + name.len()]))
    }

    fn remove(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        if !valid_name(data) {
            return Err(Status::InvalidName);
        }
        let name = core::str::from_utf8(data).map_err(|_| Status::InvalidName)?;
        if !open.writable || self.node(open.node).read_only {
            return Err(Status::PermissionDenied);
        }
        let child = *self
            .directory(open.node)?
            .get(name)
            .ok_or(Status::NotFound)?;
        let node = self.node(child);
        if node.read_only {
            return Err(Status::PermissionDenied);
        }
        if let Content::Directory(entries) = &node.content
            && !entries.is_empty()
        {
            return Err(Status::NotEmpty);
        }
        if let Content::Directory(entries) = &mut self.node_mut(open.node).content {
            entries.remove(name);
        }
        self.node_mut(child).linked = false;
        self.release_if_unused(child);
        Ok(Reply::ok(&[]))
    }

    fn truncate(&mut self, open: Open, data: &[u8]) -> Result<Reply, Status> {
        let size = usize::try_from(u64_at(data, 0).ok_or(Status::BadRequest)?)
            .map_err(|_| Status::NoSpace)?;
        if !open.writable {
            return Err(Status::PermissionDenied);
        }
        let used = self.file_bytes;
        let Content::File(bytes) = &mut self.node_mut(open.node).content else {
            return Err(Status::IsADirectory);
        };
        let growth = size.saturating_sub(bytes.len());
        if size > MAX_FILE_SIZE || used + growth > MAX_TOTAL_BYTES {
            return Err(Status::NoSpace);
        }
        let shrink = bytes.len().saturating_sub(size);
        bytes.resize(size, 0);
        self.file_bytes = used + growth - shrink;
        Ok(Reply::ok(&[]))
    }
}
