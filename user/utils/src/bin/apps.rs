//! `apps`: lists, starts and stops apps through whatever Oceans Core
//! capability it was given (ADR-0048), e.g. a narrow one:
//!
//! ```text
//! run apps out core:query -- list
//! run apps out core:query+run -- start app.oceans.hello
//! ```
//!
//! ```text
//! apps list            installed apps
//! apps start ID [ARGS] start an app in the background
//! apps stop ID         stop it
//! ```
//!
//! What the capability does not allow is refused by Core, not by `apps`.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_core_proto::{Core, CoreError, Status, op, parts, run_flags};
use oceans_rt::Start;
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant core:query\n");
oceans_rt::entry!(main);

const USAGE: &str = "usage: apps list | start ID [ARGS...] | stop ID | mint RIGHTS";

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let core = match require(&mut out, &directory, "apps", "use", "core", "core:query") {
        Ok(core) => Core(core),
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let mut reply = [0u8; 256];
    let failed = |out: &mut oceans_rt::Out, what: &str, error: CoreError| {
        let _ = writeln!(out, "apps: {what}: {}", error.message());
        EXIT_FAILED
    };
    match (words.next(), words.next()) {
        (Some("list"), None) => {
            for index in 0u32.. {
                match core.call(op::LIST, &index.to_le_bytes(), &[], &mut reply) {
                    Ok(got) if got.len >= 1 => {
                        let mut fields = parts(&reply[1..got.len]);
                        let _ = writeln!(
                            out,
                            "{} {}{}",
                            fields.next().unwrap_or("?"),
                            fields.next().unwrap_or("?"),
                            if reply[0] != 0 { " (running)" } else { "" }
                        );
                    }
                    Err((CoreError::Status(Status::NotFound), _)) | Ok(_) => return 0,
                    Err((error, _)) => return failed(&mut out, "list", error),
                }
            }
            0
        }
        (Some("start"), Some(id)) => {
            let rest: &str = words.next().map_or("", |first| {
                let at = args.find(first).unwrap_or(args.len());
                &args[at..]
            });
            let mut data = [0u8; 248];
            let len = 2 + id.len() + rest.len();
            if id.len() > oceans_core_proto::MAX_ID || len > data.len() {
                let _ = writeln!(out, "apps: {id}: arguments too long");
                return EXIT_USAGE;
            }
            data[0] = run_flags::DETACH;
            data[1] = id.len() as u8;
            data[2..2 + id.len()].copy_from_slice(id.as_bytes());
            data[2 + id.len()..len].copy_from_slice(rest.as_bytes());
            match core.call(op::RUN, &data[..len], &[], &mut reply) {
                Ok(_) => {
                    let _ = writeln!(out, "apps: started {id}");
                    0
                }
                Err((error, _)) => failed(&mut out, id, error),
            }
        }
        // Attenuation: a narrower end from ours; never a wider one.
        (Some("mint"), Some(names)) => {
            let Some(rights) = oceans_core_proto::access::parse(names) else {
                let _ = writeln!(
                    out,
                    "apps: {names}: rights are query, run, manage, decide, audit"
                );
                return EXIT_USAGE;
            };
            match core.call(op::MINT, &[rights], &[], &mut reply) {
                Ok(got) => {
                    if let Some(handle) = got.handle {
                        let _ = oceans_rt::close(handle);
                    }
                    let _ = writeln!(out, "apps: minted {names}");
                    0
                }
                Err((error, _)) => failed(&mut out, names, error),
            }
        }
        (Some("stop"), Some(id)) => match core.about(op::STOP, &[], id, &mut reply) {
            Ok(_) => {
                let _ = writeln!(out, "apps: stopped {id}");
                0
            }
            Err((error, _)) => failed(&mut out, id, error),
        },
        _ => {
            let _ = writeln!(out, "{USAGE}");
            EXIT_USAGE
        }
    }
}
