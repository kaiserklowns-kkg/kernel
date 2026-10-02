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
/// the kernel runs its self-tests, reports the result to the emulator and
/// exits instead of idling. Normal boots run no tests.
const SMOKE_TEST_FLAG: &str = "oceans.test=smoke";

/// Line emitted once the kernel core is initialised. CI matches on it.
const ONLINE_BANNER: &str = "OCEANS KERNEL ONLINE";

/// Architecture- and bootloader-neutral kernel entry point. Runs on the
/// bootloader's stack, in bootloader-owned page tables.
fn kernel_main(boot: &'static BootInfo) -> ! {
    panic::set_exit_emulator_on_panic(boot.cmdline_has(SMOKE_TEST_FLAG));

    klog::info!("Oceans {} on {}", env!("CARGO_PKG_VERSION"), arch::NAME);
    klog::info!("command line: {:?}", boot.cmdline());
    if boot.cmdline_truncated() {
        klog::warn!("command line truncated to {} bytes", boot::MAX_CMDLINE_LEN);
    }

    arch::init();
    memory::init(boot);

    let stack = memory::paging::allocate_kernel_stack()
        .unwrap_or_else(|err| panic!("cannot allocate the boot kernel stack: {err:?}"));
    // SAFETY: the stack was just mapped read-write in the active address
    // space and is used by nothing else. Nothing on the current stack is
    // needed afterwards: everything continues from kernel-owned statics.
    unsafe { arch::switch_stack(stack.top(), kernel_main_on_kernel_stack) }
}

/// Second boot stage, on a kernel-owned stack. Bootloader memory (including
/// the stack and page tables it gave us) is no longer used and is reclaimed.
extern "C" fn kernel_main_on_kernel_stack() -> ! {
    let boot = boot::info();
    memory::reclaim_bootloader_memory(boot);

    let smoke_test = boot.cmdline_has(SMOKE_TEST_FLAG);
    if smoke_test {
        self_test();
    }

    klog::info!("{ONLINE_BANNER}");

    if smoke_test {
        arch::exit_emulator(arch::EmulatorExit::Success);
    }
    arch::halt_forever()
}

/// Boot self-tests, run only with `oceans.test=smoke` (CI). Any failure panics.
fn self_test() {
    // A breakpoint must be delivered through the IDT and return cleanly.
    arch::breakpoint();
    memory::self_test();
    klog::info!("self-tests passed");
}
