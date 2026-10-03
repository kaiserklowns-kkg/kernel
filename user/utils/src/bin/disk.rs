//! `disk`: inspect and change a block device through the block service
//! (needs `use:block`, or another block service such as `use:usbdisk`).
//!
//! ```text
//! disk info                  size and sector count
//! disk read SECTOR           the sector as text (up to the first NUL)
//! disk dump SECTOR           the sector in hex
//! disk write SECTOR TEXT...  TEXT, zero-padded, as the whole sector
//! ```

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_block_proto::{Disk, SECTOR_SIZE, info};
use oceans_rt::{Out, Start};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:block\n");
oceans_rt::entry!(main);

const USAGE: &str = "usage: disk info | read SECTOR | dump SECTOR | write SECTOR TEXT...";

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    // `block`, or whichever block service was granted (e.g. `use:usbdisk`).
    let granted = directory
        .find("use", "block")
        .or_else(|| directory.find_kind("use"));
    let block = match granted {
        Some(block) => block,
        None => match require(&mut out, &directory, "disk", "use", "block", "use:block") {
            Ok(block) => block,
            Err(code) => return code,
        },
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let command = words.next().unwrap_or("");
    if command == "info" {
        return match info(block) {
            Ok(info) => {
                let _ = writeln!(
                    out,
                    "disk: {} sectors of {} bytes ({} MiB){}",
                    info.sectors,
                    info.sector_size,
                    info.bytes() >> 20,
                    if info.read_only() { ", read-only" } else { "" }
                );
                0
            }
            Err(error) => fail(&mut out, error.message()),
        };
    }
    let Some(sector) = words.next().and_then(|w| w.parse::<u64>().ok()) else {
        let _ = writeln!(out, "{USAGE}");
        return EXIT_USAGE;
    };
    let mut disk = match Disk::open(block, SECTOR_SIZE) {
        Ok(disk) => disk,
        Err(error) => return fail(&mut out, error.message()),
    };
    match command {
        "read" | "dump" => {
            if let Err(error) = disk.read(sector, 1, 0) {
                return fail(&mut out, error.message());
            }
            let bytes = &disk.buffer()[..SECTOR_SIZE];
            if command == "dump" {
                dump(&mut out, bytes);
            } else {
                let text = &bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(SECTOR_SIZE)];
                if text.is_empty() {
                    let _ = writeln!(out, "disk: sector {sector} holds no text");
                }
                for line in text.split(|&b| b == b'\n') {
                    for &byte in line {
                        let shown = if byte.is_ascii_graphic() || byte == b' ' {
                            byte
                        } else {
                            b'.'
                        };
                        let _ = out.write_char(char::from(shown));
                    }
                    let _ = writeln!(out);
                }
            }
            0
        }
        "write" => {
            let buffer = disk.buffer();
            buffer[..SECTOR_SIZE].fill(0);
            let mut len = 0;
            for (i, word) in words.enumerate() {
                let piece = if i > 0 { &b" "[..] } else { &[][..] }
                    .iter()
                    .chain(word.as_bytes())
                    .copied();
                for byte in piece {
                    if len == SECTOR_SIZE {
                        break;
                    }
                    buffer[len] = byte;
                    len += 1;
                }
            }
            match disk.write(sector, 1, 0).and_then(|()| disk.flush()) {
                Ok(()) => {
                    let _ = writeln!(out, "disk: wrote sector {sector}");
                    0
                }
                Err(error) => fail(&mut out, error.message()),
            }
        }
        _ => {
            let _ = writeln!(out, "{USAGE}");
            EXIT_USAGE
        }
    }
}

fn fail(out: &mut Out, problem: &str) -> i64 {
    let _ = writeln!(out, "disk: {problem}");
    EXIT_FAILED
}

fn dump(out: &mut Out, bytes: &[u8]) {
    for (row, line) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:04x} ", row * 16);
        for byte in line {
            let _ = write!(out, " {byte:02x}");
        }
        let _ = write!(out, "  |");
        for &byte in line {
            let shown = if byte.is_ascii_graphic() || byte == b' ' {
                byte
            } else {
                b'.'
            };
            let _ = out.write_char(char::from(shown));
        }
        let _ = writeln!(out, "|");
    }
}
