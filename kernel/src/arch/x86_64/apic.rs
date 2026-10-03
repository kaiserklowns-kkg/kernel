//! Local APIC: interrupt acknowledgement and the per-CPU timer.
//!
//! xAPIC (MMIO) mode, which every Tier 1 CPU supports; x2APIC comes with
//! SMP. The timer is calibrated once against the PIT, which is present on
//! all PC-compatible platforms (and only used for this one measurement).

use ::x86_64::instructions::port::Port;
use ::x86_64::registers::model_specific::Msr;
use spin::Once;

use super::interrupts::{VECTOR_SPURIOUS, VECTOR_TIMER};
use crate::klog;
use crate::memory::paging;

const IA32_APIC_BASE: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

// Register offsets.
const TASK_PRIORITY: usize = 0x080;
const EOI: usize = 0x0b0;
const SPURIOUS: usize = 0x0f0;
const LVT_TIMER: usize = 0x320;
const TIMER_INITIAL: usize = 0x380;
const TIMER_CURRENT: usize = 0x390;
const TIMER_DIVIDE: usize = 0x3e0;

const SPURIOUS_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const DIVIDE_BY_16: u32 = 0b0011;

const PIT_FREQUENCY: u32 = 1_193_182;
const CALIBRATION_MS: u32 = 10;

/// Virtual address of the local APIC registers.
static BASE: Once<usize> = Once::new();

fn write(offset: usize, value: u32) {
    let base = *BASE.get().expect("apic::init runs first");
    // SAFETY: `base` maps the 4 KiB APIC register page uncached (`init`);
    // `offset` is a register offset within it.
    unsafe { ((base + offset) as *mut u32).write_volatile(value) }
}

fn read(offset: usize) -> u32 {
    let base = *BASE.get().expect("apic::init runs first");
    // SAFETY: as for `write`.
    unsafe { ((base + offset) as *const u32).read_volatile() }
}

/// Enables the local APIC with all LVT sources masked except as configured
/// later.
pub fn init() {
    let mut msr = Msr::new(IA32_APIC_BASE);
    // SAFETY: IA32_APIC_BASE exists on every x86_64 CPU; setting the enable
    // bit is the architected way to turn the xAPIC on.
    let value = unsafe { msr.read() };
    unsafe { msr.write(value | APIC_BASE_ENABLE) };
    let phys = value & APIC_BASE_ADDRESS_MASK;

    let virt = paging::map_mmio(phys, 4096)
        .unwrap_or_else(|err| panic!("cannot map the local APIC at {phys:#x}: {err:?}"));
    BASE.call_once(|| virt as usize);

    write(TASK_PRIORITY, 0);
    write(SPURIOUS, SPURIOUS_ENABLE | u32::from(VECTOR_SPURIOUS));
    write(LVT_TIMER, LVT_MASKED);
    klog::debug!("local APIC at {phys:#x} enabled");
}

/// This CPU's local APIC ID (I/O APIC routing destination).
pub fn id() -> u8 {
    (read(0x020) >> 24) as u8
}

/// Signals the end of the current interrupt to the local APIC.
pub fn end_of_interrupt() {
    if BASE.get().is_some() {
        write(EOI, 0);
    }
}

/// Starts the periodic timer at `hz` interrupts per second on vector
/// [`VECTOR_TIMER`].
pub fn start_timer(hz: u32) {
    // Measure how far the APIC timer counts in CALIBRATION_MS.
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(LVT_TIMER, LVT_MASKED);
    write(TIMER_INITIAL, u32::MAX);
    pit_wait(CALIBRATION_MS);
    let elapsed = u32::MAX - read(TIMER_CURRENT);
    write(TIMER_INITIAL, 0);

    let per_second = u64::from(elapsed) * u64::from(1000 / CALIBRATION_MS);
    let initial = (per_second / u64::from(hz)).max(1);
    let initial = u32::try_from(initial).unwrap_or(u32::MAX);

    write(LVT_TIMER, LVT_PERIODIC | u32::from(VECTOR_TIMER));
    write(TIMER_INITIAL, initial);
    klog::info!("APIC timer: {} kHz bus/16, {hz} Hz tick", per_second / 1000);
}

/// Busy-waits `ms` milliseconds using PIT channel 2 in one-shot mode.
fn pit_wait(ms: u32) {
    let count = PIT_FREQUENCY * ms / 1000;
    assert!(count <= 0xffff, "PIT one-shot limited to ~54 ms");
    let mut control = Port::<u8>::new(0x61);
    let mut command = Port::<u8>::new(0x43);
    let mut channel2 = Port::<u8>::new(0x42);

    // SAFETY: ports 0x42/0x43/0x61 belong to the PIT and the speaker gate;
    // channel 2 is used only here, with interrupts disabled, during boot.
    unsafe {
        // Gate on, speaker off.
        let gate = (control.read() & !0x02) | 0x01;
        control.write(gate & !0x01);
        // Channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count).
        command.write(0b1011_0000);
        channel2.write(count as u8);
        channel2.write((count >> 8) as u8);
        // Rising gate edge starts the count.
        control.write(gate);

        // OUT2 (bit 5) goes high at terminal count. Bounded so a missing PIT
        // is reported instead of hanging the boot.
        for _ in 0..100_000_000u32 {
            if control.read() & 0x20 != 0 {
                return;
            }
            core::hint::spin_loop();
        }
    }
    panic!("PIT channel 2 did not count down; cannot calibrate the APIC timer");
}
