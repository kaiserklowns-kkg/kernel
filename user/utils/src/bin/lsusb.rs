//! `lsusb`: the USB devices the xHCI driver found (needs `use:usb`,
//! ADR-0032).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_rt::Start;
use oceans_usb::Speed;
use oceans_usb::service::{self, Record};
use utils::{EXIT_FAILED, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:usb\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let usb = match require(&mut out, &directory, "lsusb", "use", "usb", "use:usb") {
        Ok(usb) => usb,
        Err(code) => return code,
    };
    let mut reply = [0u8; service::MAX_RECORDS * service::RECORD_SIZE];
    let got = match oceans_rt::ipc_call_msg(usb, service::LIST, &[], &[], &mut reply, &mut []) {
        Ok(got) if got.label == service::OK => got,
        _ => {
            let _ = writeln!(out, "lsusb: the USB service did not answer");
            return EXIT_FAILED;
        }
    };
    let records = reply[..got.data_len]
        .as_chunks::<{ service::RECORD_SIZE }>()
        .0
        .iter()
        .filter_map(|bytes| Record::decode(bytes));
    let mut any = false;
    for record in records {
        any = true;
        let speed = Speed::from_id(record.speed).map_or("?", Speed::name);
        let _ = writeln!(
            out,
            "port {}: {:04x}:{:04x} {} ({speed}) {}",
            record.port,
            record.vendor,
            record.product,
            record.name(),
            record.kind.describe()
        );
    }
    if !any {
        let _ = writeln!(out, "no USB devices");
    }
    0
}
