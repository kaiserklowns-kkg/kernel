//! Which blocks are in use (ADR-0142): a bit each, so a 500 GB volume
//! costs 15 MiB of memory rather than a byte a block; and apart, the few
//! allocated or let go since the last commit, so a commit costs what
//! changed, not a walk over the whole volume.

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

/// A block's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Free,
    /// Referenced by the durable state (or by live nodes since).
    Used,
    /// Allocated since the last commit: may be overwritten in place.
    Fresh,
    /// No longer referenced, but the durable state still does: freed at
    /// the next commit.
    Pending,
}

pub(crate) struct Blocks {
    total: u64,
    /// The first blocks, never allocated (the superblocks).
    reserved: u64,
    /// A bit a block: not free.
    taken: Vec<u64>,
    fresh: BTreeSet<u32>,
    pending: BTreeSet<u32>,
    free: u64,
    /// Where the next search for a free block starts.
    cursor: u64,
}

impl Blocks {
    /// `total` blocks, the first `reserved` of them in use for good.
    pub(crate) fn new(total: u64, reserved: u64) -> Self {
        let mut blocks = Self {
            total,
            reserved: reserved.min(total),
            taken: vec![0; total.div_ceil(64) as usize],
            fresh: BTreeSet::new(),
            pending: BTreeSet::new(),
            free: total,
            cursor: reserved,
        };
        for block in 0..blocks.reserved {
            blocks.set_taken(block, true);
        }
        blocks.free -= blocks.reserved;
        blocks
    }

    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn free(&self) -> u64 {
        self.free
    }

    fn is_taken(&self, block: u64) -> bool {
        self.taken[(block / 64) as usize] & (1 << (block % 64)) != 0
    }

    fn set_taken(&mut self, block: u64, taken: bool) {
        let word = &mut self.taken[(block / 64) as usize];
        if taken {
            *word |= 1 << (block % 64);
        } else {
            *word &= !(1 << (block % 64));
        }
    }

    pub(crate) fn state(&self, block: u32) -> State {
        if u64::from(block) >= self.total || !self.is_taken(u64::from(block)) {
            State::Free
        } else if self.fresh.contains(&block) {
            State::Fresh
        } else if self.pending.contains(&block) {
            State::Pending
        } else {
            State::Used
        }
    }

    pub(crate) fn is_fresh(&self, block: u32) -> bool {
        self.fresh.contains(&block)
    }

    /// Marks a block the metadata references as used, exactly once.
    pub(crate) fn claim(&mut self, block: u32) -> Result<(), &'static str> {
        let block = u64::from(block);
        if block < self.reserved || block >= self.total {
            return Err("block number out of range");
        }
        if self.is_taken(block) {
            return Err("block referenced twice");
        }
        self.set_taken(block, true);
        self.free -= 1;
        Ok(())
    }

    /// The first free block at or after `from`, wrapping around.
    fn find_free(&self, from: u64) -> Option<u64> {
        let words = self.taken.len();
        if words == 0 || self.free == 0 {
            return None;
        }
        let from = if from < self.total { from } else { 0 };
        let start = (from / 64) as usize;
        for step in 0..=words {
            let index = (start + step) % words;
            let mut bits = !self.taken[index];
            if step == 0 {
                bits &= !0u64 << (from % 64);
            }
            if index == words - 1 && !self.total.is_multiple_of(64) {
                bits &= (1u64 << (self.total % 64)) - 1;
            }
            if bits != 0 {
                return Some(index as u64 * 64 + u64::from(bits.trailing_zeros()));
            }
        }
        None
    }

    /// A free block, now fresh: `near` if it is free (a file grows on
    /// where its last block is), else the next free one on from there or
    /// from where the last allocation ended.
    pub(crate) fn allocate(&mut self, near: Option<u32>) -> Option<u32> {
        let from = near.map_or(self.cursor, u64::from);
        let block = self.find_free(from)?;
        self.set_taken(block, true);
        self.free -= 1;
        self.cursor = block + 1;
        let block = block as u32;
        self.fresh.insert(block);
        Some(block)
    }

    /// No longer referenced: a fresh block is free again at once, a used
    /// one once the next commit is durable.
    pub(crate) fn release(&mut self, block: u32) {
        match self.state(block) {
            State::Fresh => {
                self.fresh.remove(&block);
                self.set_taken(u64::from(block), false);
                self.free += 1;
            }
            State::Used => {
                self.pending.insert(block);
            }
            other => debug_assert!(false, "releasing block {block}, {other:?}"),
        }
    }

    /// A new generation is durable: what it references is used, what only
    /// the old one referenced is free.
    pub(crate) fn committed(&mut self) {
        self.fresh.clear();
        for block in core::mem::take(&mut self.pending) {
            self.set_taken(u64::from(block), false);
            self.free += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_release_and_commit() {
        let mut blocks = Blocks::new(130, 2);
        assert_eq!((blocks.total(), blocks.free()), (130, 128));
        assert_eq!(blocks.claim(1), Err("block number out of range"));
        assert_eq!(blocks.claim(130), Err("block number out of range"));
        blocks.claim(5).unwrap();
        assert_eq!(blocks.claim(5), Err("block referenced twice"));
        assert_eq!(blocks.state(5), State::Used);

        // From the cursor, skipping what is taken; near a block if asked.
        assert_eq!(blocks.allocate(None), Some(2));
        assert_eq!(blocks.allocate(Some(4)), Some(4));
        assert_eq!(blocks.allocate(Some(5)), Some(6), "5 is taken");
        assert_eq!(blocks.state(6), State::Fresh);
        assert_eq!(blocks.allocate(Some(129)), Some(129), "the last block");
        assert_eq!(blocks.allocate(Some(129)), Some(3), "wrapped around");

        // Fresh: free at once. Used: free after the commit.
        blocks.release(6);
        assert_eq!(blocks.state(6), State::Free);
        blocks.release(5);
        assert_eq!(blocks.state(5), State::Pending);
        let free = blocks.free();
        blocks.committed();
        assert_eq!(blocks.state(5), State::Free);
        assert_eq!(blocks.state(4), State::Used);
        assert_eq!(blocks.free(), free + 1);
    }

    #[test]
    fn a_full_volume_has_nothing_to_give() {
        let mut blocks = Blocks::new(70, 2);
        let mut got = 0;
        while blocks.allocate(None).is_some() {
            got += 1;
        }
        assert_eq!((got, blocks.free()), (68, 0));
        blocks.release(40);
        assert_eq!(blocks.allocate(Some(3)), Some(40));
    }
}
