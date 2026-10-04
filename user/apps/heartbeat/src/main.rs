//! Heartbeat: the example Oceans service (ADR-0049).
//!
//! A service has no terminal: it writes to the system log (`log log`). It
//! counts its starts in its storage and, to show Oceans Core's restart
//! policy, fails on purpose the very first time it runs; from then on it
//! keeps running, saying so once a minute.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_fs_proto::{Kind, Node, flags};
use oceans_rt::{Buffer, Directory, Handle, Start};

oceans_rt::entry!(main);

/// The exit code of the planned first failure.
const FIRST_RUN_FAILURE: i64 = 3;

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let Some(log) = directory.find("log", "log") else {
        return 2;
    };
    let runs = match directory.find("use", "storage") {
        Some(storage) => match count_run(&Node(storage)) {
            Ok(runs) => runs,
            Err(problem) => {
                say(log, format_args!("heartbeat: storage failed: {problem}"));
                return 4;
            }
        },
        None => {
            say(log, format_args!("heartbeat: no storage; nothing to count"));
            0
        }
    };
    if runs == 1 {
        say(log, format_args!("heartbeat: run 1, failing on purpose"));
        return FIRST_RUN_FAILURE;
    }
    say(log, format_args!("heartbeat: run {runs}, beating"));
    loop {
        oceans_rt::sleep_ms(60_000);
        say(log, format_args!("heartbeat: still beating"));
    }
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<128>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// Reads, increments and writes back the `runs` file.
fn count_run(storage: &Node) -> Result<u64, &'static str> {
    let (file, kind) = storage
        .open("runs", flags::CREATE_FILE | flags::WRITE)
        .map_err(|e| e.message())?;
    let result = (|| {
        if kind != Kind::File {
            return Err("runs is not a file");
        }
        let mut text = [0u8; 20];
        let len = file.read(0, &mut text).map_err(|e| e.message())?;
        let runs = core::str::from_utf8(&text[..len])
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        let mut line = Buffer::<24>::new();
        let _ = writeln!(line, "{runs}");
        file.truncate(0)
            .and_then(|()| file.write_all(0, line.as_bytes()))
            .and_then(|()| file.sync())
            .map_err(|e| e.message())?;
        Ok(runs)
    })();
    file.close();
    result
}
