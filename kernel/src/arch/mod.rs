//! Architecture layer. Each supported architecture exposes the same surface;
//! the rest of the kernel uses only what is re-exported here.

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use self::x86_64::{
    EmulatorExit, NAME, breakpoint, console_write, disable_interrupts, early_init, exit_emulator,
    halt_forever, init, without_interrupts,
};
