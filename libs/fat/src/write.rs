//! Crash-safe writing (ADR-0037).
//!
//! FAT has no journal, so safety comes from the order of writes, with a
//! barrier ([`Disk::flush`]) wherever a later write depends on an earlier
//! one. The invariants after any prefix of those writes (and any subset of
//! the writes since the last barrier):
//!
//! - a directory entry never names a cluster that is free;
//! - a chain never runs through a free cluster, and no cluster is in two
//!   chains;
//! - a file's size never covers clusters it does not have.
//!
//! What a crash can leave: clusters marked used that nothing references
//! ("lost"), long-name entries without their short entry (ignored), a
//! chain longer than its file needs, and the two FAT copies differing
//! (the first is authoritative). [`Fat::enable_writes`] repairs the first
//! and last after an unclean session. Overwriting existing bytes of a file
//! is not atomic: a crash can leave some new and some old bytes.
//!
//! The volume's clean bit (FAT16/32) is cleared before the first change
//! and set again by [`Fat::sync`], so other systems check a volume that
//! was not synced, and so do we.

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use alloc::vec;
use alloc::vec::Vec;

use crate::check::Report;
use crate::names;
use crate::{
    ATTR_ARCHIVE, ATTR_DIRECTORY, DELETED, Disk, ENTRY, Error, Fat, FatType, IoError, Node, NodeId,
    ROOT, SECTOR, u32_at,
};

#[derive(Default)]
pub(crate) struct State {
    pub enabled: bool,
    /// The clean bit is cleared on the volume.
    pub dirty: bool,
    /// Free clusters, and where the next search starts.
    pub free: u32,
    pub hint: u32,
    pub clock: Option<fn() -> Option<u64>>,
}

/// What enabling writes found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// The volume was not synced when it was last written.
    pub unclean: bool,
    /// Lost clusters freed.
    pub reclaimed: u32,
    /// FAT sectors copied from the first FAT to the others.
    pub mirrored: u32,
    /// Long-name entries without their short entry, removed.
    pub orphans: u32,
    /// Second entries for the same clusters (an interrupted rename),
    /// removed.
    pub duplicates: u32,
    /// Directories whose `..` was corrected (an interrupted move).
    pub parents: u32,
}

const FSINFO_LEAD: u32 = 0x4161_5252;
const FSINFO_STRUCT: u32 = 0x6141_7272;
const NO_VALUE: u32 = 0xffff_ffff;

impl<D: Disk> Fat<D> {
    pub fn writable(&self) -> bool {
        self.writes.enabled
    }

    /// The source of timestamps for entries: Unix seconds, if known.
    pub fn set_clock(&mut self, clock: fn() -> Option<u64>) {
        self.writes.clock = Some(clock);
    }

    /// Turns writing on: checks the volume and, if it was not synced after
    /// its last changes, repairs what an interrupted session can leave
    /// (lost clusters, differing FAT copies). Fails (writing stays off)
    /// on a read-only disk or on damage a crash of ours cannot cause.
    pub fn enable_writes(&mut self) -> Result<Recovery, Error> {
        let mut recovery = Recovery {
            unclean: false,
            reclaimed: 0,
            mirrored: 0,
            orphans: 0,
            duplicates: 0,
            parents: 0,
        };
        if self.writes.enabled {
            return Ok(recovery);
        }
        if !self.disk.writable() {
            return Err(Error::ReadOnly);
        }
        recovery.unclean = !self.is_clean()?;
        if recovery.unclean {
            recovery.mirrored = self.mirror_fats()?;
            let report: Report = self.scan(true)?;
            recovery.reclaimed = report.reclaimed;
            recovery.orphans = report.orphans;
            recovery.duplicates = report.duplicates;
            recovery.parents = report.parents;
            self.writes.free = report.free;
            self.writes.hint = 2;
            self.update_fsinfo()?;
            if self.clean_mask().is_some() {
                self.set_clean(true)?;
                self.barrier()?;
            }
        } else {
            self.writes.free = match self.fsinfo_free()? {
                Some(free) => free,
                None => self.count_free()?,
            };
        }
        self.writes.hint = 2;
        self.writes.enabled = true;
        Ok(recovery)
    }

    /// Makes everything so far durable and marks the volume clean.
    pub fn sync(&mut self) -> Result<(), Error> {
        if !self.writes.enabled || !self.writes.dirty {
            return Ok(());
        }
        self.barrier()?;
        self.update_fsinfo()?;
        if self.clean_mask().is_some() {
            self.set_clean(true)?;
        }
        self.barrier()?;
        self.writes.dirty = false;
        Ok(())
    }

    // ---- Files -------------------------------------------------------------

    /// Writes `data` at `offset` (past the end, the gap is zero-filled).
    pub fn write_file(&mut self, id: NodeId, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let node = self.node(id)?;
        if node.directory {
            return Err(Error::IsADirectory);
        }
        if !self.writes.enabled {
            return Err(Error::ReadOnly);
        }
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|&end| end <= u64::from(u32::MAX))
            .ok_or(Error::NoSpace)?;
        let size = u64::from(node.size);
        if offset > size {
            let zeros = [0u8; 4096];
            let mut at = size;
            while at < offset {
                let take = ((offset - at) as usize).min(zeros.len());
                self.write_file(id, at, &zeros[..take])?;
                at += take as u64;
            }
            return self.write_file(id, offset, data);
        }
        self.begin()?;
        let cb = self.g.cluster_bytes;
        let have = size.div_ceil(cb) as u32;
        let need = end.div_ceil(cb) as u32;
        let new = if need > have {
            self.allocate(need - have)?
        } else {
            Vec::new()
        };

        // 1. The data: into the file's clusters (beyond the size it is not
        //    yet visible) and into the new ones (still free).
        let mut done = 0;
        while done < data.len() {
            let at = offset + done as u64;
            let index = (at / cb) as u32;
            let within = at % cb;
            let take = ((cb - within) as usize).min(data.len() - done);
            let cluster = if index < have {
                self.cluster_at(id, index)?
            } else {
                new[(index - have) as usize]
            };
            let base = self.cluster_offset(cluster)?;
            self.write_bytes(base + within, &data[done..done + take])?;
            done += take;
        }
        let mut first = node.first;
        if let Some(&last) = new.last() {
            // 2. The new clusters' own chain, ended.
            let mut links: Vec<(u32, u32)> = new.windows(2).map(|w| (w[0], w[1])).collect();
            links.push((last, self.end_value()));
            self.set_fat(&links)?;
            self.barrier()?;
            self.writes.free = self.writes.free.saturating_sub(new.len() as u32);
            // 3. Linked to the file's tail (or, for an empty file, named by
            //    its entry in step 4).
            if have > 0 {
                let tail = self.cluster_at(id, have - 1)?;
                self.set_fat(&[(tail, new[0])])?;
                self.barrier()?;
            } else {
                first = new[0];
            }
        }
        // 4. The entry: first cluster and size, last.
        let new_size = size.max(end) as u32;
        if new_size != node.size || first != node.first {
            if let Some(entry) = node.entry {
                self.update_entry(entry, first, new_size)?;
            }
            self.set_node(id, first, new_size);
        }
        self.barrier()?;
        Ok(data.len())
    }

    /// Sets a file's size: shrinking frees clusters, growing zero-fills.
    pub fn truncate(&mut self, id: NodeId, new_size: u64) -> Result<(), Error> {
        let node = self.node(id)?;
        if node.directory {
            return Err(Error::IsADirectory);
        }
        if !self.writes.enabled {
            return Err(Error::ReadOnly);
        }
        let size = u64::from(node.size);
        if new_size > u64::from(u32::MAX) {
            return Err(Error::NoSpace);
        }
        if new_size > size {
            let zeros = [0u8; 4096];
            let mut at = size;
            while at < new_size {
                let take = ((new_size - at) as usize).min(zeros.len());
                self.write_file(id, at, &zeros[..take])?;
                at += take as u64;
            }
            return Ok(());
        }
        if new_size == size {
            return Ok(());
        }
        self.begin()?;
        let keep = new_size.div_ceil(self.g.cluster_bytes) as u32;
        let (tail, rest_first) = if keep == 0 {
            (None, node.first)
        } else {
            let tail = self.cluster_at(id, keep - 1)?;
            (Some(tail), self.next(tail)?.unwrap_or(0))
        };
        let rest = self.chain(rest_first)?;
        // 1. The entry shrinks first (and an empty file names no cluster).
        let first = if keep == 0 { 0 } else { node.first };
        if let Some(entry) = node.entry {
            self.update_entry(entry, first, new_size as u32)?;
        }
        self.set_node(id, first, new_size as u32);
        self.barrier()?;
        // 2. The chain ends at the new tail.
        if let Some(tail) = tail
            && !rest.is_empty()
        {
            let end = self.end_value();
            self.set_fat(&[(tail, end)])?;
            self.barrier()?;
        }
        // 3. What is left over is freed.
        self.free(&rest)
    }

    // ---- Directories ---------------------------------------------------------

    /// Creates file or directory `name` in `dir`.
    pub fn create(&mut self, dir: NodeId, name: &str, directory: bool) -> Result<NodeId, Error> {
        if !self.writes.enabled {
            return Err(Error::ReadOnly);
        }
        names::validate(name)?;
        let parent = self.directory(dir)?;
        if self.find(parent, name)?.is_some() {
            return Err(Error::Exists);
        }
        let plan = names::plan(name);
        let short = if plan.long {
            self.unique_short(parent, &plan.short)?
        } else {
            plan.short
        };
        let long = if plan.long {
            names::long_entries(name, &short)
        } else {
            Vec::new()
        };
        self.begin()?;
        let slots = self.free_slots(parent, long.len() + 1)?;
        let stamp = self.stamp();
        // A directory's cluster is ready (".", "..", the rest empty) and
        // allocated before anything names it.
        let first = if directory {
            let cluster = self.allocate(1)?[0];
            let mut bytes = vec![0u8; self.g.cluster_bytes as usize];
            let parent_link = if dir == ROOT { 0 } else { parent };
            bytes[..ENTRY].copy_from_slice(&short_entry(
                b".          ",
                0,
                ATTR_DIRECTORY,
                cluster,
                0,
                stamp,
            ));
            bytes[ENTRY..2 * ENTRY].copy_from_slice(&short_entry(
                b"..         ",
                0,
                ATTR_DIRECTORY,
                parent_link,
                0,
                stamp,
            ));
            let base = self.cluster_offset(cluster)?;
            self.write_bytes(base, &bytes)?;
            let end = self.end_value();
            self.set_fat(&[(cluster, end)])?;
            self.barrier()?;
            self.writes.free = self.writes.free.saturating_sub(1);
            cluster
        } else {
            0
        };
        // The long entries, then the short entry that makes it exist.
        for (&slot, entry) in slots.iter().zip(&long) {
            self.write_bytes(slot, entry)?;
        }
        if !long.is_empty() {
            self.barrier()?;
        }
        let attributes = if directory {
            ATTR_DIRECTORY
        } else {
            ATTR_ARCHIVE
        };
        let offset = slots[long.len()];
        self.write_bytes(
            offset,
            &short_entry(&short, plan.case, attributes, first, 0, stamp),
        )?;
        self.barrier()?;
        Ok(self.insert(Node {
            entry: Some(offset),
            first,
            size: 0,
            directory,
            refs: 0,
            cursor: (0, first),
            deleted: false,
        }))
    }

    /// Removes `name` from `dir` (a directory only when empty). An open
    /// file keeps its clusters until its last handle goes.
    pub fn remove(&mut self, dir: NodeId, name: &str) -> Result<(), Error> {
        if !self.writes.enabled {
            return Err(Error::ReadOnly);
        }
        let parent = self.directory(dir)?;
        let entry = self.find(parent, name)?.ok_or(Error::NotFound)?;
        if entry.directory {
            let mut empty = true;
            self.walk(entry.first, |_| {
                empty = false;
                true
            })?;
            if !empty {
                return Err(Error::NotEmpty);
            }
        }
        self.begin()?;
        // 1. The entry goes: the short entry (what makes it exist) first.
        self.delete_entry(&entry)?;
        self.barrier()?;
        // 2. Its clusters: now, or when the last handle on it goes.
        let held = self
            .nodes
            .iter()
            .position(|n| n.is_some_and(|n| n.entry == Some(entry.offset) && !n.deleted));
        if let Some(id) = held
            && let Some(Some(node)) = self.nodes.get_mut(id)
        {
            node.entry = None;
            if node.refs > 0 {
                node.deleted = true;
                return Ok(());
            }
            self.nodes[id] = None;
        }
        self.free_chain(entry.first)
    }

    /// Moves entry `name` of `dir` to `new_name` in `new_dir` (ADR-0038):
    /// the new entry, a moved directory's `..`, then the old entry's
    /// removal, with barriers. A crash in between leaves two entries for
    /// the same clusters, which the next repair resolves to one of them.
    /// An existing file there is replaced, but not atomically: FAT cannot
    /// swap entries, so its entry goes first.
    pub fn rename(
        &mut self,
        dir: NodeId,
        name: &str,
        new_dir: NodeId,
        new_name: &str,
    ) -> Result<(), Error> {
        if !self.writes.enabled {
            return Err(Error::ReadOnly);
        }
        names::validate(new_name)?;
        let parent = self.directory(dir)?;
        let new_parent = self.directory(new_dir)?;
        let source = self.find(parent, name)?.ok_or(Error::NotFound)?;
        if source.directory {
            self.refuse_cycle(source.first, new_parent)?;
        }
        let mut target = self.find(new_parent, new_name)?;
        if target.as_ref().is_some_and(|t| t.offset == source.offset) {
            if source.name == new_name {
                return Ok(());
            }
            // The same entry: only the case of its name changes.
            target = None;
        }
        if let Some(t) = &target {
            match (source.directory, t.directory) {
                (false, true) => return Err(Error::IsADirectory),
                (true, false) => return Err(Error::NotADirectory),
                (true, true) => {
                    let mut empty = true;
                    self.walk(t.first, |_| {
                        empty = false;
                        true
                    })?;
                    if !empty {
                        return Err(Error::NotEmpty);
                    }
                }
                (false, false) => {}
            }
        }
        self.begin()?;
        let mut raw = [0u8; ENTRY];
        self.read(source.offset, &mut raw)?;
        let plan = names::plan(new_name);
        if parent == new_parent
            && !plan.long
            && source.long.is_empty()
            && plan.short == source.raw_short
        {
            // A short name whose case changes: one entry, rewritten whole.
            raw[12] = (raw[12] & !0x18) | plan.case;
            self.write_bytes(source.offset, &raw)?;
            return self.barrier();
        }
        // The target's node no longer has an entry.
        let target_node = target.as_ref().and_then(|t| {
            self.nodes
                .iter()
                .position(|n| n.is_some_and(|n| n.entry == Some(t.offset) && !n.deleted))
        });
        if let Some(id) = target_node
            && let Some(Some(node)) = self.nodes.get_mut(id)
        {
            node.entry = None;
        }
        // 1. A replaced target goes first.
        if let Some(t) = &target {
            self.delete_entry(t)?;
            self.barrier()?;
        }
        // 2. The new entry: the same clusters, size, attributes and times.
        let short = if plan.long {
            self.unique_short(new_parent, &plan.short)?
        } else {
            plan.short
        };
        let long = if plan.long {
            names::long_entries(new_name, &short)
        } else {
            Vec::new()
        };
        let slots = self.free_slots(new_parent, long.len() + 1)?;
        raw[..11].copy_from_slice(&short);
        raw[12] = (raw[12] & !0x18) | plan.case;
        for (&slot, entry) in slots.iter().zip(&long) {
            self.write_bytes(slot, entry)?;
        }
        if !long.is_empty() {
            self.barrier()?;
        }
        let new_offset = slots[long.len()];
        self.write_bytes(new_offset, &raw)?;
        self.barrier()?;
        // 3. A moved directory names its new parent.
        if source.directory && parent != new_parent {
            self.set_parent_link(source.first, if new_dir == ROOT { 0 } else { new_parent })?;
            self.barrier()?;
        }
        // 4. The old entry goes.
        self.delete_entry(&source)?;
        self.barrier()?;
        for node in self.nodes.iter_mut().flatten() {
            if node.entry == Some(source.offset) && !node.deleted {
                node.entry = Some(new_offset);
            }
        }
        // 5. The replaced target's clusters: now, or when its last handle
        //    goes.
        if let Some(t) = target {
            if let Some(id) = target_node
                && let Some(Some(node)) = self.nodes.get_mut(id)
            {
                if node.refs > 0 {
                    node.deleted = true;
                    return Ok(());
                }
                self.nodes[id] = None;
            }
            self.free_chain(t.first)?;
        }
        Ok(())
    }

    /// Refuses moving directory `moving` into itself or below itself.
    fn refuse_cycle(&mut self, moving: u32, new_parent: u32) -> Result<(), Error> {
        let root = if self.g.kind == FatType::Fat32 {
            self.g.root_cluster
        } else {
            0
        };
        let mut current = new_parent;
        for _ in 0..=self.g.clusters {
            if current == moving {
                return Err(Error::InvalidName);
            }
            if current == root {
                return Ok(());
            }
            let parent = self.parent_link(current)?;
            current = if parent == 0 { root } else { parent };
        }
        Err(Error::Corrupt)
    }

    /// The cluster a directory's `..` entry names (0: the root).
    pub(crate) fn parent_link(&mut self, directory: u32) -> Result<u32, Error> {
        let mut raw = [0u8; ENTRY];
        self.read(self.cluster_offset(directory)? + ENTRY as u64, &mut raw)?;
        if &raw[..11] != b"..         " {
            return Err(Error::Corrupt);
        }
        let first = u32::from(u16::from_le_bytes([raw[20], raw[21]])) << 16
            | u32::from(u16::from_le_bytes([raw[26], raw[27]]));
        Ok(if self.g.kind == FatType::Fat32 {
            first
        } else {
            first & 0xffff
        })
    }

    pub(crate) fn set_parent_link(&mut self, directory: u32, parent: u32) -> Result<(), Error> {
        let offset = self.cluster_offset(directory)? + ENTRY as u64;
        let mut raw = [0u8; ENTRY];
        self.read(offset, &mut raw)?;
        if &raw[..11] != b"..         " {
            return Err(Error::Corrupt);
        }
        raw[20..22].copy_from_slice(&((parent >> 16) as u16).to_le_bytes());
        raw[26..28].copy_from_slice(&(parent as u16).to_le_bytes());
        self.write_bytes(offset, &raw)
    }

    /// Marks an entry deleted: the short entry first, then its long ones.
    pub(crate) fn delete_entry(&mut self, entry: &crate::Entry) -> Result<(), Error> {
        self.write_bytes(entry.offset, &[DELETED])?;
        for &long in &entry.long {
            self.write_bytes(long, &[DELETED])?;
        }
        Ok(())
    }

    /// `n` free clusters, searched from the hint (not yet marked).
    fn allocate(&mut self, n: u32) -> Result<Vec<u32>, Error> {
        if n > self.writes.free {
            return Err(Error::NoSpace);
        }
        let last = self.g.clusters + 1;
        let mut cluster = self.writes.hint.clamp(2, last);
        let mut out = Vec::new();
        for _ in 0..self.g.clusters {
            if self.entry_value(cluster)? == 0 {
                out.push(cluster);
                if out.len() == n as usize {
                    break;
                }
            }
            cluster = if cluster >= last { 2 } else { cluster + 1 };
        }
        if out.len() < n as usize {
            return Err(Error::NoSpace);
        }
        self.writes.hint = if cluster >= last { 2 } else { cluster + 1 };
        Ok(out)
    }

    /// Short name `basis~N` not used in directory `first`.
    fn unique_short(&mut self, first: u32, basis: &[u8; 11]) -> Result<[u8; 11], Error> {
        let mut taken: Vec<[u8; 11]> = Vec::new();
        self.walk(first, |entry| {
            taken.push(entry.raw_short);
            false
        })?;
        (1..1_000_000)
            .map(|n| names::with_tail(basis, n))
            .find(|candidate| !taken.contains(candidate))
            .ok_or(Error::NoSpace)
    }

    /// `count` consecutive free entry slots in directory `first`, growing
    /// it by a cluster if needed.
    fn free_slots(&mut self, first: u32, count: usize) -> Result<Vec<u64>, Error> {
        for attempt in 0..2 {
            let mut all: Vec<(u64, u8)> = Vec::new();
            self.slots(first, |offset, raw| {
                all.push((offset, raw[0]));
                false
            })?;
            let end = all.iter().position(|&(_, b)| b == 0).unwrap_or(all.len());
            let free = |i: usize| i >= end || all[i].1 == DELETED;
            let mut run = 0;
            for i in 0..all.len() {
                run = if free(i) { run + 1 } else { 0 };
                if run == count {
                    let start = i + 1 - count;
                    // Taking the end marker: whatever follows our entries
                    // must read as the end.
                    if i >= end && i + 1 < all.len() && all[i + 1].1 != 0 {
                        self.write_bytes(all[i + 1].0, &[0])?;
                    }
                    return Ok(all[start..=i].iter().map(|&(offset, _)| offset).collect());
                }
            }
            if attempt == 1 || (first == 0 && self.g.kind != FatType::Fat32) {
                // FAT12/16 roots have a fixed size.
                return Err(Error::NoSpace);
            }
            // Grow: a zeroed cluster, ended, then linked.
            let chain = self.chain(first)?;
            let tail = *chain.last().ok_or(Error::Corrupt)?;
            let cluster = self.allocate(1)?[0];
            let zeros = vec![0u8; self.g.cluster_bytes as usize];
            let base = self.cluster_offset(cluster)?;
            self.write_bytes(base, &zeros)?;
            let end = self.end_value();
            self.set_fat(&[(cluster, end)])?;
            self.barrier()?;
            self.writes.free = self.writes.free.saturating_sub(1);
            self.set_fat(&[(tail, cluster)])?;
            self.barrier()?;
        }
        Err(Error::NoSpace)
    }

    // ---- The FAT -------------------------------------------------------------

    /// Frees the chain starting at `first`.
    pub(crate) fn free_chain(&mut self, first: u32) -> Result<(), Error> {
        if !self.writes.enabled {
            return Ok(());
        }
        let chain = self.chain(first)?;
        self.begin()?;
        self.free(&chain)
    }

    fn free(&mut self, clusters: &[u32]) -> Result<(), Error> {
        if clusters.is_empty() {
            return Ok(());
        }
        let updates: Vec<(u32, u32)> = clusters.iter().map(|&c| (c, 0)).collect();
        self.set_fat(&updates)?;
        self.barrier()?;
        self.writes.free += clusters.len() as u32;
        Ok(())
    }

    pub(crate) fn end_value(&self) -> u32 {
        match self.g.kind {
            FatType::Fat12 => 0xfff,
            FatType::Fat16 => 0xffff,
            FatType::Fat32 => 0x0fff_ffff,
        }
    }

    pub(crate) fn bad_value(&self) -> u32 {
        self.end_of_chain() - 1
    }

    /// Sets FAT entries in every FAT copy in use (FAT32 keeps each
    /// entry's top four bits).
    pub(crate) fn set_fat(&mut self, updates: &[(u32, u32)]) -> Result<(), Error> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut sectors: BTreeMap<u64, [u8; 512]> = BTreeMap::new();
        for &(cluster, value) in updates {
            match self.g.kind {
                FatType::Fat12 => {
                    let at = u64::from(cluster) * 3 / 2;
                    if cluster & 1 == 0 {
                        self.put(&mut sectors, at, 0xff, value as u8)?;
                        self.put(&mut sectors, at + 1, 0x0f, (value >> 8) as u8)?;
                    } else {
                        self.put(&mut sectors, at, 0xf0, (value << 4) as u8)?;
                        self.put(&mut sectors, at + 1, 0xff, (value >> 4) as u8)?;
                    }
                }
                FatType::Fat16 => {
                    let at = u64::from(cluster) * 2;
                    self.put(&mut sectors, at, 0xff, value as u8)?;
                    self.put(&mut sectors, at + 1, 0xff, (value >> 8) as u8)?;
                }
                FatType::Fat32 => {
                    let at = u64::from(cluster) * 4;
                    for i in 0..4 {
                        let mask = if i == 3 { 0x0f } else { 0xff };
                        self.put(&mut sectors, at + i, mask, (value >> (8 * i)) as u8)?;
                    }
                }
            }
        }
        let copies: Vec<u64> = match self.g.active_fat {
            Some(active) => vec![u64::from(active)],
            None => (0..u64::from(self.g.fats)).collect(),
        };
        for copy in copies {
            for (&offset, bytes) in &sectors {
                let at = self.g.fat + copy * self.g.fat_bytes + offset;
                self.disk.write_at(at, bytes).map_err(|IoError| Error::Io)?;
            }
        }
        self.fat_cache = None;
        Ok(())
    }

    /// Changes the `mask` bits of FAT byte `at` in the pending sectors.
    fn put(
        &mut self,
        sectors: &mut BTreeMap<u64, [u8; 512]>,
        at: u64,
        mask: u8,
        value: u8,
    ) -> Result<(), Error> {
        if at >= self.g.fat_bytes {
            return Err(Error::Corrupt);
        }
        let sector = at / SECTOR * SECTOR;
        if let Entry::Vacant(slot) = sectors.entry(sector) {
            let mut bytes = [0u8; 512];
            let base = self.read_fat();
            self.read(base + sector, &mut bytes)?;
            slot.insert(bytes);
        }
        let bytes = sectors.get_mut(&sector).expect("inserted");
        let byte = &mut bytes[(at - sector) as usize];
        *byte = (*byte & !mask) | (value & mask);
        Ok(())
    }

    /// Copies the first FAT over the others where they differ (a crash
    /// between the copies' writes); returns the sectors copied.
    fn mirror_fats(&mut self) -> Result<u32, Error> {
        if self.g.active_fat.is_some() || self.g.fats < 2 {
            return Ok(0);
        }
        let mut copied = 0;
        let mut first = [0u8; 512];
        let mut other = [0u8; 512];
        for offset in (0..self.g.fat_bytes).step_by(SECTOR as usize) {
            self.read(self.g.fat + offset, &mut first)?;
            for copy in 1..u64::from(self.g.fats) {
                let at = self.g.fat + copy * self.g.fat_bytes + offset;
                self.read(at, &mut other)?;
                if other != first {
                    self.disk
                        .write_at(at, &first)
                        .map_err(|IoError| Error::Io)?;
                    copied += 1;
                }
            }
        }
        if copied > 0 {
            self.barrier()?;
        }
        Ok(copied)
    }

    fn clean_mask(&self) -> Option<u32> {
        match self.g.kind {
            FatType::Fat12 => None,
            FatType::Fat16 => Some(0x8000),
            FatType::Fat32 => Some(0x0800_0000),
        }
    }

    /// FAT12 has no clean bit: always treated as unclean (and checked).
    fn is_clean(&mut self) -> Result<bool, Error> {
        match self.clean_mask() {
            Some(mask) => Ok(self.raw_entry(1)? & mask != 0),
            None => Ok(false),
        }
    }

    fn set_clean(&mut self, clean: bool) -> Result<(), Error> {
        let Some(mask) = self.clean_mask() else {
            return Ok(());
        };
        let raw = self.raw_entry(1)?;
        let value = if clean { raw | mask } else { raw & !mask };
        self.set_fat(&[(1, value)])
    }

    /// Before the first change of a session: the volume is marked unclean,
    /// durably, so a crash from here on is noticed.
    fn begin(&mut self) -> Result<(), Error> {
        if !self.writes.enabled {
            return Err(Error::ReadOnly);
        }
        if !self.writes.dirty {
            self.set_clean(false)?;
            self.barrier()?;
            self.writes.dirty = true;
        }
        Ok(())
    }

    fn count_free(&mut self) -> Result<u32, Error> {
        let mut free = 0;
        for cluster in 2..self.g.clusters + 2 {
            if self.entry_value(cluster)? == 0 {
                free += 1;
            }
        }
        Ok(free)
    }

    /// FSInfo's free count, if the sector is valid and the count plausible.
    fn fsinfo_free(&mut self) -> Result<Option<u32>, Error> {
        let Some(at) = self.g.fsinfo else {
            return Ok(None);
        };
        let mut sector = [0u8; 512];
        self.read(at, &mut sector)?;
        let valid = u32_at(&sector, 0) == FSINFO_LEAD && u32_at(&sector, 484) == FSINFO_STRUCT;
        let free = u32_at(&sector, 488);
        Ok((valid && free != NO_VALUE && free <= self.g.clusters).then_some(free))
    }

    fn update_fsinfo(&mut self) -> Result<(), Error> {
        let Some(at) = self.g.fsinfo else {
            return Ok(());
        };
        let mut sector = [0u8; 512];
        self.read(at, &mut sector)?;
        if u32_at(&sector, 0) != FSINFO_LEAD || u32_at(&sector, 484) != FSINFO_STRUCT {
            return Ok(());
        }
        sector[488..492].copy_from_slice(&self.writes.free.to_le_bytes());
        sector[492..496].copy_from_slice(&self.writes.hint.to_le_bytes());
        self.disk.write_at(at, &sector).map_err(|IoError| Error::Io)
    }

    // ---- Bytes and entries ---------------------------------------------------

    pub(crate) fn barrier(&mut self) -> Result<(), Error> {
        self.disk.flush().map_err(|IoError| Error::Io)
    }

    /// Writes bytes anywhere: whole sectors directly, edges read, changed
    /// and written back.
    fn write_bytes(&mut self, mut offset: u64, mut data: &[u8]) -> Result<(), Error> {
        while !data.is_empty() {
            let sector = offset / SECTOR * SECTOR;
            let within = (offset - sector) as usize;
            let take = if within == 0 && data.len() >= SECTOR as usize {
                let whole = (data.len() / SECTOR as usize * SECTOR as usize).min(64 * 1024);
                self.disk
                    .write_at(offset, &data[..whole])
                    .map_err(|IoError| Error::Io)?;
                whole
            } else {
                let mut bytes = [0u8; 512];
                self.read(sector, &mut bytes)?;
                let take = (SECTOR as usize - within).min(data.len());
                bytes[within..within + take].copy_from_slice(&data[..take]);
                self.disk
                    .write_at(sector, &bytes)
                    .map_err(|IoError| Error::Io)?;
                take
            };
            offset += take as u64;
            data = &data[take..];
        }
        self.fat_cache = None;
        Ok(())
    }

    /// An entry that names a cluster but has size 0 names none.
    pub(crate) fn clear_first(&mut self, offset: u64) -> Result<(), Error> {
        let mut raw = [0u8; ENTRY];
        self.read(offset, &mut raw)?;
        raw[20..22].fill(0);
        raw[26..28].fill(0);
        self.write_bytes(offset, &raw)
    }

    /// One byte (an entry's first: deleting it).
    pub(crate) fn write_at_byte(&mut self, offset: u64, byte: u8) -> Result<(), Error> {
        self.write_bytes(offset, &[byte])
    }

    /// Points a short entry at `first` with `size`, stamped now.
    fn update_entry(&mut self, offset: u64, first: u32, size: u32) -> Result<(), Error> {
        let mut raw = [0u8; ENTRY];
        self.read(offset, &mut raw)?;
        let (date, time) = self.stamp();
        raw[11] |= ATTR_ARCHIVE;
        raw[18..20].copy_from_slice(&date.to_le_bytes());
        raw[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        raw[22..24].copy_from_slice(&time.to_le_bytes());
        raw[24..26].copy_from_slice(&date.to_le_bytes());
        raw[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        raw[28..32].copy_from_slice(&size.to_le_bytes());
        self.write_bytes(offset, &raw)
    }

    fn set_node(&mut self, id: NodeId, first: u32, size: u32) {
        if let Some(Some(node)) = self.nodes.get_mut(id) {
            if node.first != first {
                node.cursor = (0, first);
            }
            node.first = first;
            node.size = size;
            if node.cursor.1 == 0 {
                node.cursor = (0, first);
            }
        }
    }

    /// FAT date and time of now (1980-01-01 without a clock).
    fn stamp(&self) -> (u16, u16) {
        let Some(seconds) = self.writes.clock.and_then(|clock| clock()) else {
            return (0x21, 0);
        };
        let (year, month, day) = civil(seconds / 86_400);
        if !(1980..=2107).contains(&year) {
            return (0x21, 0);
        }
        let in_day = seconds % 86_400;
        let date = ((year - 1980) << 9 | month << 5 | day) as u16;
        let time = ((in_day / 3600) << 11 | (in_day % 3600 / 60) << 5 | (in_day % 60 / 2)) as u16;
        (date, time)
    }
}

/// A short (8.3) directory entry.
fn short_entry(
    name: &[u8; 11],
    case: u8,
    attributes: u8,
    first: u32,
    size: u32,
    (date, time): (u16, u16),
) -> [u8; ENTRY] {
    let mut raw = [0u8; ENTRY];
    raw[..11].copy_from_slice(name);
    raw[11] = attributes;
    raw[12] = case;
    raw[14..16].copy_from_slice(&time.to_le_bytes());
    raw[16..18].copy_from_slice(&date.to_le_bytes());
    raw[18..20].copy_from_slice(&date.to_le_bytes());
    raw[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
    raw[22..24].copy_from_slice(&time.to_le_bytes());
    raw[24..26].copy_from_slice(&date.to_le_bytes());
    raw[26..28].copy_from_slice(&(first as u16).to_le_bytes());
    raw[28..32].copy_from_slice(&size.to_le_bytes());
    raw
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year_of_era + era * 400 + u64::from(month <= 2), month, day)
}
