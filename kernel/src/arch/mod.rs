//! Architecture layer. Each supported architecture exposes the same surface;
//! the rest of the kernel uses only what is re-exported here.

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use self::x86_64::{
    AddressSpace, DEVICE_VECTORS, EmulatorExit, MAX_CPUS, NAME, RtcReading, SyscallFrame,
    TrapFrame, activate_root, active_root, breakpoint, console_write, console_write_bytes,
    cpu_features, cpu_index, cycles, disable_interrupts, early_init, enable_console_input,
    enable_interrupts, enable_keyboard, enable_protections, enter_user, exception_name,
    exit_emulator, fault_address, flush_tlb_all, halt_forever, hardware_random, init,
    init_secondary, init_syscalls, msi_message, prepare_stack, register_cpu, rtc_now, send_ipi,
    send_ipi_to_cpu, serial_overruns, set_after_device_interrupt, set_cpu_protections,
    set_device_handler, set_ipi_handler, set_kernel_stack, set_user_fault_handler,
    set_user_return_hook, start_secondary_timer, start_timer, switch_context, switch_stack,
    wait_for_interrupt, without_interrupts,
};

/// Switching off and restarting (ADR-0085).
#[cfg(target_arch = "x86_64")]
pub use self::x86_64::power;
