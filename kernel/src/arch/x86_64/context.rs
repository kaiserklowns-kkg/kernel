//! Kernel thread context switching.
//!
//! A suspended thread is fully described by its saved stack pointer: the
//! callee-saved registers and the return address sit on its own stack. All
//! other registers are caller-saved, so the compiler has already spilled
//! them at the call to [`switch_context`]. The kernel is built soft-float
//! (no SSE/x87 state); user threads will add XSAVE state.

use core::arch::naked_asm;

/// Saves the current context, storing its stack pointer in `*save`, and
/// resumes the context whose stack pointer is `load`.
///
/// # Safety
///
/// Interrupts must be disabled. `save` must be valid for writes and remain
/// so until this context is resumed. `load` must be a stack pointer saved
/// by this function or produced by [`prepare_stack`], whose stack is mapped
/// and owned by the resumed thread.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(save: *mut u64, load: u64) {
    naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    );
}

/// First code of every new thread: calls `start(entry, arg)` (in r14, r12,
/// r13 from [`prepare_stack`]) on an aligned stack.
#[unsafe(naked)]
unsafe extern "C" fn thread_trampoline() {
    naked_asm!(
        "xor ebp, ebp",
        "mov rdi, r12",
        "mov rsi, r13",
        "and rsp, -16",
        "call r14",
        "ud2",
    );
}

/// Writes an initial frame onto the stack ending at `top` so that switching
/// to the returned stack pointer runs `start(entry, arg)`.
///
/// # Safety
///
/// `top` must be the 16-byte aligned end of a mapped, writable stack of at
/// least 64 bytes that nothing else uses.
pub unsafe fn prepare_stack(
    top: u64,
    start: extern "C" fn(usize, usize) -> !,
    entry: usize,
    arg: usize,
) -> u64 {
    // Popped by `switch_context` in this order: r15, r14, r13, r12, rbx,
    // rbp, then `ret` to the trampoline. The top slot is padding.
    let frame: [u64; 8] = [
        0,                                     // r15
        start as *const () as u64,             // r14
        arg as u64,                            // r13
        entry as u64,                          // r12
        0,                                     // rbx
        0,                                     // rbp
        thread_trampoline as *const () as u64, // return address
        0,                                     // padding
    ];
    let rsp = top - size_of_val(&frame) as u64;
    // SAFETY: caller guarantees the 64 bytes below `top` are ours.
    unsafe { (rsp as *mut [u64; 8]).write(frame) };
    rsp
}
