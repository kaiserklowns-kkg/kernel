//! `lspci`: the PCI functions the kernel found (needs `devices`, the
//! read-only device list).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_abi::device::DeviceRecord;
use oceans_rt::Start;
use utils::{EXIT_FAILED, console, require};

oceans_rt::manifest!(b"grant out\ngrant devices\n");
oceans_rt::entry!(main);

const MAX_FUNCTIONS: usize = 128;

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let bus = match require(
        &mut out, &directory, "lspci", "devices", "devices", "devices",
    ) {
        Ok(bus) => bus,
        Err(code) => return code,
    };
    let mut buffer = [0u8; MAX_FUNCTIONS * DeviceRecord::SIZE];
    let Ok(len) = oceans_rt::device_list(bus, &mut buffer) else {
        let _ = writeln!(out, "lspci: cannot read the device list");
        return EXIT_FAILED;
    };
    let _ = writeln!(out, "ADDRESS       ID         CLASS");
    for record in buffer[..len]
        .as_chunks::<{ DeviceRecord::SIZE }>()
        .0
        .iter()
        .filter_map(|bytes| DeviceRecord::decode(bytes))
    {
        let _ = write!(
            out,
            "{:04x}:{:02x}:{:02x}.{}  {:04x}:{:04x}  ",
            record.segment, record.bus, record.slot, record.function, record.vendor, record.device
        );
        match class_name(record.class, record.subclass) {
            Some(name) => {
                let _ = write!(out, "{name}");
            }
            None => {
                let _ = write!(out, "class {:02x}.{:02x}", record.class, record.subclass);
            }
        }
        let _ = writeln!(
            out,
            "{}",
            if record.open {
                "  (driver attached)"
            } else {
                ""
            }
        );
    }
    0
}

fn class_name(class: u8, subclass: u8) -> Option<&'static str> {
    Some(match (class, subclass) {
        (0x01, 0x06) => "SATA controller",
        (0x01, 0x08) => "NVMe controller",
        (0x01, _) => "mass storage",
        (0x02, _) => "network",
        (0x03, _) => "display",
        (0x04, _) => "multimedia",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "bridge",
        (0x0c, 0x03) => "USB controller",
        (0x0c, 0x05) => "SMBus",
        (0x0c, _) => "serial bus",
        _ => return None,
    })
}
