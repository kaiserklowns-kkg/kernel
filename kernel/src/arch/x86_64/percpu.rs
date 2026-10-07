//! Each CPU's own block of kernel data, reached through `GS` (ADR-0089).
//!
//! While a CPU runs kernel code its `GS` base points at its block; while
//! it runs user code the block's address waits in `IA32_KERNEL_GS_BASE`.
//! Every entry from ring 3 (`syscall`, an interrupt or exception whose
//! saved CS is a user one) starts with `swapgs`, and every return to ring 3
//! (`sysret`, `iretq` to a user CS, the first entry) ends with one, with
//! interrupts disabled in between. So kernel code can always read its
//! CPU's block, and user code never sees its address.
//!
//! User code cannot change `GS`'s base on its own: `WRGSBASE` is off
//! (CR4.FSGSBASE clear), and loading a selector only sets it to the
//! descriptor's base, which `swapgs` puts aside on the next entry.

use core::cell::UnsafeCell;

use ::x86_64::registers::model_specific::Msr;

use super::MAX_CPUS;

const IA32_GS_BASE: u32 = 0xc000_0101;
const IA32_KERNEL_GS_BASE: u32 = 0xc000_0102;

/// Byte offsets in [`Block`], for the entry stubs.
pub const KERNEL_RSP: usize = 8;
pub const USER_RSP: usize = 16;
const INDEX: usize = 24;

/// One CPU's block. The layout is fixed: the entry stubs address it.
#[repr(C, align(64))]
struct Block {
    /// Its own address (unused by the stubs; for debugging).
    this: u64,
    /// The running thread's kernel stack top (`syscall` switches to it).
    kernel_rsp: u64,
    /// The user stack pointer, kept while `syscall` saves registers.
    user_rsp: u64,
    index: u64,
}

struct BlockCell(UnsafeCell<Block>);

// SAFETY: each block is written only by its own CPU (with interrupts
// disabled), or before that CPU uses it.
unsafe impl Sync for BlockCell {}

static BLOCKS: [BlockCell; MAX_CPUS] = [const {
    BlockCell(UnsafeCell::new(Block {
        this: 0,
        kernel_rsp: 0,
        user_rsp: 0,
        index: 0,
    }))
}; MAX_CPUS];

/// Points this CPU's `GS` at block `index`. Runs once per CPU, in kernel
/// mode, before anything asks for the CPU's index.
pub fn init(index: usize) {
    let block = BLOCKS[index].0.get();
    // SAFETY: the block is this CPU's alone and not in use yet.
    unsafe {
        (*block).this = block as u64;
        (*block).index = index as u64;
    }
    // SAFETY: GS's base registers exist on every x86_64 CPU; the kernel
    // runs with GS at its block, and user code starts with base 0 (the
    // first `swapgs` to ring 3 puts this one aside).
    unsafe {
        Msr::new(IA32_GS_BASE).write(block as u64);
        Msr::new(IA32_KERNEL_GS_BASE).write(0);
    }
}

/// The calling CPU's index.
pub fn index() -> usize {
    let index: u64;
    // SAFETY: kernel code always runs with GS at its CPU's block (module
    // docs); the load reads its `index` field.
    unsafe {
        core::arch::asm!(
            "mov {}, gs:[{offset}]",
            out(reg) index,
            offset = const INDEX,
            options(nostack, readonly, preserves_flags),
        );
    }
    index as usize
}

/// Sets the kernel stack `syscall` switches to on this CPU. Interrupts must
/// be disabled.
pub fn set_kernel_rsp(top: u64) {
    // SAFETY: as for `index`; only this CPU writes its block.
    unsafe {
        core::arch::asm!(
            "mov gs:[{offset}], {}",
            in(reg) top,
            offset = const KERNEL_RSP,
            options(nostack, preserves_flags),
        );
    }
}
