//! iwm: the driver for the Intel Wireless 8260 (ADR-0129). It brings the
//! chip up with Intel's firmware, scans, joins the Wi-Fi network set in
//! `/system/wifi` (WPA2-Personal or open), and serves the stack as its
//! network device.
//!
//! It stands between the stack and the wired NIC, if the machine has one:
//! the stack keeps one device and one address (the wired NIC's, used on
//! the air too), and its frames go by Wi-Fi once joined, by wire until
//! then. A machine without Wi-Fi never starts this service, and its stack
//! talks to the wired driver directly.
//!
//! It runs on real hardware only (no emulator has this chip), so every
//! step says what it did, and a failure says where, with the registers
//! that tell why: the log is how it is debugged.
//!
//! The steps (the facts in `oceans-iwm`, from OpenBSD's iwm(4)):
//! 1. Wake the chip: ready, out of reset, its clocks running.
//! 2. The INIT image: its sections DMA'd into the chip through the service
//!    channel, CPU1 then CPU2; ALIVE; the scheduler and the command queue;
//!    the NVM read (MAC address, channels, antennas); calibration.
//! 3. The REGULAR image the same way, its paged code handed over, then the
//!    first commands (antennas, calibration results, PHY configuration).
//! 4. Client mode: the auxiliary station and its queue, PHY contexts,
//!    regulatory (LAR), the scan configuration, the data queues.
//! 5. A scan: beacons and probe responses arrive as RX_PHY + RX_MPDU
//!    pairs, read by `oceans-wifi`.
//! 6. Joining (`link`): the MAC context, binding and station; open-system
//!    authentication and association; the WPA2 handshakes (`oceans-wpa`);
//!    then data frames, encrypted in software.
//!
//! Interrupts are polled (the chip's cause register, every 10 ms).

#![no_std]
#![no_main]

extern crate alloc;

mod chip;
mod link;
mod wired;

use oceans_fs_proto::Node;
use oceans_iwm::fw::Firmware;
use oceans_nic::Events;
use oceans_rt::{Directory, Handle, Start, prot};

use chip::Chip;
use link::Driver;
use wired::Wired;

oceans_rt::entry!(main);

const NAME: &str = "iwm";
const FIRMWARE: &str = "iwlwifi-8000C-36.ucode";
/// The notification bit the wired driver signals.
const WIRED: u64 = 2;

const EXIT_BAD_START: i64 = 2;
const EXIT_FAILED: i64 = 3;

pub fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    oceans_nic::say(log, NAME, args);
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(device), Some(module), Some(server)) = (
        directory.find("log", "log"),
        directory.find_kind("device"),
        directory.find("module", FIRMWARE),
        directory.find_kind("provide"),
    ) else {
        return EXIT_BAD_START;
    };
    let Ok(events) = oceans_rt::notification_create() else {
        return EXIT_FAILED;
    };
    if oceans_rt::endpoint_bind(server, events).is_err() {
        return EXIT_FAILED;
    }
    let wired = directory.find("use", "wired").and_then(|driver| {
        match Wired::connect(driver, events, WIRED) {
            Ok(wired) => {
                say(
                    log,
                    format_args!(
                        "wired NIC {}: its frames pass through until Wi-Fi joins",
                        oceans_nic::ShowMac(wired.mac)
                    ),
                );
                Some(wired)
            }
            Err(why) => {
                say(log, format_args!("no wired NIC ({why})"));
                None
            }
        }
    });
    let storage = directory.find("use", "storage").map(Node);
    if storage.is_none() {
        say(
            log,
            format_args!("no storage granted: no network can be set"),
        );
    }

    let chip = match bring_up(log, device, module, wired.as_ref().map(|w| w.mac)) {
        Ok(chip) => Some(chip),
        Err(problem) => {
            say(log, format_args!("Wi-Fi is off: {problem}"));
            None
        }
    };
    let Some(mac) = wired
        .as_ref()
        .map(|w| w.mac)
        .or_else(|| chip.as_ref().and_then(|c| c.our_address().ok()))
    else {
        say(
            log,
            format_args!("neither Wi-Fi nor a wired NIC: nothing to serve"),
        );
        return EXIT_FAILED;
    };
    let mut driver = Driver::new(log, mac, chip, wired, storage);
    driver.start();
    oceans_nic::serve(
        &mut driver,
        NAME,
        log,
        server,
        Events {
            handle: events,
            interrupts: false,
        },
    )
}

/// The chip up to its first scan; its networks logged.
fn bring_up(
    log: Handle,
    device: Handle,
    module: Handle,
    address: Option<[u8; 6]>,
) -> chip::Result<Chip<'static>> {
    let file = module_bytes(module)
        .map_err(|()| alloc::format!("the firmware {FIRMWARE} cannot be read"))?;
    let firmware =
        Firmware::parse(file).map_err(|e| alloc::format!("{FIRMWARE} is not usable: {e:?}"))?;
    // Kept for the driver's life.
    let firmware: &'static Firmware<'static> =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(firmware));
    let (major, minor, _) = firmware.version;
    say(
        log,
        format_args!(
            "firmware {major}.{minor:08x}: INIT {}+{} sections, REGULAR {}+{}, paging {} KiB",
            firmware.init.cpu1.len(),
            firmware.init.cpu2.len(),
            firmware.regular.cpu1.len(),
            firmware.regular.cpu2.len(),
            firmware.paging_size / 1024
        ),
    );
    let mut chip = Chip::new(log, device, firmware)?;
    chip.address = address;
    let result = chip.bring_up().and_then(|()| chip.scan(None));
    if let Err(problem) = result {
        chip.report();
        return Err(problem);
    }
    chip.show_networks();
    Ok(chip)
}

/// The firmware module's bytes (mapped for the driver's life).
fn module_bytes(module: Handle) -> Result<&'static [u8], ()> {
    let size = oceans_rt::memory_size(module).map_err(drop)? as usize;
    let base = oceans_rt::memory_map(module, 0, prot::READ).map_err(drop)?;
    // SAFETY: `base` maps `size` bytes of the module, read-only, never
    // unmapped while the driver runs.
    Ok(unsafe { core::slice::from_raw_parts(base, size) })
}
