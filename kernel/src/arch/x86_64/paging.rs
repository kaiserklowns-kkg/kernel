//! x86_64 4-level page tables.

use ::x86_64::PhysAddr;
use ::x86_64::registers::control::{Cr3, Cr3Flags};
use ::x86_64::structures::paging::PhysFrame;

use crate::memory::paging::{Cache, MapError, MapFlags, PageSize};
use crate::memory::{frames, phys_to_virt};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const WRITE_THROUGH: u64 = 1 << 3;
const CACHE_DISABLE: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const NO_EXECUTE: u64 = 1 << 63;
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

const ENTRIES: usize = 512;

/// Level of the table an entry lives in: 3 = PML4, 0 = PT.
const fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1ff) as usize
}

const fn leaf_level(size: PageSize) -> u32 {
    match size {
        PageSize::Size4KiB => 0,
        PageSize::Size2MiB => 1,
        PageSize::Size1GiB => 2,
    }
}

const fn is_canonical(virt: u64) -> bool {
    let top = virt >> 47;
    top == 0 || top == 0x1ffff
}

/// A set of page tables rooted at one PML4.
pub struct AddressSpace {
    root: u64,
}

impl AddressSpace {
    pub fn new() -> Result<Self, MapError> {
        Ok(Self {
            root: allocate_table()?,
        })
    }

    /// A user address space: empty lower half, kernel half shared with
    /// `kernel` by copying its top-level entries (which never change after
    /// boot, ADR-0009).
    pub fn new_user(kernel: &AddressSpace) -> Result<Self, MapError> {
        let root = allocate_table()?;
        for slot in ENTRIES / 2..ENTRIES {
            write(root, slot, read(kernel.root, slot));
        }
        Ok(Self { root })
    }

    /// Frees this address space's own page tables: every table below the
    /// lower-half top-level entries, and the root. Leaf frames are not freed
    /// (they belong to memory objects); the shared kernel half is untouched.
    ///
    /// # Safety
    ///
    /// The address space must not be active on any CPU, and nothing may use
    /// it afterwards.
    pub unsafe fn destroy_user(self) {
        for slot in 0..ENTRIES / 2 {
            let entry = read(self.root, slot);
            if entry & PRESENT != 0 {
                free_subtree(entry & ADDRESS_MASK, 2);
            }
        }
        free_table(self.root);
    }

    /// Physical address of the PML4.
    pub const fn root(&self) -> u64 {
        self.root
    }

    pub fn map(
        &mut self,
        virt: u64,
        phys: u64,
        size: PageSize,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        let bytes = size.bytes();
        if !virt.is_multiple_of(bytes)
            || !phys.is_multiple_of(bytes)
            || !is_canonical(virt)
            || phys & !ADDRESS_MASK != 0
        {
            return Err(MapError::Misaligned { virt, phys });
        }
        let leaf = leaf_level(size);
        let mut table = self.root;
        for level in (leaf + 1..=3).rev() {
            table = next_table(table, index(virt, level), flags.user, virt)?;
        }
        let slot = index(virt, leaf);
        if read(table, slot) & PRESENT != 0 {
            return Err(MapError::AlreadyMapped(virt));
        }
        write(
            table,
            slot,
            phys | leaf_bits(flags) | if leaf > 0 { HUGE } else { 0 },
        );
        Ok(())
    }

    /// Removes the 4 KiB mapping at `virt` and flushes it from this CPU's
    /// TLB. Returns the physical address it mapped. Page tables emptied by
    /// this are kept (they are reused by later mappings).
    ///
    /// Only valid for the active address space or the shared kernel half;
    /// other CPUs need a shootdown once SMP exists.
    pub fn unmap(&mut self, virt: u64) -> Result<u64, MapError> {
        let mut table = self.root;
        for level in (1..=3).rev() {
            let entry = read(table, index(virt, level));
            if entry & PRESENT == 0 {
                return Err(MapError::NotMapped(virt));
            }
            if entry & HUGE != 0 {
                return Err(MapError::HugePageConflict(virt));
            }
            table = entry & ADDRESS_MASK;
        }
        let slot = index(virt, 0);
        let entry = read(table, slot);
        if entry & PRESENT == 0 {
            return Err(MapError::NotMapped(virt));
        }
        write(table, slot, 0);
        ::x86_64::instructions::tlb::flush(::x86_64::VirtAddr::new(virt));
        Ok(entry & ADDRESS_MASK)
    }

    /// Allocates the next-level table for the top-level slot covering `virt`
    /// if it does not exist yet.
    pub fn prepare_top_level(&mut self, virt: u64) -> Result<(), MapError> {
        next_table(self.root, index(virt, 3), false, virt).map(drop)
    }

    /// Physical address and flags `virt` is mapped to.
    pub fn translate(&self, virt: u64) -> Option<(u64, MapFlags)> {
        let mut table = self.root;
        for level in (0..=3).rev() {
            let entry = read(table, index(virt, level));
            if entry & PRESENT == 0 {
                return None;
            }
            if level == 0 || entry & HUGE != 0 {
                let page_mask = (1u64 << (12 + 9 * level)) - 1;
                let phys = (entry & ADDRESS_MASK & !page_mask) | (virt & page_mask);
                return Some((phys, flags_of(entry)));
            }
            table = entry & ADDRESS_MASK;
        }
        None
    }

    /// Loads these page tables.
    ///
    /// # Safety
    ///
    /// The tables must map all code, data and stack the CPU will touch from
    /// here on, with compatible permissions.
    pub unsafe fn activate(&self) {
        let frame = PhysFrame::containing_address(PhysAddr::new(self.root));
        // SAFETY: the caller guarantees the mappings; `root` is a page-table
        // frame owned by this address space.
        unsafe { Cr3::write(frame, Cr3Flags::empty()) };
    }
}

fn leaf_bits(flags: MapFlags) -> u64 {
    let mut bits = PRESENT;
    if flags.writable {
        bits |= WRITABLE;
    }
    if !flags.executable {
        bits |= NO_EXECUTE;
    }
    if flags.user {
        bits |= USER;
    }
    if flags.global {
        bits |= GLOBAL;
    }
    if let Cache::Uncached = flags.cache {
        // PAT entry 3 (PCD | PWT) is UC in the power-on PAT.
        bits |= CACHE_DISABLE | WRITE_THROUGH;
    }
    bits
}

fn flags_of(entry: u64) -> MapFlags {
    MapFlags {
        writable: entry & WRITABLE != 0,
        executable: entry & NO_EXECUTE == 0,
        user: entry & USER != 0,
        global: entry & GLOBAL != 0,
        cache: if entry & CACHE_DISABLE != 0 {
            Cache::Uncached
        } else {
            Cache::WriteBack
        },
    }
}

/// Pointer to entry `index` of the table at physical address `table`.
fn entry(table: u64, index: usize) -> *mut u64 {
    assert!(index < ENTRIES);
    phys_to_virt(table).cast::<u64>().wrapping_add(index)
}

fn read(table: u64, index: usize) -> u64 {
    // SAFETY: `table` is a page-table frame owned by an address space and
    // mapped by the direct map; tables are only modified while the owning
    // address space is borrowed mutably (behind its lock). Volatile because
    // the CPU's page walker reads (and sets A/D bits in) these entries.
    unsafe { entry(table, index).read_volatile() }
}

fn write(table: u64, index: usize, value: u64) {
    // SAFETY: as for `read`.
    unsafe { entry(table, index).write_volatile(value) }
}

/// Follows (or creates) the entry at `index` of `table` to the next level.
/// Intermediate entries are permissive; leaves carry the real permissions.
fn next_table(table: u64, index: usize, user: bool, virt: u64) -> Result<u64, MapError> {
    let current = read(table, index);
    if current & PRESENT != 0 {
        if current & HUGE != 0 {
            return Err(MapError::HugePageConflict(virt));
        }
        if user && current & USER == 0 {
            write(table, index, current | USER);
        }
        return Ok(current & ADDRESS_MASK);
    }
    let next = allocate_table()?;
    write(
        table,
        index,
        next | PRESENT | WRITABLE | if user { USER } else { 0 },
    );
    Ok(next)
}

/// Frees the table at `table` (level 2 = PDPT … 0 = PT) and all tables below.
fn free_subtree(table: u64, level: u32) {
    if level > 0 {
        for slot in 0..ENTRIES {
            let entry = read(table, slot);
            if entry & PRESENT != 0 && entry & HUGE == 0 {
                free_subtree(entry & ADDRESS_MASK, level - 1);
            }
        }
    }
    free_table(table);
}

fn free_table(table: u64) {
    let frame = oceans_frame_allocator::Frame::from_addr(table).expect("tables are page-aligned");
    if let Err(err) = frames::free_frames(frame) {
        panic!("page table {table:#x} rejected on free: {err:?}");
    }
}

/// Physical address of the active top-level table.
pub fn active_root() -> u64 {
    Cr3::read().0.start_address().as_u64()
}

/// Loads the address space rooted at `root`.
///
/// # Safety
///
/// `root` must be a complete address space (kernel half included) that
/// stays alive while active.
pub unsafe fn activate_root(root: u64) {
    let frame = PhysFrame::containing_address(PhysAddr::new(root));
    // SAFETY: caller contract.
    unsafe { Cr3::write(frame, Cr3Flags::empty()) };
}

/// A zeroed frame for a page table.
fn allocate_table() -> Result<u64, MapError> {
    let frame = frames::allocate_frames(0).map_err(|_| MapError::OutOfMemory)?;
    // SAFETY: freshly allocated frame, exclusively ours, mapped by the direct map.
    unsafe { core::ptr::write_bytes(phys_to_virt(frame.addr()), 0, 4096) };
    Ok(frame.addr())
}
