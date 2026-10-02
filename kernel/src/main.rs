//! The Oceans kernel.
//!
//! Boot flow (Phase 1, ADR-0002):
//!
//! ```text
//! bootloader → boot::<protocol> → arch::early_init (console)
//!            → kernel_main → arch::init (GDT/TSS/IDT/PIC) → memory::init
//!            → OCEANS KERNEL ONLINE
//! ```
//!
//! Everything protocol-specific lives in [`boot`], everything CPU-specific in
//! [`arch`]; `kernel_main` only sees the neutral [`boot::BootInfo`].

#![no_std]
#![no_main]

mod arch;
mod boot;
mod klog;
mod memory;
mod panic;

use boot::BootInfo;

/// Kernel command-line flag that turns a boot into an automated smoke test:
/// the kernel reports its result to the emulator and exits instead of idling.
const SMOKE_TEST_FLAG: &str = "oceans.test=smoke";

/// Line emitted once the kernel core is initialised. CI matches on it.
const ONLINE_BANNER: &str = "OCEANS KERNEL ONLINE";

/// Architecture- and bootloader-neutral kernel entry point.
fn kernel_main(boot: &BootInfo) -> ! {
    let smoke_test = boot.cmdline_has(SMOKE_TEST_FLAG);
    panic::set_exit_emulator_on_panic(smoke_test);

    klog::info!("Oceans {} on {}", env!("CARGO_PKG_VERSION"), arch::NAME);
    klog::info!("command line: {:?}", boot.cmdline());

    arch::init();
    memory::init(boot);

    // Exercise the exception path end to end: a breakpoint must be delivered
    // through the IDT and return cleanly.
    arch::breakpoint();

    klog::info!("{ONLINE_BANNER}");

    if smoke_test {
        arch::exit_emulator(arch::EmulatorExit::Success);
    }
    arch::halt_forever()
}
