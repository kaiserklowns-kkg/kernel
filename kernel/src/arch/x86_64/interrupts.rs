//! CPU exception handling.
//!
//! Every exception vector has a small assembly stub that normalises the stack
//! (pushing a dummy error code where the CPU does not), then jumps to a common
//! routine that saves all general-purpose registers into a [`TrapFrame`] and
//! calls [`exception_dispatch`]. This avoids the unstable `x86-interrupt` ABI
//! (ADR-0004).

use core::arch::{asm, global_asm, naked_asm};

use ::x86_64::VirtAddr;
use ::x86_64::instructions::segmentation::{CS, Segment};
use ::x86_64::instructions::tables::lidt;
use ::x86_64::structures::DescriptorTablePointer;
use spin::Once;

use super::gdt::DOUBLE_FAULT_IST_INDEX;
use crate::klog;

const EXCEPTION_COUNT: usize = 32;
const VECTOR_BREAKPOINT: u64 = 3;
const VECTOR_DOUBLE_FAULT: usize = 8;
const VECTOR_PAGE_FAULT: u64 = 14;

/// Register state at the time of the exception, in stack order.
#[repr(C)]
#[derive(Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    // Pushed by the CPU.
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// Per-vector entry stubs plus a table of their addresses. Vectors 8, 10–14,
// 17, 21, 29 and 30 have a CPU-pushed error code; the rest get a dummy 0 so
// every TrapFrame has the same layout.
macro_rules! exception_stubs {
    ($($vector:literal $push_error:literal),* $(,)?) => {
        global_asm!(
            ".pushsection .text.oceans_exceptions, \"ax\", @progbits",
            $(
                concat!("oceans_exception_stub_", stringify!($vector), ":"),
                $push_error,
                concat!("push ", stringify!($vector)),
                "jmp {common}",
            )*
            ".popsection",
            ".pushsection .rodata.oceans_exceptions, \"a\", @progbits",
            ".balign 8",
            ".global oceans_exception_stubs",
            ".hidden oceans_exception_stubs",
            "oceans_exception_stubs:",
            $( concat!(".quad oceans_exception_stub_", stringify!($vector)), )*
            ".popsection",
            common = sym exception_common,
        );
    };
}

exception_stubs! {
    0 "push 0", 1 "push 0", 2 "push 0", 3 "push 0", 4 "push 0", 5 "push 0",
    6 "push 0", 7 "push 0", 8 "", 9 "push 0", 10 "", 11 "", 12 "", 13 "", 14 "",
    15 "push 0", 16 "push 0", 17 "", 18 "push 0", 19 "push 0", 20 "push 0",
    21 "", 22 "push 0", 23 "push 0", 24 "push 0", 25 "push 0", 26 "push 0",
    27 "push 0", 28 "push 0", 29 "", 30 "", 31 "push 0",
}

unsafe extern "C" {
    #[link_name = "oceans_exception_stubs"]
    static EXCEPTION_STUBS: [u64; EXCEPTION_COUNT];
}

/// Saves registers, calls the dispatcher with a pointer to the frame,
/// restores registers and returns from the interrupt.
///
/// Stack alignment: the CPU aligns RSP to 16 bytes before pushing its 5-word
/// frame; with error code, vector and 15 registers the total is 22 words, so
/// RSP is 16-byte aligned at the `call` as the SysV ABI requires.
#[unsafe(naked)]
unsafe extern "C" fn exception_common() {
    naked_asm!(
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov rdi, rsp",
        "cld",
        "call {dispatch}",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        "add rsp, 16", // vector + error code
        "iretq",
        dispatch = sym exception_dispatch,
    );
}

extern "C" fn exception_dispatch(frame: &mut TrapFrame) {
    match frame.vector {
        VECTOR_BREAKPOINT => klog::info!("breakpoint at {:#x}, resuming", frame.rip),
        _ => fatal_exception(frame),
    }
}

fn fatal_exception(frame: &TrapFrame) -> ! {
    let name = EXCEPTION_NAMES
        .get(frame.vector as usize)
        .copied()
        .unwrap_or("unknown");
    if frame.vector == VECTOR_PAGE_FAULT {
        let address: u64;
        // SAFETY: reading CR2 has no side effects.
        unsafe { asm!("mov {}, cr2", out(reg) address, options(nomem, nostack, preserves_flags)) };
        panic!(
            "unhandled {name} (vector {}) accessing {address:#x}, error {:#x}\n{frame:#x?}",
            frame.vector, frame.error_code
        );
    }
    panic!(
        "unhandled {name} (vector {}), error {:#x}\n{frame:#x?}",
        frame.vector, frame.error_code
    );
}

const EXCEPTION_NAMES: [&str; EXCEPTION_COUNT] = [
    "divide error",
    "debug",
    "non-maskable interrupt",
    "breakpoint",
    "overflow",
    "bound range exceeded",
    "invalid opcode",
    "device not available",
    "double fault",
    "coprocessor segment overrun",
    "invalid TSS",
    "segment not present",
    "stack-segment fault",
    "general protection fault",
    "page fault",
    "reserved",
    "x87 floating-point exception",
    "alignment check",
    "machine check",
    "SIMD floating-point exception",
    "virtualization exception",
    "control protection exception",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "hypervisor injection exception",
    "VMM communication exception",
    "security exception",
    "reserved",
];

/// A 64-bit interrupt gate descriptor.
#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl Gate {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    /// Present, DPL 0, 64-bit interrupt gate (IF cleared on entry).
    const INTERRUPT_GATE: u8 = 0x8e;

    fn interrupt(handler: u64, selector: u16, ist: Option<u16>) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            // The IST field is 1-based; 0 means "stay on the current stack".
            ist: ist.map_or(0, |index| index as u8 + 1),
            attributes: Self::INTERRUPT_GATE,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct Idt([Gate; 256]);

static IDT: Once<Idt> = Once::new();

pub fn init() {
    let idt = IDT.call_once(|| {
        let selector = CS::get_reg().0;
        let mut gates = [Gate::MISSING; 256];
        for (vector, gate) in gates.iter_mut().enumerate().take(EXCEPTION_COUNT) {
            // SAFETY: the table is defined in `exception_stubs!` above with
            // exactly EXCEPTION_COUNT entries and is never written.
            let handler = unsafe { EXCEPTION_STUBS[vector] };
            let ist = (vector == VECTOR_DOUBLE_FAULT).then_some(DOUBLE_FAULT_IST_INDEX);
            *gate = Gate::interrupt(handler, selector, ist);
        }
        // Vectors ≥ 32 stay non-present until interrupt routing exists; a
        // stray delivery raises #GP/#NP and is reported as fatal.
        Idt(gates)
    });

    let pointer = DescriptorTablePointer {
        limit: (size_of::<Idt>() - 1) as u16,
        base: VirtAddr::from_ptr(idt),
    };
    // SAFETY: `idt` is 'static and immutable after initialisation, and every
    // present gate points at a stub that preserves state and returns via iretq.
    unsafe { lidt(&pointer) };
    klog::debug!("IDT loaded with {EXCEPTION_COUNT} exception handlers");
}
