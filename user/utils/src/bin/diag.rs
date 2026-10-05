//! `diag`: diagnostics from the log the kernel keeps (ADR-0070), without a
//! serial cable or a screen (master spec §42).
//!
//! ```text
//! diag log [LINES]   the last lines of the log (40 by default)
//! diag crashes       what went wrong: processes killed by faults, panics,
//!                    failed or restarted services
//! diag save PATH     a report (system, crashes, the whole kept log) to a
//!                    file, e.g. /usb/diag.txt, to send
//! ```
//!
//! Needs `logs` (`run diag out logs ...`), which the shell holds only
//! because init granted it (`grant = log-read`); `save` also needs
//! `use:fs`, and `sysinfo` adds the system's summary.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_abi::sysinfo::{self, KernelInfo, MemoryInfo};
use oceans_fs_proto::{Kind, Node, flags};
use oceans_rt::{Directory, Handle, Start};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\n");
oceans_rt::entry!(main);

const USAGE: &str = "usage: diag log [LINES] | crashes | save PATH";

/// What a crash, a failure or trouble looks like in the log.
const TROUBLE: &[&str] = &[
    " killed: ",
    "KERNEL PANIC",
    "panicked",
    "exited with code -",
    " failed",
    "restarting",
    "[ERROR]",
    "[WARN ]",
];

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let logs = match require(&mut out, &directory, "diag", "logs", "logs", "logs") {
        Ok(logs) => logs,
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let log = kept_log(logs);
    match (words.next(), words.next(), words.next()) {
        (None, ..) | (Some("log"), None, None) => last_lines(&mut out, &log, 40),
        (Some("log"), Some(count), None) => match count.parse() {
            Ok(count) => last_lines(&mut out, &log, count),
            Err(_) => {
                let _ = writeln!(out, "{USAGE}");
                return EXIT_USAGE;
            }
        },
        (Some("crashes"), None, None) => {
            let found = troubles(&log).count();
            if found == 0 {
                let _ = writeln!(out, "diag: nothing went wrong in the kept log");
            } else {
                let _ = writeln!(out, "diag: {found} lines of trouble in the kept log:");
                for line in troubles(&log) {
                    let _ = writeln!(out, "  {line}");
                }
            }
        }
        (Some("save"), Some(path), None) => return save(&mut out, &directory, &log, path),
        _ => {
            let _ = writeln!(out, "{USAGE}");
            return EXIT_USAGE;
        }
    }
    0
}

/// All the log the kernel keeps.
fn kept_log(logs: Handle) -> String {
    let mut bytes = Vec::new();
    let mut from = 0u64;
    let mut chunk = [0u8; 4096];
    loop {
        match oceans_rt::log_read(logs, from, &mut chunk) {
            Ok((0, _)) | Err(_) => break,
            Ok((count, start)) => {
                // Older text gave way while reading: start over from there.
                if start != from && !bytes.is_empty() {
                    bytes.clear();
                }
                bytes.extend_from_slice(&chunk[..count]);
                from = start + count as u64;
            }
        }
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // The oldest line may be cut: drop it.
    match text.find('\n') {
        Some(at) if bytes.len() >= oceans_abi::LOG_RING => text[at + 1..].into(),
        _ => text,
    }
}

fn troubles(log: &str) -> impl Iterator<Item = &str> {
    log.lines()
        .filter(|line| TROUBLE.iter().any(|mark| line.contains(mark)))
}

fn last_lines(out: &mut oceans_rt::Out, log: &str, count: usize) {
    let lines: Vec<&str> = log.lines().collect();
    for line in &lines[lines.len().saturating_sub(count)..] {
        let _ = writeln!(out, "{line}");
    }
}

fn save(out: &mut oceans_rt::Out, directory: &Directory, log: &str, path: &str) -> i64 {
    let fs = match require(out, directory, "diag", "use", "fs", "use:fs") {
        Ok(fs) => Node(fs),
        Err(code) => return code,
    };
    let mut report = String::from("== Oceans diagnostics (ADR-0070) ==\n");
    if let Some(sysinfo) = directory.find("sysinfo", "sysinfo") {
        let mut record = [0u8; KernelInfo::SIZE];
        if let Some(kernel) = oceans_rt::system_info(sysinfo, sysinfo::KERNEL, &mut record)
            .ok()
            .and_then(|len| KernelInfo::decode(&record[..len]))
        {
            let _ = writeln!(
                report,
                "system: Oceans {} {} (ABI {})",
                sysinfo::text(&kernel.version),
                sysinfo::text(&kernel.arch),
                kernel.abi_version
            );
        }
        let mut record = [0u8; MemoryInfo::SIZE];
        if let Some(memory) = oceans_rt::system_info(sysinfo, sysinfo::MEMORY, &mut record)
            .ok()
            .and_then(|len| MemoryInfo::decode(&record[..len]))
        {
            let mib = |frames: u64| (frames * memory.page_size) >> 20;
            let _ = writeln!(
                report,
                "memory: {} MiB free of {} MiB",
                mib(memory.free_frames),
                mib(memory.total_frames)
            );
        }
    }
    let _ = writeln!(report, "uptime: {} s", oceans_rt::clock_ms() / 1000);
    if let Some(ms) = oceans_rt::unix_time_ms() {
        let _ = writeln!(report, "time: {} s since 1970 (UTC)", ms / 1000);
    }
    report.push_str("\n== what went wrong ==\n");
    for line in troubles(log) {
        report.push_str(line);
        report.push('\n');
    }
    report.push_str("\n== the kept log ==\n");
    report.push_str(log);

    let file = fs.walk(path, flags::CREATE_FILE | flags::WRITE);
    let written = file.and_then(|(file, kind)| {
        let result = if kind == Kind::File {
            file.truncate(0)
                .and_then(|()| file.write_all(0, report.as_bytes()))
                .and_then(|()| file.sync())
        } else {
            Err(oceans_fs_proto::FsError::Status(
                oceans_fs_proto::Status::IsADirectory,
            ))
        };
        file.close();
        result
    });
    match written {
        Ok(()) => {
            let _ = writeln!(out, "diag: saved {} bytes to {path}", report.len());
            0
        }
        Err(error) => {
            let _ = writeln!(out, "diag: {path}: {}", error.message());
            EXIT_FAILED
        }
    }
}
