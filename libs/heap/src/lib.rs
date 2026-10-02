//! Kernel heap allocator (ADR-0010).
//!
//! Two tiers over a [`PageSource`] of naturally aligned power-of-two page
//! blocks (the kernel's buddy allocator):
//!
//! - **Small** (size or alignment ≤ 2 KiB): slab caches for 9 power-of-two
//!   size classes, 8 B to 2 KiB. A slab is one page block holding a header and
//!   equal-sized objects, threaded on an intrusive free list. Because slabs are
//!   aligned to their own size, the slab of any object is found by masking its
//!   address. At most one empty slab per class is kept; others go back to the
//!   page source.
//! - **Large**: a dedicated page block of the next power-of-two size, up to
//!   `4 KiB << MAX_ORDER` (4 MiB).
//!
//! The allocator is not thread-safe by itself; the kernel wraps it in a lock.

#![no_std]

#[cfg(test)]
extern crate std;

use core::alloc::Layout;
use core::ptr::{self, NonNull};

pub const PAGE_SIZE: usize = 4096;
/// Largest page block order the page source provides (4 MiB).
pub const MAX_ORDER: u8 = 10;
/// Largest size (and alignment) served by slab caches.
pub const MAX_SMALL: usize = 2048;

const CLASS_COUNT: usize = 9;
const MIN_CLASS_SHIFT: u32 = 3; // 8 bytes
/// Each slab holds at least this many objects (bounds header overhead).
const MIN_OBJECTS_PER_SLAB: usize = 8;

/// Supplier of page blocks.
///
/// # Safety
///
/// `allocate(order)` must return a block of `PAGE_SIZE << order` bytes,
/// aligned to that size, readable and writable, and used by nothing else
/// until passed back to `free`.
pub unsafe trait PageSource {
    fn allocate(&mut self, order: u8) -> Option<NonNull<u8>>;

    /// # Safety
    ///
    /// `block` must come from `allocate(order)` with the same `order` and
    /// must not be used afterwards.
    unsafe fn free(&mut self, block: NonNull<u8>, order: u8);
}

/// Usage statistics, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes handed out by slab caches (rounded to size classes).
    pub small_in_use: usize,
    /// Bytes handed out as large page blocks (rounded to block size).
    pub large_in_use: usize,
    /// Bytes of page blocks held by slab caches, including free objects.
    pub slab_bytes: usize,
}

#[repr(C)]
struct SlabHeader {
    free: *mut FreeObject,
    next: *mut SlabHeader,
    prev: *mut SlabHeader,
    in_use: u32,
    capacity: u32,
}

struct FreeObject {
    next: *mut FreeObject,
}

/// Slabs of one size class that have at least one free object.
#[derive(Clone, Copy)]
struct Cache {
    partial: *mut SlabHeader,
    empty_slabs: usize,
}

impl Cache {
    const EMPTY: Self = Self {
        partial: ptr::null_mut(),
        empty_slabs: 0,
    };
}

pub struct Heap<P> {
    pages: P,
    caches: [Cache; CLASS_COUNT],
    stats: Stats,
}

// SAFETY: the raw pointers refer only to page blocks owned by this heap;
// moving the heap to another CPU moves that ownership with it.
unsafe impl<P: Send> Send for Heap<P> {}

const fn class_size(class: usize) -> usize {
    1 << (class as u32 + MIN_CLASS_SHIFT)
}

/// Page block order of a slab for `class`.
const fn slab_order(class: usize) -> u8 {
    let bytes = class_size(class) * MIN_OBJECTS_PER_SLAB;
    let mut order = 0;
    while (PAGE_SIZE << order) < bytes {
        order += 1;
    }
    order
}

/// Offset of the first object: the header rounded up to the class size, so
/// every object is aligned to its class size.
const fn first_object_offset(class: usize) -> usize {
    size_of::<SlabHeader>().next_multiple_of(class_size(class))
}

/// Size class for `layout`, or `None` for the large tier.
fn class_of(layout: Layout) -> Option<usize> {
    let need = layout.size().max(layout.align()).max(1);
    if need > MAX_SMALL {
        return None;
    }
    let shift = need
        .next_power_of_two()
        .trailing_zeros()
        .max(MIN_CLASS_SHIFT);
    Some((shift - MIN_CLASS_SHIFT) as usize)
}

/// Page block order for a large allocation, or `None` if too large.
fn large_order(layout: Layout) -> Option<u8> {
    let pages = layout.size().max(layout.align()).div_ceil(PAGE_SIZE);
    let order = pages.next_power_of_two().trailing_zeros();
    (order <= u32::from(MAX_ORDER)).then_some(order as u8)
}

impl<P: PageSource> Heap<P> {
    pub const fn new(pages: P) -> Self {
        Self {
            pages,
            caches: [Cache::EMPTY; CLASS_COUNT],
            stats: Stats {
                small_in_use: 0,
                large_in_use: 0,
                slab_bytes: 0,
            },
        }
    }

    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Allocates memory for `layout`. Returns `None` when the page source is
    /// exhausted or the request exceeds the largest page block.
    pub fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        match class_of(layout) {
            Some(class) => self.allocate_small(class),
            None => {
                let order = large_order(layout)?;
                let block = self.pages.allocate(order)?;
                self.stats.large_in_use += PAGE_SIZE << order;
                Some(block)
            }
        }
    }

    /// Frees memory returned by [`allocate`](Self::allocate).
    ///
    /// # Safety
    ///
    /// `ptr` must have been returned by `allocate` on this heap with the same
    /// `layout`, and must not be used afterwards.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        match class_of(layout) {
            // SAFETY: forwarded caller contract.
            Some(class) => unsafe { self.deallocate_small(ptr, class) },
            None => {
                let order = large_order(layout).expect("allocate rejected this layout");
                self.stats.large_in_use -= PAGE_SIZE << order;
                // SAFETY: the block came from `pages.allocate(order)` in
                // `allocate` (same layout → same order) and is no longer used.
                unsafe { self.pages.free(ptr, order) };
            }
        }
    }

    fn allocate_small(&mut self, class: usize) -> Option<NonNull<u8>> {
        if self.caches[class].partial.is_null() {
            self.grow(class)?;
        }
        let cache = &mut self.caches[class];
        let slab = cache.partial;
        // SAFETY: slabs on the partial list are live headers owned by this
        // heap with at least one free object.
        unsafe {
            let object = (*slab).free;
            (*slab).free = (*object).next;
            if (*slab).in_use == 0 {
                cache.empty_slabs -= 1;
            }
            (*slab).in_use += 1;
            if (*slab).free.is_null() {
                unlink(&mut cache.partial, slab);
            }
            self.stats.small_in_use += class_size(class);
            Some(NonNull::new_unchecked(object.cast()))
        }
    }

    /// Adds a new slab to `class`'s cache.
    fn grow(&mut self, class: usize) -> Option<()> {
        let order = slab_order(class);
        let block = self.pages.allocate(order)?.as_ptr();
        let size = class_size(class);
        let offset = first_object_offset(class);
        let capacity = ((PAGE_SIZE << order) - offset) / size;

        // SAFETY: `block` is a fresh, exclusively owned page block of
        // `PAGE_SIZE << order` bytes, aligned to that size; the header and
        // all objects lie within it and are suitably aligned.
        unsafe {
            let mut free: *mut FreeObject = ptr::null_mut();
            for i in (0..capacity).rev() {
                let object = block.add(offset + i * size).cast::<FreeObject>();
                object.write(FreeObject { next: free });
                free = object;
            }
            let slab = block.cast::<SlabHeader>();
            slab.write(SlabHeader {
                free,
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
                in_use: 0,
                capacity: capacity as u32,
            });
            let cache = &mut self.caches[class];
            push(&mut cache.partial, slab);
            cache.empty_slabs += 1;
        }
        self.stats.slab_bytes += PAGE_SIZE << order;
        Some(())
    }

    /// # Safety
    ///
    /// As for [`deallocate`](Self::deallocate), with `class` derived from the
    /// allocation's layout.
    unsafe fn deallocate_small(&mut self, ptr: NonNull<u8>, class: usize) {
        let order = slab_order(class);
        let slab_bytes = PAGE_SIZE << order;
        let address = ptr.as_ptr() as usize;
        let slab = (address & !(slab_bytes - 1)) as *mut SlabHeader;

        // Cheap integrity check: the pointer must be an object slot of this
        // class. A wrong layout or a wild pointer is heap corruption.
        let offset = address - slab as usize;
        let first = first_object_offset(class);
        assert!(
            offset >= first && (offset - first).is_multiple_of(class_size(class)),
            "heap: free of {address:#x} does not match a {}-byte slot",
            class_size(class)
        );

        let cache = &mut self.caches[class];
        // SAFETY: by the caller contract `ptr` is a live object of this
        // class, so `slab` is the live header of its slab.
        unsafe {
            let was_full = (*slab).free.is_null();
            let object = ptr.as_ptr().cast::<FreeObject>();
            object.write(FreeObject { next: (*slab).free });
            (*slab).free = object;
            (*slab).in_use -= 1;
            if was_full {
                push(&mut cache.partial, slab);
            }
            if (*slab).in_use == 0 {
                if cache.empty_slabs >= 1 {
                    unlink(&mut cache.partial, slab);
                    self.stats.slab_bytes -= slab_bytes;
                    self.pages.free(NonNull::new_unchecked(slab.cast()), order);
                } else {
                    cache.empty_slabs += 1;
                }
            }
        }
        self.stats.small_in_use -= class_size(class);
    }
}

/// # Safety
///
/// `slab` must be a live header not currently on any list.
unsafe fn push(head: &mut *mut SlabHeader, slab: *mut SlabHeader) {
    // SAFETY: caller contract; `*head` is null or a live header.
    unsafe {
        (*slab).prev = ptr::null_mut();
        (*slab).next = *head;
        if !(*head).is_null() {
            (**head).prev = slab;
        }
    }
    *head = slab;
}

/// # Safety
///
/// `slab` must be a live header currently on the list starting at `head`.
unsafe fn unlink(head: &mut *mut SlabHeader, slab: *mut SlabHeader) {
    // SAFETY: caller contract; neighbours on the list are live headers.
    unsafe {
        let (prev, next) = ((*slab).prev, (*slab).next);
        if prev.is_null() {
            *head = next;
        } else {
            (*prev).next = next;
        }
        if !next.is_null() {
            (*next).prev = prev;
        }
        (*slab).next = ptr::null_mut();
        (*slab).prev = ptr::null_mut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc, dealloc};
    use std::collections::BTreeMap;
    use std::vec::Vec;

    /// Page source backed by the host allocator; checks every free.
    #[derive(Default)]
    struct HostPages {
        live: BTreeMap<usize, u8>,
        limit: Option<usize>,
    }

    fn block_layout(order: u8) -> Layout {
        let bytes = PAGE_SIZE << order;
        Layout::from_size_align(bytes, bytes).unwrap()
    }

    unsafe impl PageSource for HostPages {
        fn allocate(&mut self, order: u8) -> Option<NonNull<u8>> {
            if self.limit.is_some_and(|limit| self.live.len() >= limit) {
                return None;
            }
            let block = NonNull::new(unsafe { alloc(block_layout(order)) })?;
            self.live.insert(block.as_ptr() as usize, order);
            Some(block)
        }

        unsafe fn free(&mut self, block: NonNull<u8>, order: u8) {
            let recorded = self.live.remove(&(block.as_ptr() as usize));
            assert_eq!(
                recorded,
                Some(order),
                "freed block with wrong order or twice"
            );
            unsafe { dealloc(block.as_ptr(), block_layout(order)) };
        }
    }

    impl Drop for HostPages {
        fn drop(&mut self) {
            for (&addr, &order) in &self.live {
                unsafe { dealloc(addr as *mut u8, block_layout(order)) };
            }
        }
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn size_classes_and_orders() {
        assert_eq!(class_of(layout(1, 1)), Some(0));
        assert_eq!(class_of(layout(8, 8)), Some(0));
        assert_eq!(class_of(layout(9, 1)), Some(1));
        assert_eq!(class_of(layout(24, 8)), Some(2));
        assert_eq!(
            class_of(layout(1, 64)),
            Some(3),
            "alignment raises the class"
        );
        assert_eq!(class_of(layout(2048, 8)), Some(8));
        assert_eq!(class_of(layout(2049, 8)), None);
        assert_eq!(
            class_of(layout(16, 4096)),
            None,
            "page alignment goes large"
        );
        assert_eq!(slab_order(0), 0);
        assert_eq!(slab_order(6), 0, "512 B × 8 = 4 KiB");
        assert_eq!(slab_order(7), 1);
        assert_eq!(slab_order(8), 2);
        assert_eq!(large_order(layout(2049, 8)), Some(0));
        assert_eq!(large_order(layout(5 * PAGE_SIZE, 8)), Some(3));
        assert_eq!(
            large_order(layout(PAGE_SIZE << MAX_ORDER, 8)),
            Some(MAX_ORDER)
        );
        assert_eq!(large_order(layout((PAGE_SIZE << MAX_ORDER) + 1, 8)), None);
    }

    #[test]
    fn small_allocations_are_aligned_and_distinct() {
        let mut heap = Heap::new(HostPages::default());
        let mut ptrs = Vec::new();
        for size in [1, 7, 8, 13, 32, 100, 500, 1000, 2048] {
            for _ in 0..50 {
                let l = layout(size, 1);
                let p = heap.allocate(l).unwrap();
                assert!((p.as_ptr() as usize).is_multiple_of(size.next_power_of_two().max(8)));
                ptrs.push((p, l));
            }
        }
        let mut addrs: Vec<usize> = ptrs.iter().map(|(p, _)| p.as_ptr() as usize).collect();
        addrs.sort_unstable();
        addrs.dedup();
        assert_eq!(addrs.len(), ptrs.len());
        for (p, l) in ptrs {
            unsafe { heap.deallocate(p, l) };
        }
        assert_eq!(heap.stats().small_in_use, 0);
    }

    #[test]
    fn empty_slabs_are_returned_except_one_per_class() {
        let mut heap = Heap::new(HostPages::default());
        let l = layout(64, 8);
        let ptrs: Vec<_> = (0..1000).map(|_| heap.allocate(l).unwrap()).collect();
        let slabs_used = heap.pages.live.len();
        assert!(slabs_used > 10);
        for p in ptrs {
            unsafe { heap.deallocate(p, l) };
        }
        assert_eq!(heap.pages.live.len(), 1, "one empty slab cached");
        assert_eq!(heap.stats().slab_bytes, PAGE_SIZE);
        // The cached slab is reused.
        let p = heap.allocate(l).unwrap();
        assert_eq!(heap.pages.live.len(), 1);
        unsafe { heap.deallocate(p, l) };
    }

    #[test]
    fn large_allocations_use_page_blocks() {
        let mut heap = Heap::new(HostPages::default());
        let l = layout(3 * PAGE_SIZE + 1, 16);
        let p = heap.allocate(l).unwrap();
        assert_eq!(p.as_ptr() as usize % (4 * PAGE_SIZE), 0);
        assert_eq!(heap.stats().large_in_use, 4 * PAGE_SIZE);
        unsafe { heap.deallocate(p, l) };
        assert_eq!(heap.stats().large_in_use, 0);
        assert!(heap.pages.live.is_empty());

        let aligned = layout(64, 8192);
        let p = heap.allocate(aligned).unwrap();
        assert_eq!(p.as_ptr() as usize % 8192, 0);
        unsafe { heap.deallocate(p, aligned) };
    }

    #[test]
    fn exhaustion_and_oversize_return_none() {
        let mut heap = Heap::new(HostPages {
            limit: Some(1),
            live: BTreeMap::new(),
        });
        assert!(
            heap.allocate(layout((PAGE_SIZE << MAX_ORDER) + 1, 8))
                .is_none()
        );
        let l = layout(2048, 8);
        let first: Vec<_> = (0..7).map(|_| heap.allocate(l).unwrap()).collect();
        assert!(
            heap.allocate(l).is_none(),
            "slab full and page source exhausted"
        );
        for p in first {
            unsafe { heap.deallocate(p, l) };
        }
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn misaligned_free_is_detected() {
        let mut heap = Heap::new(HostPages::default());
        let l = layout(64, 8);
        let p = heap.allocate(l).unwrap();
        let inside = unsafe { NonNull::new_unchecked(p.as_ptr().add(8)) };
        unsafe { heap.deallocate(inside, l) };
    }

    /// Random alloc/free mix; every live allocation keeps its fill pattern.
    #[test]
    fn randomized_contents_survive() {
        let mut heap = Heap::new(HostPages::default());
        let mut live: Vec<(NonNull<u8>, Layout, u8)> = Vec::new();
        let mut rng = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let check = |p: NonNull<u8>, l: Layout, tag: u8| {
            let bytes = unsafe { core::slice::from_raw_parts(p.as_ptr(), l.size()) };
            assert!(bytes.iter().all(|&b| b == tag), "allocation corrupted");
        };

        for step in 0..30_000u32 {
            if live.is_empty() || next() % 5 < 3 {
                let size = match next() % 10 {
                    0 => (next() % 20_000) as usize + 1,
                    _ => (next() % 2048) as usize + 1,
                };
                let align = 1 << (next() % 7);
                let l = layout(size, align);
                let p = heap.allocate(l).unwrap();
                assert_eq!(p.as_ptr() as usize % align, 0);
                let tag = step as u8;
                unsafe { ptr::write_bytes(p.as_ptr(), tag, size) };
                live.push((p, l, tag));
            } else {
                let i = (next() as usize) % live.len();
                let (p, l, tag) = live.swap_remove(i);
                check(p, l, tag);
                unsafe { heap.deallocate(p, l) };
            }
        }
        for (p, l, tag) in live.drain(..) {
            check(p, l, tag);
            unsafe { heap.deallocate(p, l) };
        }
        let stats = heap.stats();
        assert_eq!((stats.small_in_use, stats.large_in_use), (0, 0));
        assert!(
            heap.pages.live.len() <= CLASS_COUNT,
            "only cached empty slabs remain"
        );
    }
}
