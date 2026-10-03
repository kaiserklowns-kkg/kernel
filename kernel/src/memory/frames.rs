//! Kernel physical frame allocator (ADR-0008).
//!
//! Wraps `oceans-frame-allocator` with its metadata placed in usable RAM and
//! reached through the direct map, behind an interrupt-safe lock.

use core::mem::MaybeUninit;
use core::ops::Range;

use oceans_frame_allocator::{
    AllocError, Frame, FrameAllocator, FrameInfo, FreeError, Layout, place_metadata,
};
use oceans_memory_map::{PAGE_SIZE, RegionKind};
use spin::{Mutex, Once};

use super::{MIB, phys_to_virt};
use crate::boot::BootInfo;
use crate::{arch, klog};

/// Physical memory below 1 MiB is never allocated: it holds firmware data on
/// some machines and is needed later for the SMP real-mode trampoline.
const LOW_MEMORY: Range<u64> = 0..MIB;

static FRAMES: Once<Mutex<FrameAllocator<'static>>> = Once::new();

pub fn init(boot: &BootInfo) {
    let regions = boot.memory_regions();
    let layout = match Layout::for_regions(regions) {
        Ok(layout) => layout,
        Err(err) => panic!("cannot lay out frame metadata: {err:?}"),
    };
    let Some(metadata) = place_metadata(regions, layout.metadata_bytes(), &[LOW_MEMORY]) else {
        panic!(
            "no usable region can hold {} KiB of frame metadata",
            layout.metadata_bytes() / 1024
        );
    };

    let storage_ptr = phys_to_virt(metadata.start).cast::<MaybeUninit<FrameInfo>>();
    // SAFETY: `metadata` lies in usable RAM (free per the boot memory map),
    // is excluded from the allocator below so it is never handed out, and is
    // mapped read-write by the direct map for the kernel's whole lifetime.
    // It is page-aligned, which satisfies `FrameInfo`'s alignment, and spans
    // `metadata_bytes() >= frame_count * size_of::<FrameInfo>()` bytes.
    // `MaybeUninit` makes the slice valid without initialisation.
    let storage: &'static mut [MaybeUninit<FrameInfo>] =
        unsafe { core::slice::from_raw_parts_mut(storage_ptr, layout.frame_count()) };

    let exclusions = [LOW_MEMORY, metadata.clone()];
    let allocator = match FrameAllocator::new(layout, storage, regions, &exclusions) {
        Ok(allocator) => allocator,
        Err(err) => panic!("frame allocator initialisation failed: {err:?}"),
    };

    let stats = allocator.stats();
    klog::info!(
        "frame allocator: {} MiB free in {} frames, metadata {} KiB at {:#x}",
        stats.free_frames * PAGE_SIZE / MIB,
        stats.managed_frames,
        layout.metadata_bytes() / 1024,
        metadata.start
    );
    FRAMES.call_once(|| Mutex::new(allocator));
}

/// Adds every bootloader-reclaimable region to the allocator.
pub fn reclaim_bootloader_memory(boot: &BootInfo) {
    let mut reclaimed = 0;
    for region in boot
        .memory_regions()
        .iter()
        .filter(|r| r.kind() == RegionKind::BootloaderReclaimable)
    {
        match with_allocator(|frames| {
            frames.add_free_range(region.base()..region.end(), &[LOW_MEMORY])
        }) {
            Ok(frames) => reclaimed += frames,
            Err(err) => panic!("reclaiming bootloader memory {:#x}: {err:?}", region.base()),
        }
    }
    let stats = with_allocator(|frames| frames.stats());
    klog::info!(
        "reclaimed {} MiB of bootloader memory; {} MiB free",
        reclaimed * PAGE_SIZE / MIB,
        stats.free_frames * PAGE_SIZE / MIB
    );
}

/// Allocates `2^order` contiguous, naturally aligned frames.
///
/// The contents are undefined; callers that expose frames to userspace must
/// zero them first.
pub fn allocate_frames(order: u8) -> Result<Frame, AllocError> {
    with_allocator(|frames| frames.allocate(order))
}

/// Returns a block obtained from [`allocate_frames`].
pub fn free_frames(frame: Frame) -> Result<(), FreeError> {
    with_allocator(|frames| frames.free(frame))
}

/// Managed and free frame counts.
pub fn stats() -> oceans_frame_allocator::Stats {
    with_allocator(|frames| frames.stats())
}

fn with_allocator<R>(f: impl FnOnce(&mut FrameAllocator<'static>) -> R) -> R {
    let frames = FRAMES
        .get()
        .expect("memory::init initialises the frame allocator");
    arch::without_interrupts(|| f(&mut frames.lock()))
}

/// Allocates real frames, writes and reads them through the direct map, and
/// checks that freeing restores the allocator exactly.
pub fn self_test() {
    let before = with_allocator(|frames| frames.stats());

    let single = allocate_frames(0).expect("self-test: allocate one frame");
    let block = allocate_frames(4).expect("self-test: allocate a 64 KiB block");
    assert_eq!(
        block.addr() % (PAGE_SIZE << 4),
        0,
        "block not naturally aligned"
    );

    for (frame, pages) in [(single, 1), (block, 16)] {
        let bytes = pages * PAGE_SIZE as usize;
        let ptr = phys_to_virt(frame.addr());
        // SAFETY: the frames were just allocated, so nothing else uses them,
        // and the direct map covers all usable RAM read-write.
        let memory = unsafe { core::slice::from_raw_parts_mut(ptr, bytes) };
        for (i, byte) in memory.iter_mut().enumerate() {
            *byte = i as u8 ^ 0x5a;
        }
        assert!(
            memory.iter().enumerate().all(|(i, &b)| b == i as u8 ^ 0x5a),
            "self-test: frame {:#x} does not hold written data",
            frame.addr()
        );
    }

    free_frames(block).expect("self-test: free block");
    free_frames(single).expect("self-test: free frame");
    assert_eq!(
        free_frames(single),
        Err(FreeError::NotAllocated(single)),
        "self-test: double free must be rejected"
    );
    assert_eq!(with_allocator(|frames| frames.stats()), before);
    klog::info!("frame allocator self-test passed");
}
