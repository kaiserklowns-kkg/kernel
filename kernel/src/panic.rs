//! Kernel panic handling: report, then stop the machine predictably.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;
use crate::klog::{self, Level};

static PANICKING: AtomicBool = AtomicBool::new(false);
static EXIT_EMULATOR_ON_PANIC: AtomicBool = AtomicBool::new(false);

/// In automated tests a panic must terminate the emulator with a failure code
/// instead of hanging until the CI timeout.
pub fn set_exit_emulator_on_panic(enabled: bool) {
    EXIT_EMULATOR_ON_PANIC.store(enabled, Ordering::Relaxed);
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    arch::disable_interrupts();

    if PANICKING.swap(true, Ordering::SeqCst) {
        // Panicked while reporting a panic; anything more could recurse.
        arch::halt_forever();
    }

    match info.location() {
        Some(loc) => klog::emergency(
            Level::Error,
            "panic",
            format_args!(
                "KERNEL PANIC at {}:{}: {}",
                loc.file(),
                loc.line(),
                info.message()
            ),
        ),
        None => klog::emergency(
            Level::Error,
            "panic",
            format_args!("KERNEL PANIC: {}", info.message()),
        ),
    }

    if EXIT_EMULATOR_ON_PANIC.load(Ordering::Relaxed) {
        arch::exit_emulator(arch::EmulatorExit::Failure);
    }
    arch::halt_forever()
}
