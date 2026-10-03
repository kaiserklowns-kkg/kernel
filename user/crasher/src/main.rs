//! Faults on purpose so the smoke test can verify that init restarts a
//! failing service the configured number of times and then gives up.
//! Manifest grants: `log`.

#![no_std]
#![no_main]

use oceans_rt::Start;

oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    if let Some(&log) = start.handles.first() {
        let _ = oceans_rt::debug_write(log, "crasher: faulting on purpose");
    }
    // Page 0 is never mapped: this page faults and the kernel kills us.
    // (Not address 0 itself: a null write is UB the compiler may turn into
    // a trap instead of a real access.)
    let unmapped = core::ptr::without_provenance_mut::<u64>(8);
    // SAFETY: deliberately invalid; the fault is the point.
    unsafe { unmapped.write_volatile(1) };
    1
}
