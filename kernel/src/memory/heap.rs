//! Kernel heap: `alloc` (`Box`, `Vec`, `BTreeMap`, …) for the kernel
//! (ADR-0010).
//!
//! `oceans-heap` slab caches and page blocks, backed by the buddy frame
//! allocator and addressed through the direct map. Allocations made before
//! [`init`] fail (and panic via the default allocation error handler).
//!
//! Lock order: heap → frames. The frame allocator never allocates from the
//! heap, so this cannot deadlock.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, Ordering};

use oceans_frame_allocator::Frame;
use oceans_heap::{Heap, PageSource, Stats};
use spin::Mutex;

use super::{frames, phys_to_virt, virt_to_phys};
use crate::{arch, klog};

/// Page blocks from the frame allocator, reached through the direct map.
struct DirectMapPages;

// SAFETY: buddy blocks of `order` are `4 KiB << order` bytes, naturally
// aligned physically; the direct map offset is 2 MiB aligned (ADR-0009), so
// virtual alignment is preserved up to the 4 MiB maximum block. Allocated
// frames are exclusively ours and mapped read-write in the direct map.
unsafe impl PageSource for DirectMapPages {
    fn allocate(&mut self, order: u8) -> Option<NonNull<u8>> {
        let frame = frames::allocate_frames(order).ok()?;
        NonNull::new(phys_to_virt(frame.addr()))
    }

    unsafe fn free(&mut self, block: NonNull<u8>, _order: u8) {
        let frame = Frame::from_addr(virt_to_phys(block.as_ptr()))
            .expect("heap page blocks are page-aligned");
        if let Err(err) = frames::free_frames(frame) {
            panic!("heap returned a block the frame allocator rejects: {err:?}");
        }
    }
}

pub struct KernelHeap {
    ready: AtomicBool,
    heap: Mutex<Heap<DirectMapPages>>,
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap {
    ready: AtomicBool::new(false),
    heap: Mutex::new(Heap::new(DirectMapPages)),
};

// SAFETY: allocation and deallocation are serialised by the lock (with
// interrupts disabled), and `oceans-heap` upholds the GlobalAlloc contract:
// returned blocks fit the layout, are aligned and are not handed out twice.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.ready.load(Ordering::Acquire) {
            return ptr::null_mut();
        }
        arch::without_interrupts(|| self.heap.lock().allocate(layout))
            .map_or(ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(ptr) = NonNull::new(ptr) else {
            return;
        };
        // SAFETY: GlobalAlloc's contract: `ptr` came from `alloc` with `layout`.
        arch::without_interrupts(|| unsafe { self.heap.lock().deallocate(ptr, layout) });
    }
}

/// Enables the heap. Requires the frame allocator and kernel page tables.
pub fn init() {
    HEAP.ready.store(true, Ordering::Release);
    klog::info!("kernel heap ready");
}

pub fn stats() -> Stats {
    arch::without_interrupts(|| HEAP.heap.lock().stats())
}

/// Exercises the heap through the `alloc` collections.
pub fn self_test() {
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::vec::Vec;

    let before = stats();
    {
        let boxed = Box::new([0xa5u8; 100]);
        assert!(boxed.iter().all(|&b| b == 0xa5));

        let mut numbers: Vec<u64> = (0..10_000).collect();
        numbers.retain(|n| n % 3 == 0);
        assert_eq!(
            numbers.iter().sum::<u64>(),
            (0..10_000).filter(|n| n % 3 == 0).sum()
        );

        let mut map = BTreeMap::new();
        for i in 0..1_000u32 {
            map.insert(i, String::from("oceans"));
        }
        assert_eq!(map.len(), 1_000);

        // Large tier: 1 MiB block.
        let big = alloc::vec![7u8; 1024 * 1024];
        assert_eq!(big[big.len() - 1], 7);
    }
    let after = stats();
    assert_eq!(
        after.small_in_use, before.small_in_use,
        "heap self-test leaked"
    );
    assert_eq!(
        after.large_in_use, before.large_in_use,
        "heap self-test leaked"
    );
    klog::info!("heap self-test passed");
}
