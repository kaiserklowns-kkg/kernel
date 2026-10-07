//! CPU exceptions and device interrupts.
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
/// Local APIC timer (above the remapped, masked legacy PIC range 32–47).
pub const VECTOR_TIMER: u8 = 48;
/// Serial console (COM1) receive interrupt, routed by the I/O APIC.
pub const VECTOR_SERIAL: u8 = 49;
/// PS/2 keyboard (ISA IRQ 1), routed by the I/O APIC (ADR-0029).
pub const VECTOR_KEYBOARD: u8 = 50;
/// Inter-processor interrupts (ADR-0088).
pub const VECTOR_IPI: u8 = 51;
/// Local APIC spurious interrupt; must not be acknowledged.
pub const VECTOR_SPURIOUS: u8 = 255;
/// Vectors for device interrupts (MSI-X), allocated to drivers (ADR-0021).
pub const DEVICE_VECTORS: core::ops::Range<u8> = 64..128;
const DEVICE_VECTOR_COUNT: usize = 64;

/// Called on every device interrupt with its vector, before EOI, with
/// interrupts disabled.
static DEVICE_HANDLER: Once<fn(u8)> = Once::new();

pub fn set_device_handler(handler: fn(u8)) {
    DEVICE_HANDLER.call_once(|| handler);
}

/// Called on every serial interrupt, before EOI, with interrupts disabled.
static SERIAL_HANDLER: Once<fn()> = Once::new();

pub fn set_serial_handler(handler: fn()) {
    SERIAL_HANDLER.call_once(|| handler);
}

/// Called on every keyboard interrupt, before EOI, with interrupts disabled.
static KEYBOARD_HANDLER: Once<fn()> = Once::new();

pub fn set_keyboard_handler(handler: fn()) {
    KEYBOARD_HANDLER.call_once(|| handler);
}

/// Called after a device interrupt has been acknowledged, with interrupts
/// disabled: lets the scheduler run a thread the interrupt woke.
static AFTER_DEVICE_INTERRUPT: Once<fn()> = Once::new();

pub fn set_after_device_interrupt(hook: fn()) {
    AFTER_DEVICE_INTERRUPT.call_once(|| hook);
}

/// Called on every timer interrupt, after EOI, with interrupts disabled.
static IPI_HANDLER: Once<fn()> = Once::new();

/// Runs on the receiving CPU for every inter-processor interrupt, after it
/// was acknowledged.
pub fn set_ipi_handler(handler: fn()) {
    IPI_HANDLER.call_once(|| handler);
}

static TIMER_HANDLER: Once<fn()> = Once::new();

pub fn set_timer_handler(handler: fn()) {
    TIMER_HANDLER.call_once(|| handler);
}

/// Called for CPU exceptions raised in ring 3. Must not return: the kernel
/// terminates the faulting process instead of resuming it.
static USER_FAULT_HANDLER: Once<fn(&TrapFrame) -> !> = Once::new();

pub fn set_user_fault_handler(handler: fn(&TrapFrame) -> !) {
    USER_FAULT_HANDLER.call_once(|| handler);
}

/// Runs before an interrupt returns to user mode (ADR-0044: a killed
/// process must not run user code again).
static USER_RETURN_HOOK: Once<fn()> = Once::new();

pub fn set_user_return_hook(hook: fn()) {
    USER_RETURN_HOOK.call_once(|| hook);
}

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

// Device interrupt stubs, same frame layout as exceptions.
global_asm!(
    ".pushsection .text.oceans_exceptions, \"ax\", @progbits",
    "oceans_irq_stub_timer:",
    "push 0",
    "push 48",
    "jmp {common}",
    "oceans_irq_stub_spurious:",
    "push 0",
    "push 255",
    "jmp {common}",
    "oceans_irq_stub_serial:",
    "push 0",
    "push 49",
    "jmp {common}",
    "oceans_irq_stub_keyboard:",
    "push 0",
    "push 50",
    "jmp {common}",
    "oceans_irq_stub_ipi:",
    "push 0",
    "push 51",
    "jmp {common}",
    ".popsection",
    ".pushsection .rodata.oceans_exceptions, \"a\", @progbits",
    ".balign 8",
    ".global oceans_irq_stubs",
    ".hidden oceans_irq_stubs",
    "oceans_irq_stubs:",
    ".quad oceans_irq_stub_timer",
    ".quad oceans_irq_stub_spurious",
    ".quad oceans_irq_stub_serial",
    ".quad oceans_irq_stub_keyboard",
    ".quad oceans_irq_stub_ipi",
    ".popsection",
    common = sym exception_common,
);

// Device interrupt stubs (MSI-X vectors), same frame layout.
macro_rules! device_stubs {
    ($($vector:literal),* $(,)?) => {
        global_asm!(
            ".pushsection .text.oceans_exceptions, \"ax\", @progbits",
            $(
                concat!("oceans_device_stub_", stringify!($vector), ":"),
                "push 0",
                concat!("push ", stringify!($vector)),
                "jmp {common}",
            )*
            ".popsection",
            ".pushsection .rodata.oceans_exceptions, \"a\", @progbits",
            ".balign 8",
            ".global oceans_device_stubs",
            ".hidden oceans_device_stubs",
            "oceans_device_stubs:",
            $( concat!(".quad oceans_device_stub_", stringify!($vector)), )*
            ".popsection",
            common = sym exception_common,
        );
    };
}

device_stubs! {
    64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
}

unsafe extern "C" {
    #[link_name = "oceans_device_stubs"]
    static DEVICE_STUBS: [u64; DEVICE_VECTOR_COUNT];
}

unsafe extern "C" {
    #[link_name = "oceans_exception_stubs"]
    static EXCEPTION_STUBS: [u64; EXCEPTION_COUNT];
}

unsafe extern "C" {
    #[link_name = "oceans_irq_stubs"]
    static IRQ_STUBS: [u64; 5];
}

/// Saves registers, calls the dispatcher with a pointer to the frame,
/// restores registers and returns from the interrupt.
///
/// Stack alignment: the CPU aligns RSP to 16 bytes before pushing its 5-word
/// frame; with error code, vector and 15 registers the total is 22 words, so
/// RSP is 16-byte aligned at the `call` as the SysV ABI requires.
///
/// From ring 3 (the saved CS's low bits), `swapgs` on the way in and out
/// (ADR-0089): kernel code always runs with its CPU's block in `GS`.
#[unsafe(naked)]
unsafe extern "C" fn exception_common() {
    naked_asm!(
        // [rsp]: vector, +8: error code, +16: RIP, +24: CS.
        "test byte ptr [rsp + 24], 3",
        "jz 2f",
        "swapgs",
        "2:",
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
        // [rsp]: RIP, +8: CS. Nothing may interrupt between `swapgs` and
        // `iretq` (it would see a kernel CS with the user's GS).
        "cli",
        "test byte ptr [rsp + 8], 3",
        "jz 3f",
        "swapgs",
        "3:",
        "iretq",
        dispatch = sym exception_dispatch,
    );
}

extern "C" fn exception_dispatch(frame: &mut TrapFrame) {
    let from_user = frame.cs & 3 == 3;
    if from_user
        && frame.vector < EXCEPTION_COUNT as u64
        && let Some(handler) = USER_FAULT_HANDLER.get()
    {
        handler(frame);
    }
    match frame.vector {
        VECTOR_BREAKPOINT => klog::info!("breakpoint at {:#x}, resuming", frame.rip),
        v if v == u64::from(VECTOR_TIMER) => {
            // Acknowledge first: the handler may switch threads, and the
            // next tick must still be delivered.
            super::apic::end_of_interrupt();
            if let Some(handler) = TIMER_HANDLER.get() {
                handler();
            }
        }
        v if v == u64::from(VECTOR_SERIAL) => {
            // Drain the UART before acknowledging, so a level-triggered line
            // cannot re-fire for data already handled.
            if let Some(handler) = SERIAL_HANDLER.get() {
                handler();
            }
            super::apic::end_of_interrupt();
            if let Some(hook) = AFTER_DEVICE_INTERRUPT.get() {
                hook();
            }
        }
        v if v == u64::from(VECTOR_KEYBOARD) => {
            if let Some(handler) = KEYBOARD_HANDLER.get() {
                handler();
            }
            super::apic::end_of_interrupt();
            if let Some(hook) = AFTER_DEVICE_INTERRUPT.get() {
                hook();
            }
        }
        v if v == u64::from(VECTOR_IPI) => {
            super::apic::end_of_interrupt();
            if let Some(handler) = IPI_HANDLER.get() {
                handler();
            }
        }
        v if v == u64::from(VECTOR_SPURIOUS) => {}
        v if (u64::from(DEVICE_VECTORS.start)..u64::from(DEVICE_VECTORS.end)).contains(&v) => {
            // MSI-X is edge-triggered: signal, then acknowledge. The device
            // itself is acknowledged by its driver.
            if let Some(handler) = DEVICE_HANDLER.get() {
                handler(v as u8);
            }
            super::apic::end_of_interrupt();
            if let Some(hook) = AFTER_DEVICE_INTERRUPT.get() {
                hook();
            }
        }
        _ => fatal_exception(frame),
    }
    if from_user && let Some(hook) = USER_RETURN_HOOK.get() {
        hook();
    }
}

pub fn exception_name(vector: u64) -> &'static str {
    EXCEPTION_NAMES
        .get(vector as usize)
        .copied()
        .unwrap_or("unknown")
}

fn fatal_exception(frame: &TrapFrame) -> ! {
    let name = exception_name(frame.vector);
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
        // SAFETY: the IRQ stub table above has exactly five entries.
        let (timer, spurious, serial, keyboard, ipi) = unsafe {
            (
                IRQ_STUBS[0],
                IRQ_STUBS[1],
                IRQ_STUBS[2],
                IRQ_STUBS[3],
                IRQ_STUBS[4],
            )
        };
        gates[usize::from(VECTOR_IPI)] = Gate::interrupt(ipi, selector, None);
        gates[usize::from(VECTOR_SERIAL)] = Gate::interrupt(serial, selector, None);
        gates[usize::from(VECTOR_KEYBOARD)] = Gate::interrupt(keyboard, selector, None);
        gates[usize::from(VECTOR_TIMER)] = Gate::interrupt(timer, selector, None);
        gates[usize::from(VECTOR_SPURIOUS)] = Gate::interrupt(spurious, selector, None);
        for (index, gate) in gates
            [usize::from(DEVICE_VECTORS.start)..usize::from(DEVICE_VECTORS.end)]
            .iter_mut()
            .enumerate()
        {
            // SAFETY: the device stub table has exactly DEVICE_VECTOR_COUNT
            // entries, one per vector in DEVICE_VECTORS, never written.
            *gate = Gate::interrupt(unsafe { DEVICE_STUBS[index] }, selector, None);
        }
        // Other vectors stay non-present; a stray delivery raises #GP/#NP
        // and is reported as fatal.
        Idt(gates)
    });
    load_table(idt);
    klog::debug!("IDT loaded with {EXCEPTION_COUNT} exception handlers");
}

/// Loads the IDT [`init`] built on this CPU (another CPU, ADR-0088).
pub fn load() {
    load_table(IDT.get().expect("the boot CPU builds the IDT first"));
}

fn load_table(idt: &'static Idt) {
    let pointer = DescriptorTablePointer {
        limit: (size_of::<Idt>() - 1) as u16,
        base: VirtAddr::from_ptr(idt),
    };
    // SAFETY: `idt` is 'static and immutable after initialisation, and every
    // present gate points at a stub that preserves state and returns via iretq.
    unsafe { lidt(&pointer) };
}
