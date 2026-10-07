//! x86_64 support (Tier 1, ADR-0005).

mod apic;
mod context;
mod cpu;
mod gdt;
mod interrupts;
mod ioapic;
mod keyboard;
mod paging;
mod percpu;
mod pic;
pub mod power;
mod rtc;
mod serial;
mod syscall;

use core::arch::asm;
use core::sync::atomic::{AtomicU32, Ordering};

use ::x86_64::instructions::{self as insn, port::Port};

pub use context::{prepare_stack, switch_context};
pub use cpu::{enable_protections, features as cpu_features};
pub use interrupts::{
    DEVICE_VECTORS, TrapFrame, set_after_device_interrupt, set_device_handler, set_ipi_handler,
    set_user_fault_handler, set_user_return_hook,
};
pub use paging::{AddressSpace, activate_root, active_root};
pub use rtc::{Reading as RtcReading, now as rtc_now};
pub use syscall::{SyscallFrame, enter_user, init as init_syscalls, set_kernel_stack};

pub const NAME: &str = "x86_64";

/// The most CPUs Oceans uses (ADR-0088); more are left halted.
pub const MAX_CPUS: usize = 64;

/// Local APIC IDs by CPU index (`u32::MAX`: no such CPU). Index 0 is the
/// boot CPU.
static CPU_APIC_IDS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(u32::MAX) }; MAX_CPUS];

/// Records that CPU `index` has local APIC ID `apic_id`.
pub fn register_cpu(index: usize, apic_id: u32) {
    CPU_APIC_IDS[index].store(apic_id, Ordering::Release);
}

/// The calling CPU's index (ADR-0089): from its per-CPU block, behind
/// `GS` whenever the CPU runs kernel code. Stable only while the caller
/// cannot move to another CPU (interrupts disabled, or no preemption).
pub fn cpu_index() -> usize {
    percpu::index()
}

/// Brings this CPU (index `index`, not the boot CPU) to where the boot CPU
/// is: its per-CPU block, its own GDT and TSS (double faults on the stack
/// ending at `double_fault_top`), the shared IDT, its local APIC, the
/// `syscall` instruction. Its protections ([`set_cpu_protections`]) must
/// already be on.
pub fn init_secondary(index: usize, double_fault_top: u64) {
    percpu::init(index);
    gdt::init_cpu(index, double_fault_top);
    interrupts::load();
    apic::init();
    syscall::init_cpu();
}

/// Interrupts CPU `index` (its IPI handler runs there).
pub fn send_ipi_to_cpu(index: usize) {
    let apic_id = CPU_APIC_IDS[index].load(Ordering::Acquire);
    if apic_id != u32::MAX {
        send_ipi(apic_id);
    }
}

/// Starts this CPU's periodic timer with the boot CPU's calibration (the
/// local APIC timers of one machine run at one rate).
pub fn start_secondary_timer() {
    apic::start_timer_calibrated();
}

/// Throws away every TLB entry of this CPU, the kernel's global ones too.
pub fn flush_tlb_all() {
    use ::x86_64::registers::control::{Cr4, Cr4Flags};
    let flags = Cr4::read();
    if flags.contains(Cr4Flags::PAGE_GLOBAL) {
        // SAFETY: turning PGE off and on again flushes the TLB, global
        // entries included; nothing else changes.
        unsafe {
            Cr4::write(flags - Cr4Flags::PAGE_GLOBAL);
            Cr4::write(flags);
        }
    } else {
        ::x86_64::instructions::tlb::flush_all();
    }
}

/// [`enable_protections`] on another CPU, quietly.
pub fn set_cpu_protections() {
    cpu::set_protections(&cpu::features());
}

/// Interrupts the CPU with local APIC ID `apic_id` (the handler set with
/// [`set_ipi_handler`] runs there).
pub fn send_ipi(apic_id: u32) {
    apic::send_ipi(apic_id as u8, interrupts::VECTOR_IPI);
}

/// Brings up what logging needs. Runs before anything else, so it must not
/// log, allocate or fault.
pub fn early_init() {
    // Logging asks for the CPU's index (`percpu`): the boot CPU's block
    // comes first.
    percpu::init(0);
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

/// A 64-bit value from the CPU's random-number instructions: RDSEED (a
/// true random seed) if present, else RDRAND; `None` without either or if
/// the hardware keeps failing (it may underflow under load).
pub fn hardware_random() -> Option<u64> {
    use core::arch::x86_64::{__cpuid, __cpuid_count, _rdrand64_step, _rdseed64_step};

    static SUPPORT: spin::Once<(bool, bool)> = spin::Once::new();
    let &(rdseed, rdrand) = SUPPORT.call_once(|| {
        let max_leaf = __cpuid(0).eax;
        let rdrand = __cpuid(1).ecx & (1 << 30) != 0;
        let rdseed = max_leaf >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0;
        (rdseed, rdrand)
    });

    #[target_feature(enable = "rdseed")]
    fn seed() -> Option<u64> {
        let mut value = 0;
        // RDSEED is present (checked by the caller); it writes `value` and
        // reports success.
        (_rdseed64_step(&mut value) == 1).then_some(value)
    }
    #[target_feature(enable = "rdrand")]
    fn random() -> Option<u64> {
        let mut value = 0;
        // RDRAND is present (checked by the caller).
        (_rdrand64_step(&mut value) == 1).then_some(value)
    }

    if rdseed {
        for _ in 0..64 {
            // SAFETY: the CPU supports RDSEED (CPUID leaf 7).
            if let Some(value) = unsafe { seed() } {
                return Some(value);
            }
            core::hint::spin_loop();
        }
    }
    if rdrand {
        for _ in 0..10 {
            // SAFETY: the CPU supports RDRAND (CPUID leaf 1).
            if let Some(value) = unsafe { random() } {
                return Some(value);
            }
        }
    }
    None
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

/// The MSI/MSI-X message (address, data) that delivers `vector` to this
/// CPU: fixed delivery, edge triggered, physical destination (ADR-0021).
pub fn msi_message(vector: u8) -> (u64, u32) {
    let address = 0xfee0_0000 | (u64::from(apic::id()) << 12);
    (address, u32::from(vector))
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

fn on_keyboard_interrupt() {
    if let Some(sink) = CONSOLE_SINK.get() {
        keyboard::drain(*sink);
    }
}

/// The I/O APIC at `address`, set up on first use.
fn io_apic(
    address: u64,
    gsi_base: u32,
) -> Result<&'static spin::Mutex<ioapic::IoApic>, &'static str> {
    if let Some(io) = IO_APIC.get() {
        return Ok(io);
    }
    match ioapic::IoApic::new(address, gsi_base) {
        Ok(io) => Ok(IO_APIC.call_once(|| spin::Mutex::new(io))),
        Err(_) => Err("cannot map the I/O APIC"),
    }
}

/// Routes the PS/2 keyboard's interrupt (global system interrupt `gsi`)
/// and delivers its key presses to `sink` as console bytes (ADR-0029).
pub fn enable_keyboard(
    io_apic_address: u64,
    gsi_base: u32,
    gsi: u32,
    active_low: bool,
    level_triggered: bool,
    sink: fn(u8),
) -> Result<(), &'static str> {
    CONSOLE_SINK.call_once(|| sink);
    if !keyboard::init() {
        return Err("no PS/2 controller");
    }
    interrupts::set_keyboard_handler(on_keyboard_interrupt);
    let io = io_apic(io_apic_address, gsi_base)?;
    let routed = without_interrupts(|| {
        io.lock().route(
            gsi,
            interrupts::VECTOR_KEYBOARD,
            active_low,
            level_triggered,
            apic::id(),
        )
    });
    if routed {
        Ok(())
    } else {
        Err("the I/O APIC does not serve the keyboard's interrupt")
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
    let io = self::io_apic(io_apic, gsi_base)?;
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
