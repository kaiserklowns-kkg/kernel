//! What the user does to the machine (ADR-0160): the power button, the
//! lid, and the embedded controller's events, polled. No interrupt is
//! taken (the SCI stays masked): the status registers are read a few
//! times a second, as their bits stay set until cleared.
//!
//! - **The fixed power button**: PM1's status register, bit 8, cleared
//!   by writing it back.
//! - **The EC's events**: its status's SCI_EVT bit; a query (0x84) gives
//!   the event's number, and `_Qxx` under the EC runs (where firmware
//!   notifies the lid, a button device, a battery).
//! - **The lid**: its `_LID`, 0 when closed.

use alloc::vec::Vec;

use oceans_acpi::{Fadt, space as gas_space};
use oceans_aml::eval::{Host, Machine, Value};
use oceans_aml::name::Path;

use crate::host::Firmware;

/// PM1 status and control bits.
const PWRBTN_STS: u64 = 1 << 8;
const PWRBTN_EN: u64 = 1 << 8;
const SCI_EN: u64 = 1 << 0;
/// The EC's status: an event waits.
const SCI_EVT: u64 = 1 << 5;
const QUERY_EC: u64 = 0x84;

/// The power button's status registers: an I/O port and its width.
pub struct Pm1 {
    status: Vec<(u16, u8)>,
}

impl Pm1 {
    /// From the FADT: each PM1 event block's first half is its status
    /// register, its second half the enable register, where the button's
    /// bit is set (some chipsets, and QEMU, set its status only then; the
    /// SCI it raises stays masked). `None` without a fixed power button.
    pub fn new(fadt: &Fadt, firmware: &mut Firmware) -> Option<Pm1> {
        if !fadt.fixed_power_button {
            return None;
        }
        let status: Vec<(u16, u8)> = [fadt.pm1a_event, fadt.pm1b_event]
            .into_iter()
            .flatten()
            .filter(|block| block.space == gas_space::SYSTEM_IO)
            .filter_map(|block| {
                let port = u16::try_from(block.address).ok()?;
                let width = (block.bit_width / 16).clamp(1, 4);
                Some((port, width))
            })
            .collect();
        for &(port, width) in &status {
            let enable = u64::from(port) + u64::from(width);
            // A press from before is not one now.
            let _ = firmware.write(gas_space::SYSTEM_IO, port.into(), width, PWRBTN_STS);
            if let Ok(now) = firmware.read(gas_space::SYSTEM_IO, enable, width) {
                let _ = firmware.write(gas_space::SYSTEM_IO, enable, width, now | PWRBTN_EN);
            }
        }
        (!status.is_empty()).then_some(Pm1 { status })
    }

    /// Whether the power button was pressed since last asked (its status
    /// bit cleared).
    pub fn pressed(&self, firmware: &mut Firmware) -> bool {
        let mut pressed = false;
        for &(port, width) in &self.status {
            let Ok(status) = firmware.read(gas_space::SYSTEM_IO, port.into(), width) else {
                continue;
            };
            if status & PWRBTN_STS != 0 {
                // Write-one-to-clear: only this bit.
                let _ = firmware.write(gas_space::SYSTEM_IO, port.into(), width, PWRBTN_STS);
                pressed = true;
            }
        }
        pressed
    }
}

/// Asks the firmware to hand power events to the system (ACPI mode), if
/// it still handles them itself: `acpi_enable` to the SMI command port,
/// then waits for SCI_EN. `true` if in ACPI mode.
pub fn enable_acpi_mode(firmware: &mut Firmware, fadt: &Fadt) -> bool {
    let Some(control) = fadt
        .pm1a_control
        .filter(|c| c.space == gas_space::SYSTEM_IO)
    else {
        return fadt.hardware_reduced;
    };
    let in_acpi = |firmware: &mut Firmware| {
        firmware
            .read(gas_space::SYSTEM_IO, control.address, 2)
            .is_ok_and(|v| v & SCI_EN != 0)
    };
    if in_acpi(firmware) {
        return true;
    }
    if fadt.smi_command == 0 || fadt.acpi_enable == 0 {
        return false;
    }
    let _ = firmware.write(
        gas_space::SYSTEM_IO,
        u64::from(fadt.smi_command),
        1,
        u64::from(fadt.acpi_enable),
    );
    for _ in 0..300 {
        if in_acpi(firmware) {
            return true;
        }
        oceans_rt::sleep_ms(10);
    }
    false
}

/// The EC's pending event, if any: its query number (0 is none).
pub fn ec_event(machine: &mut Machine<Firmware>) -> Option<u8> {
    let firmware = machine.host();
    let ec = firmware.ec?;
    let status = firmware.read_port(ec.command)?;
    if status & SCI_EVT == 0 {
        return None;
    }
    firmware.ec_command(QUERY_EC).filter(|&q| q != 0)
}

/// Runs the EC's handler for query `number` (`_Qxx`, hex digits upper
/// case), if it has one. `true` if it ran.
pub fn run_query(machine: &mut Machine<Firmware>, ec: &Path, number: u8) -> bool {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let name = [
        b'_',
        b'Q',
        HEX[usize::from(number >> 4)],
        HEX[usize::from(number & 0xf)],
    ];
    let method = ec.child(name);
    machine.exists(&method) && machine.evaluate(&method, Vec::new()).is_ok()
}

/// A lid's state: `Some(true)` open.
pub fn lid_open(machine: &mut Machine<Firmware>, lid: &Path) -> Option<bool> {
    match machine.evaluate(&lid.child(*b"_LID"), Vec::new()).ok()? {
        Value::Integer(v) => Some(v != 0),
        _ => None,
    }
}
