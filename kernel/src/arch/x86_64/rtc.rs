//! The CMOS real-time clock (ADR-0031): read once at boot for the wall
//! clock. Only this module touches ports 0x70/0x71.

use x86_64::instructions::port::Port;

const INDEX: u16 = 0x70;
const DATA: u16 = 0x71;

const SECONDS: u8 = 0x00;
const MINUTES: u8 = 0x02;
const HOURS: u8 = 0x04;
const DAY: u8 = 0x07;
const MONTH: u8 = 0x08;
const YEAR: u8 = 0x09;
const STATUS_A: u8 = 0x0a;
const STATUS_B: u8 = 0x0b;
/// The usual century register (the ACPI FADT can name another one).
const CENTURY: u8 = 0x32;

/// Status A: an update is in progress (values may be torn).
const UPDATING: u8 = 0x80;
/// Status B: values are binary, not BCD.
const BINARY: u8 = 0x04;
/// Status B: hours are 24-hour, not 12-hour with a PM bit.
const HOURS_24: u8 = 0x02;
const PM: u8 = 0x80;

fn read(register: u8) -> u8 {
    // SAFETY: the CMOS index/data ports belong to this module; bit 7 of the
    // index (NMI disable) is left clear, as firmware leaves it.
    unsafe {
        Port::<u8>::new(INDEX).write(register);
        Port::<u8>::new(DATA).read()
    }
}

/// A calendar reading, as the chip reports it (UTC on PCs we support).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reading {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

fn raw() -> [u8; 7] {
    [
        read(SECONDS),
        read(MINUTES),
        read(HOURS),
        read(DAY),
        read(MONTH),
        read(YEAR),
        read(CENTURY),
    ]
}

/// Reads the clock: waits out an update, then reads until two consecutive
/// readings agree (an update can start between registers). `None` if the
/// chip never settles or reports nonsense.
pub fn now() -> Option<Reading> {
    let mut previous = None;
    for _ in 0..1000 {
        let mut spins = 0u32;
        while read(STATUS_A) & UPDATING != 0 {
            spins += 1;
            if spins > 1_000_000 {
                return None;
            }
            core::hint::spin_loop();
        }
        let current = raw();
        if previous == Some(current) {
            return decode(current, read(STATUS_B));
        }
        previous = Some(current);
    }
    None
}

fn decode(raw: [u8; 7], status_b: u8) -> Option<Reading> {
    let binary = status_b & BINARY != 0;
    let value = |byte: u8| -> u32 {
        if binary {
            u32::from(byte)
        } else {
            u32::from(byte >> 4) * 10 + u32::from(byte & 0x0f)
        }
    };
    let [second, minute, hour, day, month, year, century] = raw;
    let mut hour_value = value(hour & !PM);
    if status_b & HOURS_24 == 0 {
        // 12-hour clock: 12 AM is 0, 12 PM is 12.
        hour_value %= 12;
        if hour & PM != 0 {
            hour_value += 12;
        }
    }
    let century = match value(century) {
        c @ 19..=99 => c,
        // No usable century register: this system cannot predate 2000.
        _ => 20,
    };
    let reading = Reading {
        year: century * 100 + value(year),
        month: value(month),
        day: value(day),
        hour: hour_value,
        minute: value(minute),
        second: value(second),
    };
    let valid = (1..=12).contains(&reading.month)
        && (1..=31).contains(&reading.day)
        && reading.hour < 24
        && reading.minute < 60
        && reading.second < 60
        && reading.year >= 2000;
    valid.then_some(reading)
}
