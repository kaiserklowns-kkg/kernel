//! `uptime`: time since the kernel's timer started (needs `sysinfo`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_abi::sysinfo::{self, UptimeInfo};
use oceans_rt::Start;
use utils::{EXIT_FAILED, Utility};

oceans_rt::manifest!(b"grant out\ngrant sysinfo\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let Utility { mut out, sysinfo } = match Utility::start("uptime", &start) {
        Ok(utility) => utility,
        Err(code) => return code,
    };
    let mut buffer = [0u8; UptimeInfo::SIZE];
    let info = oceans_rt::system_info(sysinfo, sysinfo::UPTIME, &mut buffer)
        .ok()
        .and_then(|len| UptimeInfo::decode(&buffer[..len]));
    let Some(info) = info else {
        let _ = writeln!(out, "uptime: cannot read the uptime");
        return EXIT_FAILED;
    };
    let ms = info.milliseconds();
    let _ = writeln!(out, "up {}.{:02} seconds", ms / 1000, (ms % 1000) / 10);
    0
}
