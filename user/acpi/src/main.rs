//! acpi: the firmware's ACPI definition blocks, read (ADR-0148).
//!
//! The DSDT and every SSDT, copied by the kernel (`FIRMWARE_TABLE`), are
//! checked (signature, length, checksum) and read by `oceans-aml` into a
//! namespace. The service says what it found: how many objects, devices
//! and methods, which block could not be read and why, and the devices the
//! next steps need (the battery, the AC adapter, the lid, the buttons, the
//! embedded controller). Running methods comes next.
//!
//! The AML runs here, in a service, not in the kernel: firmware's code is
//! untrusted, and a fault in reading it ends this service, nothing else.
//!
//! Manifest grants: `log`, `firmware`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use core::fmt::Write;

use oceans_aml::Namespace;
use oceans_rt::{Buffer, Directory, Handle, Start};

oceans_rt::entry!(main);

/// The most blocks read (a PC has a DSDT and up to a few dozen SSDTs).
const MAX_BLOCKS: usize = 64;
const HEADER: usize = 36;

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
    let mut namespace = Namespace::new();
    let (mut read, mut failed) = (0, 0);
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
        return 0;
    }
    let (mut devices, mut methods) = (0, 0);
    for (_, object) in namespace.iter() {
        match object {
            oceans_aml::Object::Device => devices += 1,
            oceans_aml::Object::Method { .. } => methods += 1,
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
    for (path, id) in namespace.devices() {
        let Some(id) = id else {
            continue;
        };
        if let Some((_, what)) = NOTABLE.iter().find(|(known, _)| *known == id) {
            say(log, format_args!("acpi: {what} at {path} ({id})"));
        }
    }
    0
}
