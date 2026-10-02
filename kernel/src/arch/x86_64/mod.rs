//! x86_64 support (Tier 1, ADR-0005).

mod cpu;
mod gdt;
mod interrupts;
mod paging;
mod pic;
mod serial;

use core::arch::asm;

use ::x86_64::instructions::{self as insn, port::Port};

pub use cpu::{enable_protections, features as cpu_features};
pub use paging::AddressSpace;

pub const NAME: &str = "x86_64";

/// Brings up what logging needs. Runs before anything else, so it must not
/// log, allocate or fault.
pub fn early_init() {
    serial::init();
}

/// CPU initialisation: descriptor tables, exception handling, legacy PIC.
pub fn init() {
    gdt::init();
    interrupts::init();
    pic::remap_and_mask();
}

pub fn console_write(s: &str) {
    serial::write_str(s);
}

pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    insn::interrupts::without_interrupts(f)
}

pub fn disable_interrupts() {
    insn::interrupts::disable();
}

/// Raises a breakpoint exception, which the kernel logs and returns from.
pub fn breakpoint() {
    insn::interrupts::int3();
}

pub fn halt_forever() -> ! {
    loop {
        insn::interrupts::disable();
        insn::hlt();
    }
}

/// Result reported through QEMU's `isa-debug-exit` device (iobase 0xf4).
/// QEMU exits with status `(code << 1) | 1`, i.e. 33 for success, 35 for failure.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum EmulatorExit {
    Success = 0x10,
    Failure = 0x11,
}

/// Terminates QEMU. Only called in smoke-test mode; on hardware without the
/// device the write is ignored and the CPU halts.
pub fn exit_emulator(code: EmulatorExit) -> ! {
    // SAFETY: port 0xf4 is reserved for the debug-exit device in our QEMU
    // configuration and unassigned on supported hardware.
    unsafe { Port::<u32>::new(0xf4).write(code as u32) };
    halt_forever()
}

/// Moves execution onto the stack whose top is `top` and calls `next`.
/// The current stack is abandoned and never returned to.
///
/// # Safety
///
/// `top` must be the 16-byte aligned top of a mapped, writable stack that
/// nothing else uses.
pub unsafe fn switch_stack(top: u64, next: extern "C" fn() -> !) -> ! {
    // SAFETY: the caller guarantees the stack; `call` pushes a return address
    // onto a 16-byte aligned RSP, which is the SysV entry convention, and
    // `next` never returns.
    unsafe {
        asm!(
            "mov rsp, {top}",
            "xor ebp, ebp",
            "call {next}",
            "ud2",
            top = in(reg) top,
            next = in(reg) next,
            options(noreturn),
        )
    }
}
