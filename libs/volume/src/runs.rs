//! A file's blocks as runs (ADR-0142): each a stretch of the file's
//! blocks kept in consecutive disk blocks. A file written in one go is a
//! single run however long it is; blocks with no run are holes.

use alloc::collections::BTreeMap;

/// Logical start → (disk start, length), not overlapping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Runs(BTreeMap<u32, (u32, u32)>);

impl Runs {
    /// The disk block of file block `index`; `None`: a hole.
    pub(crate) fn get(&self, index: u32) -> Option<u32> {
        let (&start, &(disk, len)) = self.0.range(..=index).next_back()?;
        (index - start < len).then(|| disk + (index - start))
    }

    /// How many runs.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// The runs, as (logical start, disk start, length), in file order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        self.0
            .iter()
            .map(|(&start, &(disk, len))| (start, disk, len))
    }

    /// Whether any of file blocks `from..to` has a disk block.
    pub(crate) fn any_in(&self, from: u32, to: u32) -> bool {
        self.0
            .range(..to)
            .next_back()
            .is_some_and(|(&start, &(_, len))| start.saturating_add(len) > from)
    }

    /// Adds a run after the last one (for loading); `false` if it does not
    /// come after it.
    pub(crate) fn push(&mut self, start: u32, disk: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        if let Some((&last, &(last_disk, last_len))) = self.0.iter().next_back() {
            if start < last.saturating_add(last_len) {
                return false;
            }
            // Contiguous on both sides: one run.
            if start == last + last_len && disk == last_disk.wrapping_add(last_len) {
                self.0.insert(last, (last_disk, last_len + len));
                return true;
            }
        }
        self.0.insert(start, (disk, len));
        true
    }

    /// Takes file block `index` out of its run (splitting it).
    fn unmap(&mut self, index: u32) {
        let Some((&start, &(disk, len))) = self.0.range(..=index).next_back() else {
            return;
        };
        if index - start >= len {
            return;
        }
        self.0.remove(&start);
        if index > start {
            self.0.insert(start, (disk, index - start));
        }
        let after = index - start + 1;
        if after < len {
            self.0.insert(index + 1, (disk + after, len - after));
        }
    }

    /// File block `index` is now disk block `disk`; neighbours that follow
    /// on, on both sides, join it.
    pub(crate) fn set(&mut self, index: u32, disk: u32) {
        self.unmap(index);
        let (mut start, mut first, mut len) = (index, disk, 1);
        if let Some((&before, &(before_disk, before_len))) = self.0.range(..index).next_back()
            && before + before_len == index
            && before_disk.wrapping_add(before_len) == disk
        {
            self.0.remove(&before);
            (start, first, len) = (before, before_disk, before_len + 1);
        }
        if let Some(&(after_disk, after_len)) = self.0.get(&(index + 1))
            && after_disk == disk.wrapping_add(1)
        {
            self.0.remove(&(index + 1));
            len += after_len;
        }
        self.0.insert(start, (first, len));
    }

    /// Keeps the first `count` file blocks; `released` gets every disk
    /// block let go.
    pub(crate) fn truncate(&mut self, count: u32, mut released: impl FnMut(u32)) {
        let tail = self.0.split_off(&count);
        for (_, (disk, len)) in tail {
            (disk..disk + len).for_each(&mut released);
        }
        if let Some((&start, &mut (disk, ref mut len))) = self.0.iter_mut().next_back()
            && start + *len > count
        {
            let keep = count - start;
            (disk + keep..disk + *len).for_each(&mut released);
            *len = keep;
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn runs(r: &Runs) -> Vec<(u32, u32, u32)> {
        r.iter().collect()
    }

    #[test]
    fn writing_in_order_makes_one_run() {
        let mut r = Runs::default();
        for i in 0..10 {
            r.set(i, 100 + i);
        }
        assert_eq!(runs(&r), [(0, 100, 10)]);
        assert_eq!(
            (r.get(0), r.get(9), r.get(10)),
            (Some(100), Some(109), None)
        );
        assert!(r.any_in(5, 6) && !r.any_in(10, 20));
    }

    #[test]
    fn a_block_moved_splits_and_rejoins() {
        let mut r = Runs::default();
        for i in 0..10 {
            r.set(i, 100 + i);
        }
        r.set(4, 500);
        assert_eq!(runs(&r), [(0, 100, 4), (4, 500, 1), (5, 105, 5)]);
        r.set(4, 104);
        assert_eq!(runs(&r), [(0, 100, 10)], "back in place: one run again");
        // Holes stay holes.
        r.set(20, 7);
        assert_eq!(r.get(15), None);
        assert_eq!(runs(&r), [(0, 100, 10), (20, 7, 1)]);
    }

    #[test]
    fn truncating_releases_the_tail() {
        let mut r = Runs::default();
        assert!(r.push(0, 10, 4));
        assert!(r.push(4, 14, 2), "joins the run before");
        assert!(r.push(8, 50, 3));
        assert!(!r.push(9, 60, 1), "overlaps");
        assert!(!r.push(20, 60, 0), "empty");
        assert_eq!(runs(&r), [(0, 10, 6), (8, 50, 3)]);
        let mut released = Vec::new();
        r.truncate(3, |b| released.push(b));
        assert_eq!(released, [50, 51, 52, 13, 14, 15]);
        assert_eq!(runs(&r), [(0, 10, 3)]);
        r.truncate(0, |_| {});
        assert_eq!(r.len(), 0);
    }
}
