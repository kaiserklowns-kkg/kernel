//! acpi: the firmware's AML, read and run (ADR-0148, ADR-0153, ADR-0160).
//!
//! The DSDT and every SSDT, copied by the kernel (`FIRMWARE_TABLE`), are
//! checked (signature, length, checksum) and read by `oceans-aml` into a
//! namespace. Then, as other systems do at start, the devices are
//! initialised (`\_SB._INI`, each present device's `_INI`), the embedded
//! controller is found and its regions connected (`_REG`), and, while
//! there is a battery, it is read every few seconds (`_BIX`/`_BIF`,
//! `_BST`, the adapter's `_PSR`) into `/system/battery/status`, which the
//! desktop shows (ADR-0152).
//!
//! It also watches what the user does to the machine (ADR-0160, `events`):
//! the power button, the lid and the embedded controller's events, into
//! `/system/battery/events`.
//!
//! The AML runs here, in a service, not in the kernel: firmware's code is
//! untrusted, and a fault in it ends this service, nothing else. What it
//! reaches goes through the kernel's firmware system calls, which keep the
//! system's own ports, RAM and devices out of its reach.
//!
//! Manifest grants: `log`, `firmware`, `storage:/system/battery as
//! battery`.

#![no_std]
#![no_main]

extern crate alloc;

mod events;
mod host;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_aml::eval::{Machine, RunError, Value, space};
use oceans_aml::name::Path;
use oceans_aml::{Namespace, Object, battery};
use oceans_rt::{Buffer, Directory, Handle, Start};
use oceans_window::battery::{Battery, Events, State};

oceans_rt::entry!(main);

/// The most blocks read (a PC has a DSDT and up to a few dozen SSDTs).
const MAX_BLOCKS: usize = 64;
const HEADER: usize = 36;
/// How often the battery is read.
const EVERY_MS: u64 = 5_000;
/// `Notify` values a battery or adapter sends when it changes (ACPI 6.5
/// §10.2.1): its status, its information.
const STATUS_CHANGED: u64 = 0x80;

/// Devices worth naming, by hardware ID (ACPI 6.5 §5.6.7, §10, §12).
const NOTABLE: [(&str, &str); 7] = [
    ("PNP0C0A", "battery"),
    ("ACPI0003", "AC adapter"),
    ("PNP0C0D", "lid"),
    ("PNP0C0C", "power button"),
    ("PNP0C0E", "sleep button"),
    ("PNP0C09", "embedded controller"),
    ("ACPI0008", "light sensor"),
];

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<240>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(firmware)) = (
        directory.find("log", "log"),
        directory.find("firmware", "firmware"),
    ) else {
        return 3;
    };
    let storage = directory.find("use", "battery").map(oceans_fs_proto::Node);
    let Some((namespace, int32)) = load(log, firmware) else {
        return 0;
    };
    let mut machine = Machine::new(namespace, host::Firmware::new(firmware));
    machine.set_int32(int32);

    // Devices by kind, as their constant or computed `_HID` says.
    let devices: Vec<Path> = machine
        .namespace()
        .iter()
        .filter(|(_, o)| matches!(o, Object::Device))
        .map(|(p, _)| p.clone())
        .collect();
    let mut found: Vec<(Path, &str)> = Vec::new();
    for device in &devices {
        if let Some(id) = machine.hardware_id(device)
            && let Some((_, what)) = NOTABLE.iter().find(|(known, _)| *known == id)
        {
            say(log, format_args!("acpi: {what} at {device} ({id})"));
            found.push((device.clone(), what));
        }
    }

    initialise(log, &mut machine, &devices);
    connect_ec(log, &mut machine, &found);

    // The power button and the lid (ADR-0160): the FADT says where the
    // fixed button is; the firmware hands its events over in ACPI mode.
    let fadt = fadt(firmware);
    let pm1 = fadt.as_ref().and_then(|fadt| {
        if !events::enable_acpi_mode(machine.host(), fadt) {
            say(
                log,
                format_args!("acpi: the firmware did not enter ACPI mode"),
            );
        }
        events::Pm1::new(fadt, machine.host())
    });
    let watched = Watched {
        batteries: kind(&found, "battery"),
        adapters: kind(&found, "AC adapter"),
        lids: kind(&found, "lid"),
        buttons: kind(&found, "power button"),
        ec: machine
            .host()
            .ec
            .and_then(|_| kind(&found, "embedded controller").into_iter().next()),
        pm1,
    };
    if watched.batteries.is_empty() {
        say(log, format_args!("acpi: no battery on this machine"));
    }
    say(
        log,
        format_args!(
            "acpi: watching: power button {}, lid {}, embedded controller {}",
            if watched.pm1.is_some() {
                "fixed"
            } else if watched.buttons.is_empty() {
                "none"
            } else {
                "device"
            },
            if watched.lids.is_empty() {
                "none"
            } else {
                "yes"
            },
            if watched.ec.is_some() { "yes" } else { "none" },
        ),
    );
    if watched.batteries.is_empty()
        && watched.lids.is_empty()
        && watched.pm1.is_none()
        && watched.ec.is_none()
    {
        return 0;
    }
    watch(log, &mut machine, &watched, storage.as_ref())
}

/// The FADT (ADR-0160), if the kernel has one.
fn fadt(firmware: Handle) -> Option<oceans_acpi::Fadt> {
    let index = oceans_rt::FIRMWARE_FADT;
    let length = oceans_rt::firmware_table(firmware, index, &mut []).ok()?;
    let mut table = vec![0u8; length];
    oceans_rt::firmware_table(firmware, index, &mut table).ok()?;
    oceans_acpi::Table::parse(&table)
        .and_then(oceans_acpi::Fadt::parse)
        .ok()
}

fn kind(found: &[(Path, &str)], what: &str) -> Vec<Path> {
    found
        .iter()
        .filter(|(_, w)| *w == what)
        .map(|(p, _)| p.clone())
        .collect()
}

/// Reads every definition block into a namespace; says what it found.
/// Also whether integers are 32 bits (the DSDT's revision below 2).
fn load(log: Handle, firmware: Handle) -> Option<(Namespace, bool)> {
    let mut namespace = Namespace::new();
    let (mut read, mut failed, mut int32) = (0, 0, false);
    for index in 0..MAX_BLOCKS {
        let Ok(length) = oceans_rt::firmware_table(firmware, index, &mut []) else {
            break;
        };
        let mut table = vec![0u8; length];
        if oceans_rt::firmware_table(firmware, index, &mut table) != Ok(length) {
            break;
        }
        let what = if index == 0 { "DSDT" } else { "SSDT" };
        let checked = (|| {
            if length < HEADER || (&table[..4] != b"DSDT" && &table[..4] != b"SSDT") {
                return Err(String::from("not a definition block"));
            }
            let declared = u32::from_le_bytes(table[4..8].try_into().expect("4 bytes")) as usize;
            if declared != length {
                return Err(String::from("its length does not match"));
            }
            if table.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) != 0 {
                // Firmware ships blocks with wrong checksums; the reader
                // checks everything it reads anyway.
                say(log, format_args!("acpi: {what} {index} has a bad checksum"));
            }
            if index == 0 {
                int32 = table[8] < 2;
            }
            let aml = &table[HEADER..];
            namespace.load(aml).map_err(|error| {
                // What is there, for a report (a photo of the screen).
                let at = error.offset().min(aml.len());
                let mut near = alloc::format!("{error:?}, there:");
                for b in &aml[at..(at + 12).min(aml.len())] {
                    let _ = write!(near, " {b:02x}");
                }
                near
            })
        })();
        match checked {
            Ok(_) => read += 1,
            Err(why) => {
                failed += 1;
                say(log, format_args!("acpi: {what} {index} not read: {why}"));
            }
        }
    }
    if read + failed == 0 {
        say(log, format_args!("acpi: no definition blocks"));
        return None;
    }
    let (mut devices, mut methods) = (0, 0);
    for (_, object) in namespace.iter() {
        match object {
            Object::Device => devices += 1,
            Object::Method { .. } => methods += 1,
            _ => {}
        }
    }
    say(
        log,
        format_args!(
            "acpi: {read} definition block(s) read, {failed} not: {} objects, {devices} devices, {methods} methods",
            namespace.len()
        ),
    );
    Some((namespace, int32))
}

/// `\_SB._INI`, then each present device's `_INI` (ACPI 6.5 §6.5.1), as
/// the firmware expects before it is used.
fn initialise(log: Handle, machine: &mut Machine<host::Firmware>, devices: &[Path]) {
    let (mut ran, mut failed) = (0, 0);
    let mut first: Option<(Path, RunError)> = None;
    let mut order = vec![Path::parse("\\_SB._INI").expect("a path")];
    order.extend(devices.iter().map(|d| d.child(*b"_INI")));
    for ini in order {
        if !machine.exists(&ini) {
            continue;
        }
        // A device that is not there is not initialised (bit 0 of _STA).
        let device = ini.parent();
        if device.depth() > 1 && matches!(machine.status(&device), Ok(sta) if sta & 1 == 0) {
            continue;
        }
        match machine.evaluate(&ini, Vec::new()) {
            Ok(_) => ran += 1,
            Err(error) => {
                failed += 1;
                first.get_or_insert((ini, error));
            }
        }
    }
    say(log, format_args!("acpi: _INI ran {ran}, failed {failed}"));
    if let Some((path, error)) = first {
        say(log, format_args!("acpi: {path}: {error:?}"));
    }
    // Each device's status and resources, which read only: the firmware's
    // code run once at start, so what the interpreter cannot run shows in
    // the log (and in the smoke tests, on QEMU's AML).
    let (mut ran, mut failed) = (0, 0);
    let mut first: Option<(Path, RunError)> = None;
    for device in devices {
        for name in [*b"_STA", *b"_CRS"] {
            let method = device.child(name);
            if !machine.exists(&method) {
                continue;
            }
            match machine.evaluate(&method, Vec::new()) {
                Ok(_) => ran += 1,
                Err(error) => {
                    failed += 1;
                    first.get_or_insert((method, error));
                }
            }
        }
    }
    say(
        log,
        format_args!("acpi: _STA and _CRS failed {failed} of {ran}"),
    );
    if let Some((path, error)) = first {
        say(log, format_args!("acpi: {path}: {error:?}"));
    }
}

/// The embedded controller: its ports from `_CRS`, then its regions
/// connected (`_REG`).
fn connect_ec(log: Handle, machine: &mut Machine<host::Firmware>, found: &[(Path, &str)]) {
    let Some((ec, _)) = found
        .iter()
        .find(|(_, what)| *what == "embedded controller")
    else {
        return;
    };
    let crs = ec.child(*b"_CRS");
    let ports = match machine.evaluate(&crs, Vec::new()) {
        Ok(Value::Buffer(resources)) => host::ec_ports(&resources),
        _ => None,
    };
    let Some(ports) = ports else {
        say(
            log,
            format_args!("acpi: embedded controller at {ec}: no ports in _CRS"),
        );
        return;
    };
    machine.host().ec = Some(ports);
    let (ran, error) = machine.connect(space::EMBEDDED_CONTROL);
    say(
        log,
        format_args!(
            "acpi: embedded controller at {ec}, ports {:#x}/{:#x}: _REG ran {ran}",
            ports.data, ports.command
        ),
    );
    if let Some(error) = error {
        say(log, format_args!("acpi: _REG: {error:?}"));
    }
}

/// What the service watches (ADR-0153, ADR-0160).
struct Watched {
    batteries: Vec<Path>,
    adapters: Vec<Path>,
    lids: Vec<Path>,
    buttons: Vec<Path>,
    ec: Option<Path>,
    pm1: Option<events::Pm1>,
}

/// How often the button, the EC and the lid are looked at; how often the
/// lid is read when nothing notifies.
const POLL_MS: u64 = 200;
const LID_EVERY_MS: u64 = 1_000;

/// Watches until the service is stopped: the power button and the EC's
/// events every 200 ms, the lid every second, the batteries every 5 s,
/// and each at once when the firmware notifies it.
fn watch(
    log: Handle,
    machine: &mut Machine<host::Firmware>,
    watched: &Watched,
    storage: Option<&oceans_fs_proto::Node>,
) -> i64 {
    let mut shown: Option<Battery> = None;
    let mut first = true;
    let mut said_error = false;
    let mut events = Events::default();
    // How things are at start, so that the desktop sees the first press
    // as one.
    write_events(storage, &events);
    let mut since_lid = LID_EVERY_MS;
    let mut since_battery = EVERY_MS;
    loop {
        let (mut lid_now, mut battery_now) = (false, false);
        // The fixed power button.
        if watched
            .pm1
            .as_ref()
            .is_some_and(|pm1| pm1.pressed(machine.host()))
        {
            events.button = events.button.wrapping_add(1);
            say(log, format_args!("acpi: power button"));
            write_events(storage, &events);
        }
        // The EC's events, a few at a time.
        if let Some(ec) = &watched.ec {
            for _ in 0..8 {
                let Some(query) = events::ec_event(machine) else {
                    break;
                };
                events::run_query(machine, ec, query);
            }
        }
        // What the firmware notified: a button device, the lid, a battery.
        let notified = core::mem::take(&mut machine.host().notified);
        for (object, value) in &notified {
            if watched.buttons.contains(object) && *value == STATUS_CHANGED {
                events.button = events.button.wrapping_add(1);
                say(log, format_args!("acpi: power button ({object})"));
                write_events(storage, &events);
            }
            lid_now |= watched.lids.contains(object);
            battery_now |= *value >= STATUS_CHANGED
                && (watched.batteries.contains(object) || watched.adapters.contains(object));
        }
        // The lid.
        if let Some(lid) = watched.lids.first()
            && (lid_now || since_lid >= LID_EVERY_MS)
        {
            since_lid = 0;
            let open = events::lid_open(machine, lid);
            if open.is_some() && open != events.lid_open {
                events.lid_open = open;
                let state = if open == Some(true) { "open" } else { "closed" };
                say(log, format_args!("acpi: lid {state}"));
                write_events(storage, &events);
            }
        }
        // The batteries.
        if !watched.batteries.is_empty() && (battery_now || since_battery >= EVERY_MS) {
            since_battery = 0;
            match read_batteries(machine, &watched.batteries, &watched.adapters) {
                Ok(now) => {
                    said_error = false;
                    if now != shown || first {
                        match now {
                            Some(b) => say(
                                log,
                                format_args!("acpi: battery {}%, {}", b.percent, b.describe()),
                            ),
                            None => say(log, format_args!("acpi: battery not present")),
                        }
                        write_status(storage, now.as_ref());
                        shown = now;
                        first = false;
                    }
                }
                Err(error) if !said_error => {
                    said_error = true;
                    say(log, format_args!("acpi: battery not read: {error:?}"));
                }
                Err(_) => {}
            }
        }
        oceans_rt::sleep_ms(POLL_MS);
        since_lid += POLL_MS;
        since_battery += POLL_MS;
    }
}

/// Writes what the user did (ADR-0160) for the desktop.
fn write_events(storage: Option<&oceans_fs_proto::Node>, events: &Events) {
    write_file(
        storage,
        oceans_window::battery::EVENTS,
        events.to_text().as_bytes(),
    );
}

fn write_file(storage: Option<&oceans_fs_proto::Node>, name: &str, bytes: &[u8]) {
    let Some(dir) = storage else {
        return;
    };
    let create = oceans_fs_proto::flags::CREATE_FILE | oceans_fs_proto::flags::WRITE;
    if let Ok((node, kind)) = dir.open(name, create) {
        if kind == oceans_fs_proto::Kind::File {
            let _ = node.truncate(0);
            let _ = node.write_all(0, bytes);
        }
        node.close();
    }
}

/// The batteries together: their charge summed, charging if any is.
fn read_batteries(
    machine: &mut Machine<host::Firmware>,
    batteries: &[Path],
    adapters: &[Path],
) -> Result<Option<Battery>, RunError> {
    let (mut remaining, mut full, mut rate) = (0u64, 0u64, 0u64);
    let (mut charging, mut discharging, mut any) = (false, false, false);
    for path in batteries {
        if !battery::present(machine, path)? {
            continue;
        }
        let info = battery::info(machine, path)?;
        let status = battery::status(machine, path)?;
        let Some(full_now) = info.last_full.or(info.design) else {
            continue;
        };
        any = true;
        remaining += status.remaining.unwrap_or(0).min(full_now);
        full += full_now;
        rate += status.rate.unwrap_or(0);
        charging |= status.charging;
        discharging |= status.discharging;
    }
    if !any || full == 0 {
        return Ok(None);
    }
    let on_power = adapters
        .iter()
        .any(|a| battery::on_power(machine, a).unwrap_or(false));
    let reading = battery::reading(
        &battery::Info {
            milliwatts: true,
            design: None,
            last_full: Some(full),
        },
        &battery::Status {
            discharging,
            charging,
            critical: false,
            rate: (rate > 0).then_some(rate),
            remaining: Some(remaining),
        },
    );
    Ok(reading.map(|r| Battery {
        percent: r.percent,
        state: if r.charging {
            State::Charging
        } else if r.discharging && !on_power {
            State::Discharging
        } else {
            State::Full
        },
        minutes: r.minutes,
    }))
}

/// Writes the status line the desktop reads (ADR-0152); no battery, no
/// file.
fn write_status(storage: Option<&oceans_fs_proto::Node>, battery: Option<&Battery>) {
    use oceans_window::battery::STATUS;
    let Some(dir) = storage else {
        return;
    };
    let Some(battery) = battery else {
        let _ = dir.remove(STATUS);
        return;
    };
    write_file(storage, STATUS, battery.to_text().as_bytes());
}
