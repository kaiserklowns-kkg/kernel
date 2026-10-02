//! Buddy allocator for physical memory frames (ADR-0008).
//!
//! Pure bookkeeping: the allocator never reads or writes the frames it hands
//! out, so it is fully testable on the host. The kernel supplies the metadata
//! storage (one [`FrameInfo`] per frame, placed in RAM through the direct
//! map) and is responsible for what it does with allocated frames.
//!
//! Blocks are `2^order` contiguous frames, naturally aligned in physical
//! memory, for orders `0..=MAX_ORDER` (4 KiB to 4 MiB). Each order has a
//! doubly linked free list threaded through the metadata, so allocation,
//! splitting, freeing and coalescing are all O(MAX_ORDER).

#![no_std]

#[cfg(test)]
extern crate std;

use core::mem::{MaybeUninit, size_of};
use core::ops::Range;

use oceans_memory_map::{PAGE_SIZE, Region, RegionKind};

/// Largest block order: 2^10 frames = 4 MiB.
pub const MAX_ORDER: u8 = 10;
const ORDER_COUNT: usize = MAX_ORDER as usize + 1;
const MAX_BLOCK_FRAMES: u64 = 1 << MAX_ORDER;

/// List terminator. Frame indices are therefore limited to `u32::MAX - 1`
/// (16 TiB of physical address span), checked in [`Layout::for_regions`].
const NONE: u32 = u32::MAX;

/// A naturally aligned 4 KiB physical frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Frame(u64);

impl Frame {
    /// The frame starting at `addr`, if `addr` is page-aligned.
    pub const fn from_addr(addr: u64) -> Option<Self> {
        if addr.is_multiple_of(PAGE_SIZE) {
            Some(Self(addr))
        } else {
            None
        }
    }

    /// Physical start address.
    pub const fn addr(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum State {
    /// Not RAM, reserved, or excluded: never handed out.
    Unavailable,
    /// Inside a block whose head is another frame.
    Body,
    /// First frame of a free block of `order`; linked into a free list.
    FreeHead,
    /// First frame of an allocated block of `order`.
    AllocatedHead,
}

/// Per-frame metadata. 12 bytes per 4 KiB frame (≈0.3% of RAM).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameInfo {
    next: u32,
    prev: u32,
    state: State,
    order: u8,
    /// Reserved for the reference count needed by shared and copy-on-write
    /// mappings (Phase 2, ADR-0008).
    _reserved: u16,
}

const _: () = assert!(size_of::<FrameInfo>() == 12);

impl FrameInfo {
    const UNAVAILABLE: Self = Self {
        next: NONE,
        prev: NONE,
        state: State::Unavailable,
        order: 0,
        _reserved: 0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// The memory map has no complete usable page.
    NoUsableMemory,
    /// The usable physical span needs more frame indices than supported.
    TooManyFrames { frames: u64 },
    /// The metadata storage has fewer entries than the layout requires.
    MetadataTooSmall { needed: usize, provided: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    /// `order` is larger than [`MAX_ORDER`].
    OrderTooLarge(u8),
    /// No free block of the requested order or larger.
    OutOfMemory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    /// The frame lies outside the memory managed by this allocator.
    NotManaged(Frame),
    /// The frame is not the start of an allocated block: a double free, a
    /// pointer into the middle of a block, or a reserved frame.
    NotAllocated(Frame),
}

/// Which physical frames the metadata array describes.
///
/// It spans from the lowest to the highest usable page. The base is rounded
/// down to a `MAX_ORDER` block boundary so that buddy arithmetic on indices
/// matches physical alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    base_frame: u64,
    frame_count: usize,
}

impl Layout {
    pub fn for_regions(regions: &[Region]) -> Result<Self, InitError> {
        let mut span: Option<(u64, u64)> = None;
        for region in regions.iter().filter(|r| r.kind() == RegionKind::Usable) {
            if let Some((start, pages)) = region.whole_pages() {
                let (lo, hi) = (start / PAGE_SIZE, start / PAGE_SIZE + pages);
                span = Some(match span {
                    Some((min, max)) => (min.min(lo), max.max(hi)),
                    None => (lo, hi),
                });
            }
        }
        let (first, end) = span.ok_or(InitError::NoUsableMemory)?;
        let base_frame = first - first % MAX_BLOCK_FRAMES;
        let frames = end - base_frame;
        if frames >= u64::from(NONE) {
            return Err(InitError::TooManyFrames { frames });
        }
        Ok(Self {
            base_frame,
            frame_count: frames as usize,
        })
    }

    /// Number of [`FrameInfo`] entries the allocator needs.
    pub const fn frame_count(&self) -> usize {
        self.frame_count
    }

    /// Bytes of metadata, rounded up to whole pages.
    pub const fn metadata_bytes(&self) -> u64 {
        let bytes = self.frame_count as u64 * size_of::<FrameInfo>() as u64;
        bytes.div_ceil(PAGE_SIZE) * PAGE_SIZE
    }

    /// Physical address range the metadata describes.
    pub const fn span(&self) -> Range<u64> {
        self.base_frame * PAGE_SIZE..(self.base_frame + self.frame_count as u64) * PAGE_SIZE
    }
}

/// Picks a page-aligned physical range of `bytes` inside usable RAM and
/// outside `avoid`, from the largest suitable piece.
pub fn place_metadata(regions: &[Region], bytes: u64, avoid: &[Range<u64>]) -> Option<Range<u64>> {
    let mut best: Option<Range<u64>> = None;
    for_each_usable_piece(regions, avoid, &mut |piece| {
        let fits = piece.end - piece.start >= bytes;
        let larger = best
            .as_ref()
            .is_none_or(|b| piece.end - piece.start > b.end - b.start);
        if fits && larger {
            best = Some(piece);
        }
    });
    best.map(|piece| piece.start..piece.start + bytes)
}

/// Calls `f` with every page-aligned, non-empty piece of usable RAM that
/// does not overlap `exclusions`.
fn for_each_usable_piece(
    regions: &[Region],
    exclusions: &[Range<u64>],
    f: &mut impl FnMut(Range<u64>),
) {
    for region in regions.iter().filter(|r| r.kind() == RegionKind::Usable) {
        if let Some((start, pages)) = region.whole_pages() {
            subtract(start..start + pages * PAGE_SIZE, exclusions, f);
        }
    }
}

fn subtract(range: Range<u64>, exclusions: &[Range<u64>], f: &mut impl FnMut(Range<u64>)) {
    let start = range.start.next_multiple_of(PAGE_SIZE);
    let end = range.end - range.end % PAGE_SIZE;
    if start >= end {
        return;
    }
    match exclusions
        .iter()
        .find(|ex| ex.start < end && start < ex.end)
    {
        None => f(start..end),
        Some(ex) => {
            // Each side no longer overlaps `ex`, so recursion terminates.
            subtract(start..ex.start.max(start), exclusions, f);
            subtract(ex.end.min(end)..end, exclusions, f);
        }
    }
}

/// Allocator statistics, in frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames the allocator may hand out (usable RAM minus exclusions).
    pub managed_frames: u64,
    pub free_frames: u64,
}

pub struct FrameAllocator<'m> {
    layout: Layout,
    frames: &'m mut [FrameInfo],
    free_lists: [u32; ORDER_COUNT],
    stats: Stats,
}

impl<'m> FrameAllocator<'m> {
    /// Builds an allocator managing every usable page in `regions` except
    /// those overlapping `exclusions` (which must include the metadata
    /// storage itself if it lives in usable RAM).
    pub fn new(
        layout: Layout,
        storage: &'m mut [MaybeUninit<FrameInfo>],
        regions: &[Region],
        exclusions: &[Range<u64>],
    ) -> Result<Self, InitError> {
        let provided = storage.len();
        let Some(storage) = storage.get_mut(..layout.frame_count) else {
            return Err(InitError::MetadataTooSmall {
                needed: layout.frame_count,
                provided,
            });
        };
        for slot in storage.iter_mut() {
            slot.write(FrameInfo::UNAVAILABLE);
        }
        // SAFETY: every element was initialised by the loop above, and
        // `MaybeUninit<T>` has the same layout as `T`.
        let frames =
            unsafe { &mut *(storage as *mut [MaybeUninit<FrameInfo>] as *mut [FrameInfo]) };

        let mut allocator = Self {
            layout,
            frames,
            free_lists: [NONE; ORDER_COUNT],
            stats: Stats::default(),
        };
        let span = layout.span();
        for_each_usable_piece(regions, exclusions, &mut |piece| {
            let start = piece.start.max(span.start);
            let end = piece.end.min(span.end);
            if start < end {
                allocator
                    .add_free_range(allocator.index_of_addr(start), allocator.index_of_addr(end));
            }
        });
        Ok(allocator)
    }

    pub const fn stats(&self) -> Stats {
        self.stats
    }

    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// Allocates a block of `2^order` contiguous frames aligned to its size.
    pub fn allocate(&mut self, order: u8) -> Result<Frame, AllocError> {
        if order > MAX_ORDER {
            return Err(AllocError::OrderTooLarge(order));
        }
        let mut current = (order..=MAX_ORDER)
            .find(|&k| self.free_lists[usize::from(k)] != NONE)
            .ok_or(AllocError::OutOfMemory)?;

        let index = self.free_lists[usize::from(current)] as usize;
        self.unlink(current, index);

        // Split down to the requested size, returning upper halves.
        while current > order {
            current -= 1;
            let buddy = index + (1 << current);
            self.set_head(buddy, State::FreeHead, current);
            self.link(current, buddy);
        }

        self.set_head(index, State::AllocatedHead, order);
        self.stats.free_frames -= 1 << order;
        Ok(self.frame_at(index))
    }

    /// Frees a block previously returned by [`allocate`](Self::allocate).
    /// The block's order is taken from the metadata.
    pub fn free(&mut self, frame: Frame) -> Result<(), FreeError> {
        let index = self.index_of(frame).ok_or(FreeError::NotManaged(frame))?;
        let info = self.frames[index];
        if info.state != State::AllocatedHead {
            return Err(FreeError::NotAllocated(frame));
        }
        self.stats.free_frames += 1 << info.order;
        self.release_block(index, info.order);
        Ok(())
    }

    /// Number of free blocks of exactly `order`. Walks the list; for
    /// diagnostics and tests.
    pub fn free_blocks(&self, order: u8) -> usize {
        let mut count = 0;
        let mut cursor = self
            .free_lists
            .get(usize::from(order))
            .copied()
            .unwrap_or(NONE);
        while cursor != NONE {
            count += 1;
            cursor = self.frames[cursor as usize].next;
        }
        count
    }

    /// Adds frames `[start, end)` (indices) as free, in maximal aligned blocks.
    fn add_free_range(&mut self, mut start: usize, end: usize) {
        while start < end {
            let order = (0..=MAX_ORDER)
                .rev()
                .find(|&k| start.is_multiple_of(1 << k) && start + (1 << k) <= end)
                .unwrap_or(0);
            let len = 1usize << order;
            for info in &mut self.frames[start + 1..start + len] {
                info.state = State::Body;
            }
            self.stats.managed_frames += len as u64;
            self.stats.free_frames += len as u64;
            // Coalesces with neighbours added by earlier, adjacent ranges.
            self.release_block(start, order);
            start += len;
        }
    }

    /// Inserts a block into the free lists, merging with free buddies.
    fn release_block(&mut self, mut index: usize, mut order: u8) {
        while order < MAX_ORDER {
            let buddy = index ^ (1 << order);
            match self.frames.get(buddy) {
                Some(info) if info.state == State::FreeHead && info.order == order => {}
                _ => break,
            }
            self.unlink(order, buddy);
            let (low, high) = (index.min(buddy), index.max(buddy));
            self.frames[high].state = State::Body;
            index = low;
            order += 1;
        }
        self.set_head(index, State::FreeHead, order);
        self.link(order, index);
    }

    fn set_head(&mut self, index: usize, state: State, order: u8) {
        let info = &mut self.frames[index];
        info.state = state;
        info.order = order;
    }

    fn link(&mut self, order: u8, index: usize) {
        let head = &mut self.free_lists[usize::from(order)];
        let next = *head;
        // `index < frame_count < NONE` (Layout::for_regions), so it fits.
        *head = index as u32;
        let info = &mut self.frames[index];
        info.prev = NONE;
        info.next = next;
        if next != NONE {
            self.frames[next as usize].prev = index as u32;
        }
    }

    fn unlink(&mut self, order: u8, index: usize) {
        let FrameInfo { prev, next, .. } = self.frames[index];
        if prev == NONE {
            self.free_lists[usize::from(order)] = next;
        } else {
            self.frames[prev as usize].next = next;
        }
        if next != NONE {
            self.frames[next as usize].prev = prev;
        }
        let info = &mut self.frames[index];
        info.next = NONE;
        info.prev = NONE;
    }

    fn frame_at(&self, index: usize) -> Frame {
        Frame((self.layout.base_frame + index as u64) * PAGE_SIZE)
    }

    fn index_of(&self, frame: Frame) -> Option<usize> {
        let number = frame.addr() / PAGE_SIZE;
        let index = number.checked_sub(self.layout.base_frame)?;
        (index < self.layout.frame_count as u64).then_some(index as usize)
    }

    /// Index for a page-aligned address known to lie within the span (or at
    /// its end).
    fn index_of_addr(&self, addr: u64) -> usize {
        (addr / PAGE_SIZE - self.layout.base_frame) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::vec;
    use std::vec::Vec;

    const MIB: u64 = 1024 * 1024;

    fn usable(base: u64, length: u64) -> Region {
        Region::new(base, length, RegionKind::Usable).unwrap()
    }

    fn reserved(base: u64, length: u64) -> Region {
        Region::new(base, length, RegionKind::Reserved).unwrap()
    }

    /// Owns metadata storage and builds allocators over it.
    struct Harness {
        layout: Layout,
        storage: Vec<MaybeUninit<FrameInfo>>,
    }

    impl Harness {
        fn new(regions: &[Region]) -> Self {
            let layout = Layout::for_regions(regions).unwrap();
            let storage = vec![MaybeUninit::uninit(); layout.frame_count()];
            Self { layout, storage }
        }

        fn allocator(
            &mut self,
            regions: &[Region],
            exclusions: &[Range<u64>],
        ) -> FrameAllocator<'_> {
            FrameAllocator::new(self.layout, &mut self.storage, regions, exclusions).unwrap()
        }
    }

    fn free_block_histogram(a: &FrameAllocator<'_>) -> Vec<usize> {
        (0..=MAX_ORDER).map(|k| a.free_blocks(k)).collect()
    }

    #[test]
    fn layout_spans_usable_memory_from_aligned_base() {
        let regions = [
            usable(0x5000, 0x3000),
            reserved(0x8000, 0x1000),
            usable(6 * MIB, MIB),
        ];
        let layout = Layout::for_regions(&regions).unwrap();
        assert_eq!(layout.span(), 0..7 * MIB);
        assert_eq!(layout.frame_count(), (7 * MIB / PAGE_SIZE) as usize);
        assert_eq!(layout.metadata_bytes() % PAGE_SIZE, 0);
        assert!(layout.metadata_bytes() >= layout.frame_count() as u64 * 12);
    }

    #[test]
    fn layout_requires_usable_memory() {
        assert_eq!(
            Layout::for_regions(&[reserved(0, MIB)]),
            Err(InitError::NoUsableMemory)
        );
        assert_eq!(
            Layout::for_regions(&[usable(0x100, 0x800)]),
            Err(InitError::NoUsableMemory)
        );
    }

    #[test]
    fn rejects_short_metadata() {
        let regions = [usable(0, 4 * MIB)];
        let layout = Layout::for_regions(&regions).unwrap();
        let mut storage = vec![MaybeUninit::uninit(); 10];
        assert_eq!(
            FrameAllocator::new(layout, &mut storage, &regions, &[]).err(),
            Some(InitError::MetadataTooSmall {
                needed: 1024,
                provided: 10
            })
        );
    }

    #[test]
    fn aligned_region_becomes_max_order_blocks() {
        let regions = [usable(8 * MIB, 8 * MIB)];
        let mut h = Harness::new(&regions);
        let a = h.allocator(&regions, &[]);
        assert_eq!(
            a.stats(),
            Stats {
                managed_frames: 2048,
                free_frames: 2048
            }
        );
        assert_eq!(a.free_blocks(MAX_ORDER), 2);
    }

    #[test]
    fn adjacent_regions_coalesce() {
        let regions = [usable(4 * MIB, 2 * MIB), usable(6 * MIB, 2 * MIB)];
        let mut h = Harness::new(&regions);
        let a = h.allocator(&regions, &[]);
        assert_eq!(a.free_blocks(MAX_ORDER), 1);
        assert_eq!(a.stats().free_frames, 1024);
    }

    #[test]
    fn split_leaves_one_buddy_per_order() {
        let regions = [usable(0, 4 * MIB)];
        let mut h = Harness::new(&regions);
        let mut a = h.allocator(&regions, &[]);
        let frame = a.allocate(0).unwrap();
        assert_eq!(frame.addr(), 0);
        let mut expected = vec![1; MAX_ORDER as usize];
        expected.push(0);
        assert_eq!(free_block_histogram(&a), expected);

        a.free(frame).unwrap();
        assert_eq!(a.free_blocks(MAX_ORDER), 1);
        assert_eq!(a.stats().free_frames, 1024);
    }

    #[test]
    fn blocks_are_naturally_aligned() {
        let regions = [usable(0x1000, 16 * MIB)];
        let mut h = Harness::new(&regions);
        let mut a = h.allocator(&regions, &[]);
        for order in [3, 0, 5, 1, MAX_ORDER, 2] {
            let frame = a.allocate(order).unwrap();
            assert_eq!(frame.addr() % (PAGE_SIZE << order), 0, "order {order}");
        }
    }

    #[test]
    fn rejects_bad_frees() {
        let regions = [usable(4 * MIB, 4 * MIB)];
        let mut h = Harness::new(&regions);
        let mut a = h.allocator(&regions, &[]);
        let block = a.allocate(2).unwrap();
        let inner = Frame::from_addr(block.addr() + PAGE_SIZE).unwrap();
        let outside = Frame::from_addr(64 * MIB).unwrap();
        let below = Frame::from_addr(0).unwrap();

        assert_eq!(a.free(inner), Err(FreeError::NotAllocated(inner)));
        assert_eq!(a.free(outside), Err(FreeError::NotManaged(outside)));
        assert_eq!(a.free(below), Err(FreeError::NotManaged(below)));
        assert_eq!(a.free(block), Ok(()));
        assert_eq!(
            a.free(block),
            Err(FreeError::NotAllocated(block)),
            "double free"
        );
        assert_eq!(a.stats().free_frames, 1024);
    }

    #[test]
    fn order_limits_and_exhaustion() {
        let regions = [usable(0, 64 * 1024)];
        let mut h = Harness::new(&regions);
        let mut a = h.allocator(&regions, &[]);
        assert_eq!(
            a.allocate(MAX_ORDER + 1),
            Err(AllocError::OrderTooLarge(MAX_ORDER + 1))
        );
        assert_eq!(
            a.allocate(5),
            Err(AllocError::OutOfMemory),
            "only 16 frames exist"
        );
        let block = a.allocate(4).unwrap();
        assert_eq!(a.allocate(0), Err(AllocError::OutOfMemory));
        a.free(block).unwrap();
        assert!(a.allocate(0).is_ok());
    }

    #[test]
    fn exclusions_and_reserved_memory_are_never_returned() {
        let regions = [
            usable(0, 0x9f000),
            reserved(0x9f000, 0x61000),
            usable(MIB, 7 * MIB),
            usable(9 * MIB + 0x800, MIB), // unaligned edges are trimmed
        ];
        let exclusions = [0..MIB, 3 * MIB..3 * MIB + 0x5000];
        let mut h = Harness::new(&regions);
        let mut a = h.allocator(&regions, &exclusions);

        let expected_frames = (7 * MIB - 0x5000) / PAGE_SIZE + (MIB - PAGE_SIZE) / PAGE_SIZE;
        assert_eq!(a.stats().managed_frames, expected_frames);

        let mut seen = Vec::new();
        while let Ok(frame) = a.allocate(0) {
            let addr = frame.addr();
            assert!(addr >= MIB, "{addr:#x} in excluded low memory");
            assert!(
                !(3 * MIB..3 * MIB + 0x5000).contains(&addr),
                "{addr:#x} excluded"
            );
            assert!(
                (MIB..8 * MIB).contains(&addr) || (9 * MIB + PAGE_SIZE..10 * MIB).contains(&addr)
            );
            seen.push(addr);
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len() as u64,
            expected_frames,
            "every frame exactly once"
        );
    }

    #[test]
    fn place_metadata_uses_largest_piece_outside_avoided_ranges() {
        let regions = [
            usable(0, 0x9f000),
            usable(MIB, 2 * MIB),
            usable(16 * MIB, 4 * MIB),
        ];
        let avoid = core::slice::from_ref(&(16 * MIB..17 * MIB));
        let placed = place_metadata(&regions, 0x3000, avoid).unwrap();
        assert_eq!(placed, 17 * MIB..17 * MIB + 0x3000);
        assert_eq!(place_metadata(&regions, 8 * MIB, avoid), None);
    }

    /// Random allocate/free sequence checked against a shadow model.
    #[test]
    fn randomized_against_shadow_model() {
        let regions = [
            usable(0x2000, 3 * MIB),
            reserved(3 * MIB + 0x2000, MIB),
            usable(5 * MIB, 11 * MIB),
        ];
        let mut h = Harness::new(&regions);
        let mut a = h.allocator(&regions, &[]);
        let initial_stats = a.stats();
        let initial_histogram = free_block_histogram(&a);

        // Allocated blocks: start address → order.
        let mut live: BTreeMap<u64, u8> = BTreeMap::new();
        let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        for _ in 0..20_000 {
            if live.is_empty() || next() % 3 != 0 {
                let order = (next() % 6) as u8;
                if let Ok(frame) = a.allocate(order) {
                    let start = frame.addr();
                    let end = start + (PAGE_SIZE << order);
                    assert_eq!(start % (PAGE_SIZE << order), 0);
                    // No overlap with the neighbouring live blocks.
                    if let Some((&prev, &prev_order)) = live.range(..start).next_back() {
                        assert!(
                            prev + (PAGE_SIZE << prev_order) <= start,
                            "overlap below {start:#x}"
                        );
                    }
                    if let Some((&following, _)) = live.range(start..).next() {
                        assert!(end <= following, "overlap above {start:#x}");
                    }
                    live.insert(start, order);
                }
            } else {
                let pick = (next() as usize) % live.len();
                let (&start, _) = live.iter().nth(pick).unwrap();
                live.remove(&start);
                a.free(Frame::from_addr(start).unwrap()).unwrap();
            }
            let used: u64 = live.values().map(|&o| 1u64 << o).sum();
            assert_eq!(a.stats().free_frames + used, initial_stats.managed_frames);
        }

        for &start in live.keys() {
            a.free(Frame::from_addr(start).unwrap()).unwrap();
        }
        assert_eq!(a.stats(), initial_stats);
        assert_eq!(
            free_block_histogram(&a),
            initial_histogram,
            "fully coalesced again"
        );
    }
}
