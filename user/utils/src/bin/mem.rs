//! `mem`: physical memory and kernel heap usage (needs `sysinfo`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_abi::sysinfo::{self, MemoryInfo};
use oceans_rt::Start;
use utils::{EXIT_FAILED, Size, Utility};

oceans_rt::manifest!(b"grant out\ngrant sysinfo\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let Utility { mut out, sysinfo } = match Utility::start("mem", &start) {
        Ok(utility) => utility,
        Err(code) => return code,
    };
    let mut buffer = [0u8; MemoryInfo::SIZE];
    let info = oceans_rt::system_info(sysinfo, sysinfo::MEMORY, &mut buffer)
        .ok()
        .and_then(|len| MemoryInfo::decode(&buffer[..len]));
    let Some(info) = info else {
        let _ = writeln!(out, "mem: cannot read memory information");
        return EXIT_FAILED;
    };
    let mib = |frames: u64| frames * info.page_size / (1024 * 1024);
    let _ = writeln!(
        out,
        "memory: {} MiB free of {} MiB ({} of {} frames of {} KiB free)",
        mib(info.free_frames),
        mib(info.total_frames),
        info.free_frames,
        info.total_frames,
        info.page_size / 1024
    );
    let _ = writeln!(out, "kernel heap: {} in use", Size(info.kernel_heap));
    0
}
