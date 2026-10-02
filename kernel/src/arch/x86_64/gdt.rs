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

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<(GlobalDescriptorTable, Selectors)> = Once::new();

pub fn init() {
    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        let stack_top = VirtAddr::from_ptr(DOUBLE_FAULT_STACK.0.get()) + IST_STACK_SIZE as u64;
        tss.interrupt_stack_table[usize::from(DOUBLE_FAULT_IST_INDEX)] = stack_top;
        tss
    });

    let (gdt, selectors) = GDT.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code = gdt.append(Descriptor::kernel_code_segment());
        let data = gdt.append(Descriptor::kernel_data_segment());
        let tss = gdt.append(Descriptor::tss_segment(tss));
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
