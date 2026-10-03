//! Calls the echo service once and logs the answer. Manifest grants:
//! `log`, `use = echo`.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_rt::{Buffer, Start};

oceans_rt::entry!(main);

const GREETING: &[u8] = b"hello, oceans";

fn main(start: Start) -> i64 {
    let (Some(&log), Some(&echo)) = (start.handles.first(), start.handles.get(1)) else {
        return 1;
    };
    let mut reply = [0u8; 64];
    let Ok((len, _)) = oceans_rt::ipc_call(echo, 1, GREETING, &mut reply) else {
        let _ = oceans_rt::debug_write(log, "hello: echo service unreachable");
        return 2;
    };
    let mut expected = [0u8; GREETING.len()];
    expected.copy_from_slice(GREETING);
    expected.make_ascii_uppercase();
    if reply[..len] != expected {
        return 3;
    }
    let mut line = Buffer::<128>::new();
    let _ = write!(
        line,
        "hello: echo replied \"{}\"",
        core::str::from_utf8(&reply[..len]).unwrap_or("?")
    );
    let _ = oceans_rt::debug_write(log, line.as_str());
    0
}
