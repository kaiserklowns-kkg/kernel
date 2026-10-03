//! Global descriptor table and task state segment.
//!
//! Replaces the bootloader's GDT with one the kernel owns. User segments are
//! added in Phase 2 together with the syscall entry path.

use core::cell::UnsafeCell;

use ::x86_64::VirtAddr;
use ::x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use ::x86_64::instructions::tables::load_tss;
use ::x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use ::x86_64::structures::tss::TaskStateSegment;
use spin::Once;

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

/// The TSS is written after loading (RSP0 on every thread switch), so it
/// lives in an `UnsafeCell` rather than behind `Once`.
struct TssCell(UnsafeCell<TaskStateSegment>);

// SAFETY: written only by `init` (once) and `set_kernel_stack` (with
// interrupts disabled on the only CPU); read by the CPU.
unsafe impl Sync for TssCell {}

static TSS: TssCell = TssCell(UnsafeCell::new(TaskStateSegment::new()));
static GDT: Once<(GlobalDescriptorTable, Selectors)> = Once::new();

/// Sets the stack the CPU switches to on an interrupt or exception from
/// ring 3: the running thread's kernel stack. Interrupts must be disabled.
pub fn set_kernel_stack(top: u64) {
    // The TSS is packed: RSP0 sits at offset 4, so it is written as two
    // 4-byte-aligned halves (volatile: the CPU reads it, Rust never does).
    // SAFETY: see `TssCell`; the pointer is in bounds of the TSS.
    unsafe {
        let rsp0 = (&raw mut (*TSS.0.get()).privilege_stack_table).cast::<u32>();
        rsp0.write_volatile(top as u32);
        rsp0.add(1).write_volatile((top >> 32) as u32);
    }
}

pub fn init() {
    // SAFETY: runs once, before the TSS is loaded; nothing else accesses it.
    let tss: &'static TaskStateSegment = unsafe {
        let tss = &mut *TSS.0.get();
        let stack_top = VirtAddr::from_ptr(DOUBLE_FAULT_STACK.0.get()) + IST_STACK_SIZE as u64;
        tss.interrupt_stack_table[usize::from(DOUBLE_FAULT_IST_INDEX)] = stack_top;
        tss
    };

    let (gdt, selectors) = GDT.call_once(|| {
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
    klog::debug!("GDT and TSS loaded");
}
