//! System time in scheduler ticks.

use core::sync::atomic::{AtomicU64, Ordering};

/// Timer interrupts per second.
pub const HZ: u32 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Ticks since the timer started.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Converts milliseconds to ticks, rounding up so sleeps are never short.
pub fn ms_to_ticks(ms: u64) -> u64 {
    (ms * u64::from(HZ)).div_ceil(1000)
}

/// Advances time by one tick. Called only from the timer interrupt.
pub(crate) fn advance() -> u64 {
    TICKS.fetch_add(1, Ordering::Relaxed) + 1
}
