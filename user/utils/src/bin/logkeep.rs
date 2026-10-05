//! `logkeep`: the log, kept on disk across reboots (ADR-0074).
//!
//! A service. At start it moves the last boot's log aside, then copies
//! what the kernel keeps (ADR-0070) into `/system/logs/boot.log` every
//! couple of seconds, synced. After a crash or a hang, the next boot finds
//! it in `/system/logs/previous-boot.log` (`diag previous`).
//!
//! Its capabilities, in order: `grant = log`, `grant = log-read`,
//! `use = fs`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use oceans_fs_proto::{FsError, Kind, Node, flags};
use oceans_rt::{Handle, Start};

oceans_rt::entry!(main);

const DIR: &str = "system/logs";
const CURRENT: &str = "boot.log";
const PREVIOUS: &str = "previous-boot.log";
/// The most kept of one boot: the start of a boot is what explains it.
const MAX_FILE: u64 = 512 * 1024;
/// How often new lines are copied and synced.
const PERIOD_MS: u64 = 2000;

fn main(start: Start) -> i64 {
    let (Some(&log), Some(&logs), Some(&fs)) = (
        start.handles.first(),
        start.handles.get(1),
        start.handles.get(2),
    ) else {
        return 1;
    };
    let root = Node(fs);
    let file = match open(&root) {
        Ok(file) => file,
        Err(error) => {
            say(
                log,
                &format!(
                    "logkeep: cannot keep the log in /{DIR}: {}",
                    error.message()
                ),
            );
            // Nothing to do without a disk; not worth restarting.
            loop {
                oceans_rt::sleep_ms(60_000);
            }
        }
    };
    say(
        log,
        &format!("logkeep: keeping the log in /{DIR}/{CURRENT}"),
    );
    let mut from = 0u64;
    let mut written = 0u64;
    let mut full = false;
    let mut chunk = [0u8; 4096];
    loop {
        let mut copied = false;
        while !full {
            let Ok((count, start)) = oceans_rt::log_read(logs, from, &mut chunk) else {
                break;
            };
            if count == 0 {
                break;
            }
            // Older text gave way before it was copied.
            if start > from && from > 0 {
                let note = format!("[logkeep: {} bytes of log lost]\n", start - from);
                if file.write_all(written, note.as_bytes()).is_ok() {
                    written += note.len() as u64;
                }
            }
            let take = (count as u64).min(MAX_FILE.saturating_sub(written)) as usize;
            if file.write_all(written, &chunk[..take]).is_err() {
                break;
            }
            written += take as u64;
            from = start + count as u64;
            copied = true;
            if written >= MAX_FILE {
                let note = b"\n[logkeep: the rest of this boot's log is not kept on disk]\n";
                let _ = file.write_all(written, note);
                full = true;
            }
        }
        if copied {
            let _ = file.sync();
        }
        oceans_rt::sleep_ms(PERIOD_MS);
    }
}

/// `/system/logs/boot.log`, new and empty; the last boot's renamed to
/// `previous-boot.log` first.
fn open(root: &Node) -> Result<Node, FsError> {
    let (dir, _) = root.walk(DIR, flags::CREATE_DIRECTORY | flags::WRITE)?;
    let result = (|| {
        if let Ok((old, _)) = dir.walk(CURRENT, 0) {
            old.close();
            dir.rename(CURRENT, PREVIOUS)?;
        }
        let (file, kind) = dir.walk(CURRENT, flags::CREATE_FILE | flags::WRITE)?;
        if kind != Kind::File {
            file.close();
            return Err(FsError::Status(oceans_fs_proto::Status::IsADirectory));
        }
        file.truncate(0)?;
        dir.sync()?;
        Ok(file)
    })();
    dir.close();
    result
}

fn say(log: Handle, line: &str) {
    let _ = oceans_rt::debug_write(log, line);
}
