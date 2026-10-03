//! Memory objects: page-granular memory that can be shared by capability
//! and mapped into address spaces.
//!
//! Three backings (ADR-0011, ADR-0021):
//!
//! - **pages**: zeroed RAM frames, the normal case;
//! - **contiguous**: one physically contiguous, zeroed block, for DMA;
//! - **device**: device registers (a PCI BAR). Not RAM and not owned: the
//!   kernel never reads or writes them itself, maps them uncached, never
//!   executable, and leaves out the pages listed as holes (the MSI-X
//!   table, which only the kernel programs).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;
use core::ptr;
use core::sync::atomic::{AtomicU8, Ordering};

use oceans_frame_allocator::{Frame, MAX_ORDER};
use oceans_memory_map::PAGE_SIZE;

use super::ObjectError;
use crate::memory::paging::Cache;
use crate::memory::{frames, phys_to_virt};

/// Largest memory object, bounding a single request's frame consumption.
const MAX_SIZE: u64 = 1 << 30;

/// `mapping_mode` bits: has the object ever been mapped writable / executable.
const MAPPED_WRITABLE: u8 = 1 << 0;
const MAPPED_EXECUTABLE: u8 = 1 << 1;

/// Device ranges left unmapped (MSI-X table and pending bits).
pub const MAX_HOLES: usize = 2;

#[derive(Debug)]
enum Backing {
    Pages(Vec<Frame>),
    Contiguous(Frame),
    Device {
        base: u64,
        /// Page-aligned byte ranges, relative to `base`, never mapped.
        holes: [Range<u64>; MAX_HOLES],
    },
}

#[derive(Debug)]
pub struct MemoryObject {
    backing: Backing,
    size: u64,
    /// W^X across mappings: once mapped writable an object can never be
    /// mapped executable, and vice versa, so two mappings of one object
    /// cannot be combined into writable code.
    mapping_mode: AtomicU8,
}

impl MemoryObject {
    /// A zero-filled object of `size` bytes, rounded up to whole pages.
    /// Zeroing ensures no previous contents ever leak to a new owner.
    pub fn new(size: u64) -> Result<Arc<Self>, ObjectError> {
        if size == 0 || size > MAX_SIZE {
            return Err(ObjectError::OutOfBounds);
        }
        let pages = size.div_ceil(PAGE_SIZE) as usize;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(pages)
            .map_err(|_| ObjectError::OutOfMemory)?;
        let mut object = Self {
            backing: Backing::Pages(frames),
            size: pages as u64 * PAGE_SIZE,
            mapping_mode: AtomicU8::new(0),
        };
        let Backing::Pages(frames) = &mut object.backing else {
            unreachable!()
        };
        for _ in 0..pages {
            // On failure `object` is dropped, returning frames taken so far.
            let frame = frames::allocate_frames(0).map_err(|_| ObjectError::OutOfMemory)?;
            // SAFETY: freshly allocated, exclusively owned, direct-mapped.
            unsafe { ptr::write_bytes(phys_to_virt(frame.addr()), 0, PAGE_SIZE as usize) };
            frames.push(frame);
        }
        Ok(Arc::new(object))
    }

    /// A zero-filled, physically contiguous object of at least `size`
    /// bytes (rounded up to a power-of-two number of pages, at most
    /// 4 MiB), for device DMA. Never executable.
    pub fn new_contiguous(size: u64) -> Result<Arc<Self>, ObjectError> {
        let pages = size.div_ceil(PAGE_SIZE).max(1).next_power_of_two();
        let order = pages.trailing_zeros();
        if size == 0 || order > u32::from(MAX_ORDER) {
            return Err(ObjectError::OutOfBounds);
        }
        let frame = frames::allocate_frames(order as u8).map_err(|_| ObjectError::OutOfMemory)?;
        let bytes = pages * PAGE_SIZE;
        // SAFETY: freshly allocated, exclusively owned, direct-mapped RAM.
        unsafe { ptr::write_bytes(phys_to_virt(frame.addr()), 0, bytes as usize) };
        Ok(Arc::new(Self {
            backing: Backing::Contiguous(frame),
            size: bytes,
            // Device-written memory is data: never executable.
            mapping_mode: AtomicU8::new(MAPPED_WRITABLE),
        }))
    }

    /// Device registers at physical `base..base + size` (page-aligned),
    /// with `holes` (page-aligned, relative to `base`) never mapped. The
    /// caller vouches that the range is device memory, not RAM.
    pub fn new_device(base: u64, size: u64, holes: [Range<u64>; MAX_HOLES]) -> Arc<Self> {
        debug_assert!(base.is_multiple_of(PAGE_SIZE) && size.is_multiple_of(PAGE_SIZE));
        Arc::new(Self {
            backing: Backing::Device { base, holes },
            size,
            mapping_mode: AtomicU8::new(MAPPED_WRITABLE),
        })
    }

    /// Size in bytes (a multiple of the page size).
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Records that the object is about to be mapped with these
    /// permissions; fails if that would make it both writable and
    /// executable over its lifetime.
    pub fn claim_mapping(&self, writable: bool, executable: bool) -> Result<(), ObjectError> {
        let wanted = if writable { MAPPED_WRITABLE } else { 0 }
            | if executable { MAPPED_EXECUTABLE } else { 0 };
        self.mapping_mode
            .try_update(Ordering::AcqRel, Ordering::Acquire, |mode| {
                let mode = mode | wanted;
                (mode != MAPPED_WRITABLE | MAPPED_EXECUTABLE).then_some(mode)
            })
            .map(drop)
            .map_err(|_| ObjectError::WriteExecute)
    }

    /// Physical address of page `index`, or `None` for a hole (or past the
    /// end).
    pub fn page(&self, index: u64) -> Option<u64> {
        if index >= self.size / PAGE_SIZE {
            return None;
        }
        let offset = index * PAGE_SIZE;
        match &self.backing {
            Backing::Pages(frames) => Some(frames[index as usize].addr()),
            Backing::Contiguous(frame) => Some(frame.addr() + offset),
            Backing::Device { base, holes } => {
                (!holes.iter().any(|hole| hole.contains(&offset))).then_some(base + offset)
            }
        }
    }

    /// Memory type to map the object with.
    pub fn cache(&self) -> Cache {
        match self.backing {
            Backing::Device { .. } => Cache::Uncached,
            _ => Cache::WriteBack,
        }
    }

    /// Physical base of a contiguous object (what a device uses for DMA).
    pub fn contiguous_base(&self) -> Option<u64> {
        match self.backing {
            Backing::Contiguous(frame) => Some(frame.addr()),
            _ => None,
        }
    }

    /// Copies `buffer.len()` bytes starting at `offset` out of the object.
    pub fn read(&self, offset: u64, buffer: &mut [u8]) -> Result<(), ObjectError> {
        self.for_each_chunk(offset, buffer.len(), |object_ptr, at, len| {
            // SAFETY: `object_ptr..+len` lies within one owned RAM frame (see
            // `for_each_chunk`); `buffer[at..at + len]` is in bounds.
            unsafe { ptr::copy_nonoverlapping(object_ptr, buffer.as_mut_ptr().add(at), len) };
        })
    }

    /// Copies `data` into the object starting at `offset`.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<(), ObjectError> {
        self.for_each_chunk(offset, data.len(), |object_ptr, at, len| {
            // SAFETY: as in `read`. Concurrent writers race on contents only,
            // as with any shared memory; there is no memory unsafety because
            // the bytes are plain data never interpreted by the kernel.
            unsafe { ptr::copy_nonoverlapping(data.as_ptr().add(at), object_ptr, len) };
        })
    }

    /// Splits `[offset, offset + len)` at page boundaries and calls `f` with
    /// a direct-map pointer into each frame, the position in the caller's
    /// buffer and the chunk length. Device memory is refused: it is not in
    /// the direct map, and reading registers can have side effects.
    fn for_each_chunk(
        &self,
        offset: u64,
        len: usize,
        mut f: impl FnMut(*mut u8, usize, usize),
    ) -> Result<(), ObjectError> {
        if let Backing::Device { .. } = self.backing {
            return Err(ObjectError::NotRam);
        }
        let end = offset
            .checked_add(len as u64)
            .filter(|&end| end <= self.size)
            .ok_or(ObjectError::OutOfBounds)?;
        let mut position = offset;
        while position < end {
            let frame = self.page(position / PAGE_SIZE).expect("RAM has no holes");
            let in_page = position % PAGE_SIZE;
            let chunk = (PAGE_SIZE - in_page).min(end - position);
            let object_ptr = phys_to_virt(frame + in_page);
            f(object_ptr, (position - offset) as usize, chunk as usize);
            position += chunk;
        }
        Ok(())
    }
}

impl Drop for MemoryObject {
    fn drop(&mut self) {
        let owned: &[Frame] = match &self.backing {
            Backing::Pages(frames) => frames,
            Backing::Contiguous(frame) => core::slice::from_ref(frame),
            Backing::Device { .. } => &[],
        };
        for &frame in owned {
            if let Err(err) = frames::free_frames(frame) {
                panic!(
                    "memory object frame {:#x} rejected on free: {err:?}",
                    frame.addr()
                );
            }
        }
    }
}
