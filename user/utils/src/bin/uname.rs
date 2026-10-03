//! `uname`: kernel name, version, architecture and ABI (needs `sysinfo`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_abi::sysinfo::{self, KernelInfo};
use oceans_rt::Start;
use utils::{EXIT_FAILED, Utility};

oceans_rt::manifest!(b"grant out\ngrant sysinfo\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let Utility { mut out, sysinfo } = match Utility::start("uname", &start) {
        Ok(utility) => utility,
        Err(code) => return code,
    };
    let mut buffer = [0u8; KernelInfo::SIZE];
    let info = oceans_rt::system_info(sysinfo, sysinfo::KERNEL, &mut buffer)
        .ok()
        .and_then(|len| KernelInfo::decode(&buffer[..len]));
    let Some(info) = info else {
        let _ = writeln!(out, "uname: cannot read kernel information");
        return EXIT_FAILED;
    };
    let _ = writeln!(
        out,
        "Oceans {} {} (ABI {})",
        sysinfo::text(&info.version),
        sysinfo::text(&info.arch),
        info.abi_version
    );
    0
}
