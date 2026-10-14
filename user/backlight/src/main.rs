//! backlight: the screen's brightness (ADR-0147), for Control Center.
//!
//! A laptop's panel is lit by a PWM signal. On Intel graphics from Haswell
//! to Kaby Lake (8 and 9 series to 200 series PCHs) the PCH generates it,
//! and two registers in the graphics device's first BAR hold it:
//!
//! - `BLC_PWM_PCH_CTL1` (`0xc8250`): bit 31 enables it;
//! - `BLC_PWM_PCH_CTL2` (`0xc8254`): the period in bits 31:16, the duty
//!   cycle (how long it is on) in bits 15:0.
//!
//! The firmware has set both by the time Oceans starts. This service reads
//! them, offers the duty as a percentage, and writes only the duty, never
//! below [`MIN_PERCENT`]. Elsewhere (other graphics, newer PCHs, a desktop
//! screen, QEMU) it says so and answers that brightness is not available.
//!
//! Manifest grants: `log`, `provide = backlight` (first: the name exists
//! even without a display device, so the desktop always starts),
//! `grant = device-class:030000`.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_display_proto::proto::backlight::{GET, MIN_PERCENT, SET};
use oceans_rt::{Buffer, Directory, Error, Handle, Start, prot};

oceans_rt::entry!(main);

const CTL1: usize = 0xc8250;
const CTL2: usize = 0xc8254;
const ENABLED: u32 = 1 << 31;
/// The first byte past the registers used.
const SPAN: usize = CTL2 + 4;

/// Reply labels.
const OK: u64 = 0;
const UNAVAILABLE: u64 = 1;
const BAD_REQUEST: u64 = 2;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// Whether graphics device `id` has the PCH PWM at these registers:
/// Haswell (04xx, 0Axx, 0Dxx), Broadwell (16xx), Skylake (19xx) and Kaby
/// Lake (59xx). Coffee Lake and later (300 series PCHs on) moved them.
fn supported(id: u16) -> bool {
    matches!(id >> 8, 0x04 | 0x0a | 0x0d | 0x16 | 0x19 | 0x59)
}

/// The PWM's registers, mapped.
struct Pwm {
    base: *mut u8,
    /// The period, from the firmware.
    period: u32,
}

impl Pwm {
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: `offset` + 4 is within the mapped BAR (checked at map),
        // aligned, and device registers are read volatile.
        unsafe { core::ptr::read_volatile(self.base.add(offset).cast::<u32>()) }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: as `read`.
        unsafe { core::ptr::write_volatile(self.base.add(offset).cast::<u32>(), value) }
    }

    fn percent(&self) -> u8 {
        let duty = self.read(CTL2) & 0xffff;
        (u64::from(duty) * 100 / u64::from(self.period.max(1))).min(100) as u8
    }

    fn set(&self, percent: u8) {
        let percent = u32::from(percent.clamp(MIN_PERCENT, 100));
        let duty = (self.period * percent / 100).max(1);
        self.write(CTL2, (self.period << 16) | duty);
    }
}

/// The graphics device's PWM, if it has one we know and the firmware left
/// it on.
fn open(log: Handle, device: Handle) -> Result<Pwm, ()> {
    let vendor = oceans_rt::device_config_read(device, 0, 2).map_err(drop)? as u16;
    let id = oceans_rt::device_config_read(device, 2, 2).map_err(drop)? as u16;
    if vendor != 0x8086 || !supported(id) {
        say(
            log,
            format_args!("backlight: none on this display ({vendor:04x}:{id:04x})"),
        );
        return Err(());
    }
    let (memory, size) = oceans_rt::device_bar(device, 0).map_err(drop)?;
    let mapped = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
    let _ = oceans_rt::close(memory);
    let base = mapped.map_err(drop)?;
    if (size as usize) < SPAN {
        say(log, format_args!("backlight: the registers are not there"));
        return Err(());
    }
    let pwm = Pwm { base, period: 0 };
    let (ctl1, ctl2) = (pwm.read(CTL1), pwm.read(CTL2));
    let period = ctl2 >> 16;
    if ctl1 & ENABLED == 0 || period == 0 {
        say(
            log,
            format_args!("backlight: the PWM is off (8086:{id:04x}, {ctl1:#x} {ctl2:#x})"),
        );
        return Err(());
    }
    let pwm = Pwm { base, period };
    say(
        log,
        format_args!(
            "backlight: Intel 8086:{id:04x}, PWM period {period}, at {}%",
            pwm.percent()
        ),
    );
    Ok(pwm)
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(server)) = (
        directory.find("log", "log"),
        directory.find("provide", "backlight"),
    ) else {
        return 3;
    };
    let pwm = directory
        .find_kind("device")
        .ok_or(())
        .and_then(|device| open(log, device))
        .ok();
    let mut request = [0u8; 16];
    loop {
        let (len, label) = match oceans_rt::ipc_receive(server, &mut request) {
            Ok(got) => got,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        let replied = match (label, &request[..len], &pwm) {
            (GET, _, Some(pwm)) => oceans_rt::ipc_reply(OK, &[pwm.percent(), 1]),
            (GET, _, None) => oceans_rt::ipc_reply(OK, &[0, 0]),
            (SET, &[percent], Some(pwm)) if percent <= 100 => {
                pwm.set(percent);
                oceans_rt::ipc_reply(OK, &[pwm.percent(), 1])
            }
            (SET, _, None) => oceans_rt::ipc_reply(UNAVAILABLE, &[]),
            _ => oceans_rt::ipc_reply(BAD_REQUEST, &[]),
        };
        if replied.is_err() {
            return 5;
        }
    }
}
