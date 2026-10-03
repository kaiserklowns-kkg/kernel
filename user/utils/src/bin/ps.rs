//! `ps`: list processes (needs `sysinfo`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_abi::sysinfo::{self, ProcessRecord};
use oceans_rt::Start;
use utils::{EXIT_FAILED, Size, Utility};

oceans_rt::manifest!(b"grant out\ngrant sysinfo\n");
oceans_rt::entry!(main);

/// Room for 256 processes.
const BUFFER: usize = 256 * ProcessRecord::SIZE;

fn main(start: Start) -> i64 {
    let Utility { mut out, sysinfo } = match Utility::start("ps", &start) {
        Ok(utility) => utility,
        Err(code) => return code,
    };
    let mut buffer = [0u8; BUFFER];
    let Ok(len) = oceans_rt::system_info(sysinfo, sysinfo::PROCESSES, &mut buffer) else {
        let _ = writeln!(out, "ps: cannot read the process list");
        return EXIT_FAILED;
    };
    let _ = writeln!(out, "  PID  PPID  MEMORY  STATE        NAME");
    for record in buffer[..len].as_chunks::<{ ProcessRecord::SIZE }>().0 {
        let Some(process) = ProcessRecord::decode(record) else {
            continue;
        };
        let mut state = oceans_rt::Buffer::<16>::new();
        match process.exit() {
            None => {
                let _ = state.write_str("running");
            }
            Some(code) => {
                let _ = write!(state, "exited {code}");
            }
        }
        let _ = writeln!(
            out,
            "{:>5} {:>5} {:>7}  {:<12} {}",
            process.id,
            process.parent,
            Size(process.memory),
            state.as_str(),
            sysinfo::text(&process.name)
        );
    }
    0
}
