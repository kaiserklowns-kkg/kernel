//! `date`: the current date and time in UTC (ADR-0031).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_rt::Start;
use utils::{EXIT_FAILED, console};

oceans_rt::manifest!(b"grant out\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let (mut out, _) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let Some(ms) = oceans_rt::unix_time_ms() else {
        let _ = writeln!(out, "date: the time is unknown (no real-time clock)");
        return EXIT_FAILED;
    };
    let seconds = ms / 1000;
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let time = seconds % 86_400;
    let _ = writeln!(
        out,
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        time / 3600,
        time % 3600 / 60,
        time % 60
    );
    0
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}
