//! Global descriptor tables and task state segments, one of each per CPU
//! (ADR-0088): a TSS holds its CPU's ring-0 stack and double-fault stack,
//! and a GDT names one TSS.
//!
//! Replaces the bootloader's GDT with one the kernel owns. The segment
//! layout is the same on every CPU.

use core::cell::UnsafeCell;

use ::x86_64::VirtAddr;
use ::x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use ::x86_64::instructions::tables::load_tss;
use ::x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use ::x86_64::structures::tss::TaskStateSegment;
use spin::Once;

use super::MAX_CPUS;
use crate::klog;

/// IST slot used for double faults, so they run on a known-good stack even
/// when the fault was caused by a kernel stack overflow.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const IST_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack(UnsafeCell<[u8; IST_STACK_SIZE]>);

// SAFETY: the stack is only ever touched by the CPU while handling a double
// fault; Rust code never reads or writes it.
unsafe impl Sync for Stack {}

static DOUBLE_FAULT_STACK: Stack = Stack(UnsafeCell::new([0; IST_STACK_SIZE]));

// Selector values. The order kernel code, kernel data, user data, user code
// is what `syscall`/`sysret` require (see `syscall::init`).
pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const USER_DATA: u16 = 0x18 | 3;
pub const USER_CODE: u16 = 0x20 | 3;

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

/// A TSS is written after loading (RSP0 on every thread switch), so it
/// lives in an `UnsafeCell` rather than behind `Once`.
struct TssCell(UnsafeCell<TaskStateSegment>);

// SAFETY: each CPU's TSS is written only by that CPU: by `init_cpu` (once,
// before loading it) and `set_kernel_stack` (interrupts disabled); read by
// that CPU's hardware.
unsafe impl Sync for TssCell {}

static TSS: [TssCell; MAX_CPUS] =
    [const { TssCell(UnsafeCell::new(TaskStateSegment::new())) }; MAX_CPUS];
static GDT: [Once<(GlobalDescriptorTable, Selectors)>; MAX_CPUS] =
    [const { Once::new() }; MAX_CPUS];

/// Sets the stack CPU `cpu` switches to on an interrupt or exception from
/// ring 3: the running thread's kernel stack. Interrupts must be disabled,
/// and `cpu` must be the calling CPU.
pub fn set_kernel_stack(cpu: usize, top: u64) {
    // The TSS is packed: RSP0 sits at offset 4, so it is written as two
    // 4-byte-aligned halves (volatile: the CPU reads it, Rust never does).
    // SAFETY: see `TssCell`; the pointer is in bounds of the TSS.
    unsafe {
        let rsp0 = (&raw mut (*TSS[cpu].0.get()).privilege_stack_table).cast::<u32>();
        rsp0.write_volatile(top as u32);
        rsp0.add(1).write_volatile((top >> 32) as u32);
    }
}

/// The boot CPU's GDT and TSS, with a static double-fault stack.
pub fn init() {
    let top = VirtAddr::from_ptr(DOUBLE_FAULT_STACK.0.get()) + IST_STACK_SIZE as u64;
    init_cpu(0, top.as_u64());
    klog::debug!("GDT and TSS loaded");
}

/// Loads CPU `cpu`'s own GDT and TSS, with its double-fault stack ending at
/// `double_fault_top`. Runs once, on that CPU.
pub fn init_cpu(cpu: usize, double_fault_top: u64) {
    // SAFETY: runs once per CPU, on that CPU, before its TSS is loaded;
    // nothing else accesses it yet.
    let tss: &'static TaskStateSegment = unsafe {
        let tss = &mut *TSS[cpu].0.get();
        tss.interrupt_stack_table[usize::from(DOUBLE_FAULT_IST_INDEX)] =
            VirtAddr::new(double_fault_top);
        tss
    };

    let (gdt, selectors) = GDT[cpu].call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code = gdt.append(Descriptor::kernel_code_segment());
        let data = gdt.append(Descriptor::kernel_data_segment());
        let user_data = gdt.append(Descriptor::user_data_segment());
        let user_code = gdt.append(Descriptor::user_code_segment());
        let tss = gdt.append(Descriptor::tss_segment(tss));
        assert_eq!(
            [code.0, data.0, user_data.0, user_code.0],
            [KERNEL_CODE, KERNEL_DATA, USER_DATA, USER_CODE],
            "GDT layout must match the syscall selector constants"
        );
        (gdt, Selectors { code, data, tss })
    });

    gdt.load();
    // SAFETY: the selectors index descriptors in the GDT just loaded, which
    // lives in a 'static and is never modified again.
    unsafe {
        CS::set_reg(selectors.code);
        SS::set_reg(selectors.data);
        DS::set_reg(selectors.data);
        ES::set_reg(selectors.data);
        load_tss(selectors.tss);
    }
}
