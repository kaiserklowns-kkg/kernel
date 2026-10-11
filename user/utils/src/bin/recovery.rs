//! `recovery`: repairing the usual system from recovery mode (ADR-0122),
//! where Core, the desktop and the apps are not running.
//!
//! ```text
//! recovery                      what is here: the accounts, the boots
//!                               that did not finish, what can be done
//! recovery password LOGIN NEW   a new password for an account whose
//!                               password is forgotten (`owner` is the
//!                               first account)
//! recovery factory-reset --erase-everything
//!                               the accounts, settings, installed apps and
//!                               every user's files go; the next boot is
//!                               a new computer's
//! ```
//!
//! Needs `use:fs` (`run recovery out use:fs ...`): the root filesystem.
//! The disk is not encrypted (ADR-0122): whoever can start recovery can
//! read the files anyway, so a new password gives nothing more.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_fs_proto::{FsError, Kind, Node, flags, tree};
use oceans_rt::Start;
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\n");
oceans_rt::entry!(main);

const USAGE: &str = "usage: recovery [password LOGIN NEW | factory-reset --erase-everything]";
/// Where Core keeps the accounts (ADR-0110, ADR-0115) and counts boots
/// (ADR-0122).
const USERS: &str = "system/user";
const BOOTS: &str = "system/boots";
/// What a factory reset lets go: the system's settings (all of `/system`
/// but its logs), the installed apps and their data, and every Home.
const SYSTEM: &str = "system";
const KEPT_IN_SYSTEM: &str = "logs";
const APPS: &str = "apps";
const HOMES: [&str; 2] = ["home", "users"];

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let root = match require(&mut out, &directory, "recovery", "use", "fs", "use:fs") {
        Ok(fs) => Node(fs),
        Err(code) => return code,
    };
    let args = directory.args();
    let words: Vec<&str> = args.split_whitespace().collect();
    match words.as_slice() {
        [] | ["status"] => status(&mut out, &root),
        ["password", login, new] => password(&mut out, &root, login, new),
        ["factory-reset", "--erase-everything"] => factory_reset(&mut out, &root),
        ["factory-reset", ..] => {
            let _ = writeln!(
                out,
                "recovery: a factory reset erases every account, setting, app and file;\n\
                 to do it, type: run recovery out use:fs -- factory-reset --erase-everything"
            );
            EXIT_USAGE
        }
        _ => {
            let _ = writeln!(out, "{USAGE}");
            EXIT_USAGE
        }
    }
}

/// What is here, and what can be done.
fn status(out: &mut oceans_rt::Out, root: &Node) -> i64 {
    let _ = writeln!(out, "recovery: the usual system is not started (ADR-0122)");
    match read(root, USERS).map(|text| oceans_account::Users::decode(&text)) {
        Ok(Some(users)) => {
            for account in &users.accounts {
                let _ = writeln!(
                    out,
                    "  account {} ({}){}{}",
                    account.id,
                    account.name,
                    if account.admin() {
                        ", administrator"
                    } else {
                        ""
                    },
                    if account.secret.is_some() {
                        ", a password"
                    } else {
                        ", no password"
                    }
                );
            }
        }
        Ok(None) => {
            let _ = writeln!(
                out,
                "  the accounts' file is damaged: a factory reset starts afresh"
            );
        }
        Err(_) => {
            let _ = writeln!(out, "  one account, never given a password");
        }
    }
    let boots = read(root, BOOTS)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    if let Some(boots) = boots.filter(|&n| n > 0) {
        let _ = writeln!(out, "  {boots} boot(s) in a row did not finish");
    }
    let _ = writeln!(
        out,
        "what can be done:\n\
         \x20 run recovery out use:fs -- password LOGIN NEW   a forgotten password\n\
         \x20 run recovery out use:fs -- factory-reset        everything goes\n\
         \x20 run diag out logs use:fs -- previous            the last boot's log\n\
         \x20 run diag out logs use:fs -- save /usb/diag.txt  the logs, to send\n\
         \x20 run update out use:fs -- previous               the previous release first\n\
         \x20 reboot, and choose \"(safe mode)\": the usual system without its services"
    );
    0
}

/// A new password for `login` (ADR-0122): Argon2id with a new salt, as
/// Core keeps one; the account stays as it was otherwise.
fn password(out: &mut oceans_rt::Out, root: &Node, login: &str, new: &str) -> i64 {
    if !oceans_account::valid_password(new) {
        let _ = writeln!(out, "recovery: a password has 4 to 64 characters");
        return EXIT_USAGE;
    }
    let text = read(root, USERS).unwrap_or_default();
    let Some(mut users) = oceans_account::Users::decode(&text) else {
        let _ = writeln!(
            out,
            "recovery: the accounts' file is damaged; a factory reset starts afresh"
        );
        return EXIT_FAILED;
    };
    let Some(index) = users.index(login) else {
        let logins: Vec<&str> = users.accounts.iter().map(|a| a.id.as_str()).collect();
        let _ = writeln!(
            out,
            "recovery: no account {login} (there are: {})",
            logins.join(", ")
        );
        return EXIT_USAGE;
    };
    let mut salt = [0u8; oceans_account::SALT_LEN];
    if oceans_rt::random(&mut salt).is_err() {
        let _ = writeln!(out, "recovery: no randomness for the password's salt");
        return EXIT_FAILED;
    }
    let Some(secret) = oceans_account::Secret::new(new, salt, oceans_account::DEFAULT) else {
        let _ = writeln!(out, "recovery: the password could not be hashed");
        return EXIT_FAILED;
    };
    users.accounts[index].secret = Some(secret);
    match write(root, USERS, users.encode().as_bytes()) {
        Ok(()) => {
            let _ = writeln!(
                out,
                "recovery: {login}'s password is set; it opens the lock screen"
            );
            0
        }
        Err(problem) => {
            let _ = writeln!(out, "recovery: {USERS}: {problem}");
            EXIT_FAILED
        }
    }
}

/// Everything the users made or chose goes (ADR-0122): the next boot is a
/// new computer's, its system apps put back by Core from the image.
fn factory_reset(out: &mut oceans_rt::Out, root: &Node) -> i64 {
    let mut failed = false;
    let mut erase =
        |out: &mut oceans_rt::Out, parent: &Node, name: &str, path: &str| match tree::remove_tree(
            parent, name,
        ) {
            Ok(()) => {
                let _ = writeln!(out, "  erased /{path}");
            }
            Err(FsError::Status(oceans_fs_proto::Status::NotFound)) => {}
            Err(problem) => {
                failed = true;
                let _ = writeln!(out, "  /{path}: {}", problem.message());
            }
        };
    let _ = writeln!(out, "recovery: factory reset");
    // `/system`, but its logs (what happened stays readable).
    if let Ok((system, Kind::Directory)) = root.open(SYSTEM, flags::WRITE) {
        for name in names(&system) {
            if name != KEPT_IN_SYSTEM {
                erase(out, &system, &name, &alloc::format!("{SYSTEM}/{name}"));
            }
        }
        system.close();
    }
    erase(out, root, APPS, APPS);
    // Each Home emptied, the folders kept for Core.
    for home in HOMES {
        if let Ok((dir, Kind::Directory)) = root.open(home, flags::WRITE) {
            for name in names(&dir) {
                erase(out, &dir, &name, &alloc::format!("{home}/{name}"));
            }
            dir.close();
        }
    }
    if failed {
        let _ = writeln!(out, "recovery: not everything could be erased");
        return EXIT_FAILED;
    }
    let _ = writeln!(
        out,
        "recovery: done; `reboot` starts Oceans as on a new computer"
    );
    0
}

/// The names in `dir`.
fn names(dir: &Node) -> Vec<String> {
    let mut found = Vec::new();
    let mut name = [0u8; oceans_fs_proto::MAX_NAME];
    for index in 0.. {
        match dir.entry(index, &mut name) {
            Ok(Some((_, len))) => {
                if let Ok(text) = core::str::from_utf8(&name[..len]) {
                    found.push(String::from(text));
                }
            }
            _ => break,
        }
    }
    found
}

fn read(root: &Node, path: &str) -> Result<String, FsError> {
    let (file, kind) = root.walk(path, 0)?;
    let result = if kind == Kind::File {
        file.stat().and_then(|stat| {
            let mut bytes = alloc::vec![0u8; stat.size as usize];
            let mut done = 0;
            while done < bytes.len() {
                let got = file.read(done as u64, &mut bytes[done..])?;
                if got == 0 {
                    break;
                }
                done += got;
            }
            bytes.truncate(done);
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })
    } else {
        Err(FsError::Status(oceans_fs_proto::Status::IsADirectory))
    };
    file.close();
    result
}

fn write(root: &Node, path: &str, bytes: &[u8]) -> Result<(), &'static str> {
    let (file, kind) = root
        .walk(path, flags::CREATE_FILE | flags::WRITE)
        .map_err(FsError::message)?;
    let result = if kind == Kind::File {
        file.truncate(0)
            .and_then(|()| file.write_all(0, bytes))
            .and_then(|()| file.sync())
            .map_err(FsError::message)
    } else {
        Err("not a file")
    };
    file.close();
    result
}
