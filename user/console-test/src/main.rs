//! Smoke test for console input (ADR-0017). Asks for a line on the kernel
//! log (which `cargo xtask smoke` watches), reads raw console bytes up to
//! Enter, echoing them like a terminal would, and exits 0 if the line is
//! the one the test harness types. Manifest grants: `log`, `console`.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_rt::{Buffer, Start};

oceans_rt::entry!(main);

/// What `cargo xtask smoke` types (kept in sync with tools/xtask).
const EXPECTED: &[u8] = b"hello oceans";

fn main(start: Start) -> i64 {
    let (Some(&log), Some(&console)) = (start.handles.first(), start.handles.get(1)) else {
        return 1;
    };
    let _ = oceans_rt::debug_write(log, "console-test: waiting for input");

    let mut line = [0u8; 64];
    let mut len: usize = 0;
    let mut chunk = [0u8; 16];
    'read: loop {
        let Ok(count) = oceans_rt::console_read(console, &mut chunk) else {
            return 2;
        };
        for &byte in &chunk[..count] {
            match byte {
                b'\r' | b'\n' => break 'read,
                0x7f | 0x08 => len = len.saturating_sub(1), // backspace
                _ if len < line.len() => {
                    line[len] = byte;
                    len += 1;
                    let _ = oceans_rt::console_write(console, &[byte]);
                }
                _ => {}
            }
        }
    }
    let _ = oceans_rt::console_write(console, b"\r\n");

    let mut report = Buffer::<128>::new();
    let _ = write!(
        report,
        "console-test: received \"{}\"",
        core::str::from_utf8(&line[..len]).unwrap_or("<not UTF-8>")
    );
    let _ = oceans_rt::debug_write(log, report.as_str());
    if &line[..len] == EXPECTED { 0 } else { 3 }
}
