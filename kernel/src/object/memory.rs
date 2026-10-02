//! Memory objects: page-granular RAM that can be shared by capability and,
//! later, mapped into address spaces.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr;

use oceans_frame_allocator::Frame;
use oceans_memory_map::PAGE_SIZE;

use super::ObjectError;
use crate::memory::{frames, phys_to_virt};

/// Largest memory object, bounding a single request's frame consumption.
const MAX_SIZE: u64 = 1 << 30;

#[derive(Debug)]
pub struct MemoryObject {
    frames: Vec<Frame>,
    size: u64,
}

impl MemoryObject {
    /// A zero-filled object of `size` bytes, rounded up to whole pages.
    /// Zeroing ensures no previous contents ever leak to a new owner.
    pub fn new(size: u64) -> Result<Arc<Self>, ObjectError> {
        if size == 0 || size > MAX_SIZE {
            return Err(ObjectError::OutOfBounds);
        }
        let pages = size.div_ceil(PAGE_SIZE) as usize;
        let mut object = Self {
            frames: Vec::new(),
            size: pages as u64 * PAGE_SIZE,
        };
        object
            .frames
            .try_reserve_exact(pages)
            .map_err(|_| ObjectError::OutOfMemory)?;
        for _ in 0..pages {
            // On failure `object` is dropped, returning frames taken so far.
            let frame = frames::allocate_frames(0).map_err(|_| ObjectError::OutOfMemory)?;
            // SAFETY: freshly allocated, exclusively owned, direct-mapped.
            unsafe { ptr::write_bytes(phys_to_virt(frame.addr()), 0, PAGE_SIZE as usize) };
            object.frames.push(frame);
        }
        Ok(Arc::new(object))
    }

    /// Size in bytes (a multiple of the page size).
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Copies `buffer.len()` bytes starting at `offset` out of the object.
    pub fn read(&self, offset: u64, buffer: &mut [u8]) -> Result<(), ObjectError> {
        self.for_each_chunk(offset, buffer.len(), |object_ptr, at, len| {
            // SAFETY: `object_ptr..+len` lies within one owned frame (see
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
    /// buffer and the chunk length.
    fn for_each_chunk(
        &self,
        offset: u64,
        len: usize,
        mut f: impl FnMut(*mut u8, usize, usize),
    ) -> Result<(), ObjectError> {
        let end = offset
            .checked_add(len as u64)
            .filter(|&end| end <= self.size)
            .ok_or(ObjectError::OutOfBounds)?;
        let mut position = offset;
        while position < end {
            let frame = self.frames[(position / PAGE_SIZE) as usize];
            let in_page = position % PAGE_SIZE;
            let chunk = (PAGE_SIZE - in_page).min(end - position);
            let object_ptr = phys_to_virt(frame.addr() + in_page);
            f(object_ptr, (position - offset) as usize, chunk as usize);
            position += chunk;
        }
        Ok(())
    }
}

impl Drop for MemoryObject {
    fn drop(&mut self) {
        for &frame in &self.frames {
            if let Err(err) = frames::free_frames(frame) {
                panic!(
                    "memory object frame {:#x} rejected on free: {err:?}",
                    frame.addr()
                );
            }
        }
    }
}
