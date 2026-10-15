//! `update`: system updates (ADR-0071), from the shell.
//!
//! ```text
//! update status              the running release and the boot entries
//! update apply PATH          verify an update and install it to start next
//! update previous            the other slot's release starts first (ADR-0122)
//! ```
//!
//! The work is `oceans-update`'s, shared with the `updater` service that
//! Settings uses (ADR-0158).
//!
//! Needs `use:fs` (`run update out use:fs -- apply /usb/update.opk`); with
//! Secure Boot also the stick itself, `use:usbdisk`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write;

use oceans_fs_proto::Node;
use oceans_rt::Start;
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\n");
oceans_rt::entry!(main);

const USAGE: &str =
    "usage: update status | apply PATH | previous   (run update out use:fs [use:usbdisk] -- ...)";

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let root = match require(&mut out, &directory, "update", "use", "fs", "use:fs") {
        Ok(fs) => Node(fs),
        Err(code) => return code,
    };
    oceans_update::detect(&root);
    let stick = directory.find("use", "usbdisk");
    let args = directory.args();
    let words: Vec<&str> = args.split_whitespace().collect();
    let result = match words.as_slice() {
        ["status"] | [] => oceans_update::status(&mut out, &root, stick),
        ["apply", path] => oceans_update::apply(&mut out, &root, stick, path),
        ["previous"] => oceans_update::previous(&mut out, &root, stick),
        _ => {
            let _ = writeln!(out, "{USAGE}");
            return EXIT_USAGE;
        }
    };
    match result {
        Ok(()) => 0,
        Err(why) => {
            let _ = writeln!(out, "update: {why}");
            EXIT_FAILED
        }
    }
}
