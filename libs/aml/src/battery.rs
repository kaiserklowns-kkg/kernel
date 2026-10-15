//! The battery as ACPI describes it (ACPI 6.5 §10.2): what it is
//! (`_BIX`, or the older `_BIF`), how it is now (`_BST`), and whether the
//! machine runs on its adapter (`_PSR`), read into a charge, a state and
//! the time left (ADR-0153).

use alloc::vec::Vec;

use crate::eval::{Host, Machine, RunError, Value};
use crate::name::Path;

/// A capacity or rate ACPI does not know.
const UNKNOWN: u64 = 0xffff_ffff;

/// What the battery is: its capacities, in mWh or mAh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// Capacities and rates in mW and mWh, else mA and mAh.
    pub milliwatts: bool,
    pub design: Option<u64>,
    /// How much it holds when full now (it wears).
    pub last_full: Option<u64>,
}

/// How the battery is now (`_BST`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub discharging: bool,
    pub charging: bool,
    pub critical: bool,
    /// mW or mA, in or out.
    pub rate: Option<u64>,
    pub remaining: Option<u64>,
}

/// What the desktop shows: the charge, what it does, the minutes left
/// (to empty, or to full).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub percent: u8,
    pub charging: bool,
    pub discharging: bool,
    pub minutes: Option<u16>,
}

/// The battery's `_STA` says it is there (bit 4).
pub fn present<H: Host>(machine: &mut Machine<H>, battery: &Path) -> Result<bool, RunError> {
    Ok(machine.status(battery)? & 0x10 != 0)
}

/// `_BIX` (ACPI 4.0 on), else `_BIF`.
pub fn info<H: Host>(machine: &mut Machine<H>, battery: &Path) -> Result<Info, RunError> {
    let bix = battery.child(*b"_BIX");
    let (items, first) = if machine.exists(&bix) {
        // _BIX starts with its revision.
        (integers(machine, &bix)?, 1)
    } else {
        (integers(machine, &battery.child(*b"_BIF"))?, 0)
    };
    let at = |i: usize| items.get(first + i).copied();
    Ok(Info {
        milliwatts: at(0) == Some(0),
        design: at(1).filter(|&v| known(v)),
        last_full: at(2).filter(|&v| known(v)),
    })
}

/// `_BST`.
pub fn status<H: Host>(machine: &mut Machine<H>, battery: &Path) -> Result<Status, RunError> {
    let items = integers(machine, &battery.child(*b"_BST"))?;
    let state = items.first().copied().unwrap_or(0);
    Ok(Status {
        discharging: state & 1 != 0,
        charging: state & 2 != 0,
        critical: state & 4 != 0,
        rate: items.get(1).copied().filter(|&v| known(v)),
        remaining: items.get(2).copied().filter(|&v| known(v)),
    })
}

/// The adapter's `_PSR`: on mains power.
pub fn on_power<H: Host>(machine: &mut Machine<H>, adapter: &Path) -> Result<bool, RunError> {
    Ok(machine.evaluate_int(&adapter.child(*b"_PSR"), Vec::new())? != 0)
}

/// The charge and the time left, from what the battery is and how it is.
pub fn reading(info: &Info, status: &Status) -> Option<Reading> {
    let remaining = status.remaining?;
    let full = info.last_full.or(info.design).filter(|&f| f > 0)?;
    let percent = (remaining.min(full) * 100 / full) as u8;
    let rate = status.rate.filter(|&r| r > 0);
    let minutes = match (status.discharging, status.charging, rate) {
        (true, _, Some(rate)) => Some(remaining * 60 / rate),
        (_, true, Some(rate)) => Some(full.saturating_sub(remaining) * 60 / rate),
        _ => None,
    }
    .map(|m| m.min(u64::from(u16::MAX)) as u16);
    Some(Reading {
        percent,
        charging: status.charging,
        discharging: status.discharging,
        minutes,
    })
}

fn known(v: u64) -> bool {
    v != UNKNOWN && v != u64::MAX
}

/// A method's package of integers (strings and the like as zero).
fn integers<H: Host>(machine: &mut Machine<H>, path: &Path) -> Result<Vec<u64>, RunError> {
    match machine.evaluate(path, Vec::new())? {
        Value::Package(items) => Ok(items
            .iter()
            .map(|item| match item {
                Value::Integer(v) => *v,
                _ => 0,
            })
            .collect()),
        _ => Err(RunError::Type("a battery's package")),
    }
}
