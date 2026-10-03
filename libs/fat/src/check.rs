//! Checking a volume's structure (and, after an unclean session of
//! ours, reclaiming lost clusters; ADR-0037).

use alloc::vec;
use alloc::vec::Vec;

use crate::{ATTR_LONG_NAME, DELETED, Disk, Error, Fat, FatType, LongName, checksum};

/// What a check found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub files: u32,
    pub directories: u32,
    /// Clusters referenced by files and directories.
    pub used: u32,
    /// Clusters marked used that nothing references.
    pub lost: u32,
    /// Lost clusters freed.
    pub reclaimed: u32,
    /// Long-name entries that belong to no short entry (a crash between
    /// the two): ignored by readers, flagged by checkers.
    pub orphans: u32,
    /// Files whose chain is longer than their size needs (a crash while
    /// shrinking); trimmed by a repair.
    pub overlong: u32,
    pub free: u32,
}

impl<D: Disk> Fat<D> {
    /// Walks every directory and chain. Fails with `Corrupt` on damage an
    /// interrupted session of ours cannot cause: two chains sharing a
    /// cluster, a file shorter in clusters than its size, a link to a free
    /// or bad cluster, a directory naming no cluster. Lost clusters are
    /// counted, not changed.
    pub fn check(&mut self) -> Result<Report, Error> {
        self.scan(false)
    }

    /// The long-name entries of directory `first` that do not lead to a
    /// short entry with their checksum.
    fn orphans(&mut self, first: u32) -> Result<Vec<u64>, Error> {
        let mut orphans = Vec::new();
        let mut pending: Vec<u64> = Vec::new();
        let mut long = LongName::new();
        self.slots(first, |offset, raw| {
            match raw[0] {
                0 => {
                    orphans.append(&mut pending);
                    return true;
                }
                DELETED => {
                    orphans.append(&mut pending);
                    long.clear();
                    return false;
                }
                _ => {}
            }
            if raw[11] & 0x3f == ATTR_LONG_NAME {
                if raw[0] & 0x40 != 0 {
                    orphans.append(&mut pending);
                }
                long.add(raw);
                pending.push(offset);
                return false;
            }
            if !pending.is_empty() && !long.matches(checksum(raw)) {
                orphans.append(&mut pending);
            }
            pending.clear();
            long.clear();
            false
        })?;
        orphans.append(&mut pending);
        Ok(orphans)
    }

    pub(crate) fn scan(&mut self, reclaim: bool) -> Result<Report, Error> {
        let total = self.g.clusters as usize + 2;
        let mut marks = vec![0u64; total.div_ceil(64)];
        let mut mark = |cluster: u32| -> bool {
            let (word, bit) = (cluster as usize / 64, cluster % 64);
            let fresh = marks[word] & (1 << bit) == 0;
            marks[word] |= 1 << bit;
            fresh
        };
        let mut report = Report::default();
        let cluster_bytes = self.g.cluster_bytes;
        let root = if self.g.kind == FatType::Fat32 {
            self.g.root_cluster
        } else {
            0
        };
        for cluster in self.chain(root)? {
            if !mark(cluster) {
                return Err(Error::Corrupt);
            }
            report.used += 1;
        }
        let mut directories = vec![root];
        // Over-long files: their entry, chain and clusters needed.
        let mut trims: Vec<(u64, Vec<u32>, usize)> = Vec::new();
        while let Some(directory) = directories.pop() {
            report.directories += 1;
            if report.directories as usize > total {
                return Err(Error::Corrupt);
            }
            let mut entries = Vec::new();
            self.walk(directory, |entry| {
                entries.push(entry);
                false
            })?;
            let orphans = self.orphans(directory)?;
            report.orphans += orphans.len() as u32;
            if reclaim {
                for offset in orphans {
                    self.write_at_byte(offset, DELETED)?;
                }
            }
            for entry in entries {
                if entry.first == 0 {
                    if entry.directory || entry.size > 0 {
                        return Err(Error::Corrupt);
                    }
                    report.files += 1;
                    continue;
                }
                let chain = self.chain(entry.first)?;
                for &cluster in &chain {
                    if !mark(cluster) {
                        return Err(Error::Corrupt);
                    }
                }
                report.used += chain.len() as u32;
                if entry.directory {
                    directories.push(entry.first);
                } else {
                    report.files += 1;
                    let needed = u64::from(entry.size).div_ceil(cluster_bytes) as usize;
                    if chain.len() < needed {
                        return Err(Error::Corrupt);
                    }
                    if chain.len() > needed {
                        report.overlong += 1;
                        trims.push((entry.offset, chain, needed));
                    }
                }
            }
        }
        let bad = self.bad_value();
        let mut lost = Vec::new();
        for cluster in 2..self.g.clusters + 2 {
            let value = self.entry_value(cluster)?;
            let marked = marks[cluster as usize / 64] & (1 << (cluster % 64)) != 0;
            if value == 0 {
                report.free += 1;
            } else if !marked && value != bad {
                lost.push(cluster);
            }
        }
        report.lost = lost.len() as u32;
        if reclaim && report.orphans > 0 {
            self.barrier()?;
        }
        if reclaim && !lost.is_empty() {
            for chunk in lost.chunks(4096) {
                let updates: Vec<(u32, u32)> = chunk.iter().map(|&c| (c, 0)).collect();
                self.set_fat(&updates)?;
            }
            self.barrier()?;
            report.reclaimed = report.lost;
            report.free += report.lost;
        }
        if reclaim {
            for (entry, chain, needed) in trims {
                // As a shrink does: the entry, then the chain's end, then
                // the rest.
                if needed == 0 {
                    self.clear_first(entry)?;
                } else {
                    let end = self.end_value();
                    self.set_fat(&[(chain[needed - 1], end)])?;
                }
                self.barrier()?;
                let rest: Vec<(u32, u32)> = chain[needed..].iter().map(|&c| (c, 0)).collect();
                self.set_fat(&rest)?;
                self.barrier()?;
                report.free += rest.len() as u32;
            }
        }
        Ok(report)
    }
}
