//! Bringing up the other CPUs (ADR-0088).
//!
//! The bootloader starts every CPU it finds and parks it, on its own stack
//! and page tables, in memory the kernel reclaims early. So before that
//! memory is reclaimed, each CPU is started in turn and must, within a
//! second:
//! 1. turn on the same protections as the boot CPU (NX first: the kernel's
//!    page tables use it);
//! 2. switch to the kernel's page tables and to a kernel stack of its own:
//!    from then on it no longer touches bootloader memory;
//! 3. load its own GDT and TSS (with its own double-fault stack), the
//!    shared IDT, and enable its local APIC.
//!
//! It then waits for interrupts. Threads are still scheduled on the boot
//! CPU only; scheduling on every CPU is the next step (ADR-0089). Until
//! then, the other CPUs answer inter-processor interrupts, which the boot
//! self-test checks.
//!
//! A CPU that does not leave bootloader memory in time is given up on, and
//! that memory is then never reclaimed: the CPU may still be reading it.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use spin::Mutex;

use crate::arch::{self, MAX_CPUS};
use crate::boot::{self, BootInfo};
use crate::klog;
use crate::memory::paging::{self, KernelStack};

/// How long a started CPU gets for each step.
const START_TIMEOUT_MS: u64 = 1000;

/// CPUs running the kernel (the boot CPU included).
static ONLINE_COUNT: AtomicUsize = AtomicUsize::new(1);
static APIC_IDS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
/// Inter-processor interrupts each CPU has received.
static IPIS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// The CPU being started, and its stacks (kernel, double fault).
static STARTING: AtomicUsize = AtomicUsize::new(0);
static STACK_TOP: AtomicU64 = AtomicU64::new(0);
static DOUBLE_FAULT_TOP: AtomicU64 = AtomicU64::new(0);
/// It runs on its kernel stack and the kernel's page tables.
static LEFT_BOOTLOADER: AtomicBool = AtomicBool::new(false);
/// It is set up.
static ARRIVED: AtomicBool = AtomicBool::new(false);
/// The started CPUs' stacks, kept for good.
static STACKS: Mutex<Vec<KernelStack>> = Mutex::new(Vec::new());

/// Starts every other CPU the bootloader reported. Runs on the boot CPU,
/// before bootloader memory is reclaimed. Returns `false` if a CPU may
/// still be using bootloader memory: it must then not be reclaimed.
pub fn start(boot: &BootInfo) -> bool {
    arch::set_ipi_handler(on_ipi);
    let Some(bsp) = boot.bsp_lapic_id() else {
        klog::info!("the bootloader reported no other CPUs");
        return true;
    };
    APIC_IDS[0].store(u64::from(bsp), Ordering::Relaxed);
    arch::register_cpu(0, bsp);
    if boot.cpus_truncated() {
        klog::warn!("more than {MAX_CPUS} CPUs; the rest stay halted");
    }
    for (slot, &apic_id) in boot.secondary_cpus().iter().enumerate() {
        let index = ONLINE_COUNT.load(Ordering::Relaxed);
        match start_one(slot, index, apic_id) {
            Ok(()) => {
                ONLINE_COUNT.fetch_add(1, Ordering::Release);
            }
            Err(Stuck::Unstarted(problem)) => {
                klog::warn!("CPU with APIC ID {apic_id} not started: {problem}");
            }
            Err(Stuck::InBootloader) => {
                klog::error!("CPU with APIC ID {apic_id} did not start; bootloader memory is kept");
                return false;
            }
            Err(Stuck::Setup) => {
                klog::error!("CPU with APIC ID {apic_id} did not finish starting");
            }
        }
    }
    klog::info!("{} CPUs online", count());
    true
}

enum Stuck {
    /// Not started: nothing to wait for.
    Unstarted(&'static str),
    /// Started, but it may still use bootloader memory.
    InBootloader,
    /// Off bootloader memory, but not set up: it stays out.
    Setup,
}

fn start_one(slot: usize, index: usize, apic_id: u32) -> Result<(), Stuck> {
    let stack = paging::allocate_kernel_stack().map_err(|_| Stuck::Unstarted("no memory"))?;
    let double_fault =
        paging::allocate_kernel_stack().map_err(|_| Stuck::Unstarted("no memory"))?;
    STARTING.store(index, Ordering::Relaxed);
    STACK_TOP.store(stack.top(), Ordering::Relaxed);
    DOUBLE_FAULT_TOP.store(double_fault.top(), Ordering::Relaxed);
    LEFT_BOOTLOADER.store(false, Ordering::Relaxed);
    ARRIVED.store(false, Ordering::Relaxed);
    APIC_IDS[index].store(u64::from(apic_id), Ordering::Relaxed);
    arch::register_cpu(index, apic_id);
    {
        let mut stacks = STACKS.lock();
        stacks.push(stack);
        stacks.push(double_fault);
    }
    boot::start_secondary(slot, secondary_entry, index as u64);
    if !wait(&LEFT_BOOTLOADER) {
        return Err(Stuck::InBootloader);
    }
    if !wait(&ARRIVED) {
        APIC_IDS[index].store(u64::MAX, Ordering::Relaxed);
        arch::register_cpu(index, u32::MAX);
        return Err(Stuck::Setup);
    }
    Ok(())
}

/// Waits up to [`START_TIMEOUT_MS`] for `flag` (interrupts are not running
/// yet: the delay is counted in I/O cycles).
fn wait(flag: &AtomicBool) -> bool {
    for _ in 0..START_TIMEOUT_MS {
        if flag.load(Ordering::Acquire) {
            return true;
        }
        arch::power::delay_ms(1);
    }
    flag.load(Ordering::Acquire)
}

/// A started CPU, on the bootloader's stack and page tables.
extern "C" fn secondary_entry(_index: u64) -> ! {
    // NX before the kernel's tables, which use it.
    arch::set_cpu_protections();
    let top = STACK_TOP.load(Ordering::Acquire);
    // SAFETY: the kernel's page tables map this code, the direct map (the
    // bootloader's stack, until the switch below) and the kernel stack
    // allocated for this CPU; the stack is this CPU's alone.
    unsafe {
        arch::activate_root(paging::kernel_root());
        arch::switch_stack(top, secondary_main)
    }
}

/// The started CPU, on its kernel stack and the kernel's page tables.
extern "C" fn secondary_main() -> ! {
    LEFT_BOOTLOADER.store(true, Ordering::Release);
    let index = STARTING.load(Ordering::Acquire);
    arch::init_secondary(index, DOUBLE_FAULT_TOP.load(Ordering::Acquire));
    ARRIVED.store(true, Ordering::Release);
    loop {
        arch::wait_for_interrupt();
    }
}

/// Inter-processor interrupt: counted.
fn on_ipi() {
    IPIS[arch::cpu_index()].fetch_add(1, Ordering::Relaxed);
}

/// CPUs running the kernel.
pub fn count() -> usize {
    ONLINE_COUNT.load(Ordering::Acquire)
}

/// Smoke test: every other CPU answers an inter-processor interrupt on its
/// own (QEMU runs with four).
pub fn self_test() {
    let cpus = count();
    assert!(
        cpus >= 2,
        "the smoke test runs with several CPUs; {cpus} online"
    );
    for index in 1..cpus {
        let before = IPIS[index].load(Ordering::Relaxed);
        let apic_id = APIC_IDS[index].load(Ordering::Relaxed) as u32;
        arch::without_interrupts(|| arch::send_ipi(apic_id));
        let deadline = crate::time::ticks() + crate::time::ms_to_ticks(1000);
        while IPIS[index].load(Ordering::Relaxed) == before {
            assert!(
                crate::time::ticks() < deadline,
                "CPU {index} (APIC ID {apic_id}) did not answer an interrupt"
            );
            crate::sched::sleep_ms(1);
        }
    }
    assert_eq!(
        IPIS[0].load(Ordering::Relaxed),
        0,
        "the boot CPU took another's IPI"
    );
    klog::info!("self-test passed: {} CPUs answered an interrupt", cpus - 1);
}
