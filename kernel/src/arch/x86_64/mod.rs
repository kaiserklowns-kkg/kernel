//! x86_64 support (Tier 1, ADR-0005).

mod apic;
mod context;
mod cpu;
mod gdt;
mod interrupts;
mod ioapic;
mod paging;
mod pic;
mod serial;
mod syscall;

use core::arch::asm;

use ::x86_64::instructions::{self as insn, port::Port};

pub use context::{prepare_stack, switch_context};
pub use cpu::{enable_protections, features as cpu_features};
pub use interrupts::{TrapFrame, set_after_device_interrupt, set_user_fault_handler};
pub use paging::{AddressSpace, activate_root, active_root};
pub use syscall::{SyscallFrame, enter_user, init as init_syscalls, set_kernel_stack};

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

/// Writes bytes to the console unchanged (no newline translation).
pub fn console_write_bytes(bytes: &[u8]) {
    serial::write_bytes(bytes);
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

/// Starts the periodic timer: `handler` runs on every tick, in interrupt
/// context with interrupts disabled. Requires the kernel page tables.
pub fn start_timer(hz: u32, handler: fn()) {
    apic::init();
    interrupts::set_timer_handler(handler);
    apic::start_timer(hz);
}

pub fn enable_interrupts() {
    insn::interrupts::enable();
}

/// Enables interrupts and halts until the next one, atomically (no wake-up
/// can be lost between the two).
pub fn wait_for_interrupt() {
    insn::interrupts::enable_and_hlt();
}

/// CPU timestamp counter, for benchmarks (cycles; invariant TSC on Tier 1).
pub fn cycles() -> u64 {
    // SAFETY: RDTSC is available on every x86_64 CPU and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Faulting address of the most recent page fault (CR2).
pub fn fault_address() -> u64 {
    let address: u64;
    // SAFETY: reading CR2 has no side effects.
    unsafe {
        asm!("mov {}, cr2", out(reg) address, options(nomem, nostack, preserves_flags));
    }
    address
}

/// Human-readable name of exception `vector`.
pub fn exception_name(vector: u64) -> &'static str {
    interrupts::exception_name(vector)
}

/// Where received console bytes go (set once by `enable_console_input`).
static CONSOLE_SINK: spin::Once<fn(u8)> = spin::Once::new();
/// The I/O APIC used for device interrupts, kept for future routes.
static IO_APIC: spin::Once<spin::Mutex<ioapic::IoApic>> = spin::Once::new();

fn on_serial_interrupt() {
    if let Some(sink) = CONSOLE_SINK.get() {
        serial::drain_input(sink);
    }
}

/// Routes the serial console's interrupt (global system interrupt `gsi` on
/// the I/O APIC at `io_apic`) to this CPU and delivers every received byte
/// to `sink`, in interrupt context. Requires the kernel page tables.
pub fn enable_console_input(
    io_apic: u64,
    gsi_base: u32,
    gsi: u32,
    active_low: bool,
    level_triggered: bool,
    sink: fn(u8),
) -> Result<(), &'static str> {
    CONSOLE_SINK.call_once(|| sink);
    interrupts::set_serial_handler(on_serial_interrupt);
    let io = match ioapic::IoApic::new(io_apic, gsi_base) {
        Ok(io) => IO_APIC.call_once(|| spin::Mutex::new(io)),
        Err(_) => return Err("cannot map the I/O APIC"),
    };
    let routed = without_interrupts(|| {
        io.lock().route(
            gsi,
            interrupts::VECTOR_SERIAL,
            active_low,
            level_triggered,
            apic::id(),
        )
    });
    if !routed {
        return Err("the I/O APIC does not serve the console's interrupt");
    }
    serial::enable_receive_interrupt();
    Ok(())
}
