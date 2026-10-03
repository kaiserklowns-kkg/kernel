//! System time: scheduler ticks since boot, and the wall clock taken from
//! the real-time clock at boot (ADR-0031).

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

/// Milliseconds since boot (10 ms resolution).
pub fn uptime_ms() -> u64 {
    ticks() * 1000 / u64::from(HZ)
}

/// Unix time in milliseconds at boot (tick 0); 0 while unknown.
static BOOT_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// Reads the real-time clock once. Afterwards the wall clock runs on the
/// timer, so it neither jumps nor goes backwards while the system runs.
pub fn init_wall_clock() {
    let Some(reading) = crate::arch::rtc_now() else {
        crate::klog::warn!("no usable real-time clock; wall time unknown");
        return;
    };
    let seconds = unix_seconds(&reading);
    let boot = (seconds * 1000).saturating_sub(uptime_ms());
    BOOT_UNIX_MS.store(boot, Ordering::Relaxed);
    crate::klog::info!(
        "wall clock {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        reading.year,
        reading.month,
        reading.day,
        reading.hour,
        reading.minute,
        reading.second
    );
}

/// Unix time in milliseconds, if the real-time clock was readable.
pub fn unix_ms() -> Option<u64> {
    match BOOT_UNIX_MS.load(Ordering::Relaxed) {
        0 => None,
        boot => Some(boot + uptime_ms()),
    }
}

/// Seconds since 1970-01-01 00:00:00 UTC of a valid reading.
fn unix_seconds(reading: &crate::arch::RtcReading) -> u64 {
    let days = days_from_civil(reading.year, reading.month, reading.day);
    days * 86_400
        + u64::from(reading.hour) * 3600
        + u64::from(reading.minute) * 60
        + u64::from(reading.second)
}

/// Days from 1970-01-01 to a proleptic Gregorian date at or after 2000
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: u32, month: u32, day: u32) -> u64 {
    let year = u64::from(if month <= 2 { year - 1 } else { year });
    let era = year / 400;
    let year_of_era = year - era * 400;
    let month = u64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + u64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Checks the calendar arithmetic against known dates.
pub fn self_test() {
    let at = |year, month, day| days_from_civil(year, month, day) * 86_400;
    assert_eq!(at(2000, 1, 1), 946_684_800);
    assert_eq!(at(2000, 3, 1), 951_868_800);
    assert_eq!(at(2024, 2, 29), 1_709_164_800);
    assert_eq!(at(2026, 10, 3), 1_790_985_600);
    assert_eq!(at(2100, 3, 1), 4_107_542_400);
    crate::klog::info!("calendar self-test passed");
}
