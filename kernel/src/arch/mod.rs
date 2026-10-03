//! Architecture layer. Each supported architecture exposes the same surface;
//! the rest of the kernel uses only what is re-exported here.

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use self::x86_64::{
    AddressSpace, EmulatorExit, NAME, SyscallFrame, TrapFrame, activate_root, active_root,
    breakpoint, console_write, console_write_bytes, cpu_features, cycles, disable_interrupts,
    early_init, enable_console_input, enable_interrupts, enable_protections, enter_user,
    exception_name, exit_emulator, fault_address, halt_forever, init, init_syscalls, prepare_stack,
    set_after_device_interrupt, set_kernel_stack, set_user_fault_handler, start_timer,
    switch_context, switch_stack, wait_for_interrupt, without_interrupts,
};
