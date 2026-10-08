//! `update`: system updates (ADR-0071).
//!
//! ```text
//! update status              the running release and the boot entries
//! update apply PATH          verify an update and install it to start next
//! ```
//!
//! An update is a package (`.opk`, ADR-0046) with the id `system.oceans`:
//! the kernel (`oceans-kernel`) and the boot archive (`initrd`), signed by
//! a key in the system's **update keys** (`/bin/update.keys`, from the boot
//! image; never the keys of app developers). It must be newer than the
//! running release (`/bin/release`).
//!
//! The boot partition (the USB stick Oceans started from, `/usb`) has two
//! slots, `/boot/a` and `/boot/b`. The update goes into the slot not in
//! use; then the boot configuration is switched so that it starts first
//! and the running release stays in the boot menu as "previous". Limine
//! reads `/limine.conf` and, if that is missing, `/boot/limine/limine.conf`:
//! the second is replaced first, then the first, so whatever moment power
//! fails, one complete configuration is there.
//!
//! A stick signed for **Secure Boot** (ADR-0091, `/boot/secure-boot`) keeps
//! each slot on a partition of its own, with its own signed Limine, and
//! switches them in the partition table (ADR-0092, `secure.rs`). The
//! update must carry the slot's signed Limine and configuration.
//!
//! Needs `use:fs` (`run update out use:fs -- apply /usb/update.opk`); with
//! Secure Boot also the stick itself, `use:usbdisk`.

#![no_std]
#![no_main]

extern crate alloc;

mod secure;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_fs_proto::{FsError, Kind, Node, Shared, flags};
use oceans_package::{Package, TrustedKey, Version};
use oceans_rt::{Handle, Out, Start};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\n");
oceans_rt::entry!(main);

const USAGE: &str =
    "usage: update status | apply PATH   (run update out use:fs [use:usbdisk] -- ...)";
/// The boot partition: the stick Oceans started from.
const ESP: &str = "usb";
const SYSTEM_ID: &str = "system.oceans";
/// On a boot partition signed for Secure Boot (ADR-0091).
const SECURE_BOOT_MARKER: &str = "usb/boot/secure-boot";
/// Bytes moved per request.
const CHUNK: usize = 256 * 1024;
/// The largest update read.
const MAX_UPDATE: u64 = 64 << 20;

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let root = match require(&mut out, &directory, "update", "use", "fs", "use:fs") {
        Ok(fs) => Node(fs),
        Err(code) => return code,
    };
    let stick = directory.find("use", "usbdisk");
    let args = directory.args();
    let words: Vec<&str> = args.split_whitespace().collect();
    let result = match words.as_slice() {
        ["status"] | [] => status(&mut out, &root, stick),
        ["apply", path] => apply(&mut out, &root, stick, path),
        _ => {
            let _ = writeln!(out, "{USAGE}");
            return EXIT_USAGE;
        }
    };
    match result {
        Ok(()) => 0,
        Err(why) => {
            let _ = writeln!(out, "update: {why}");
            EXIT_FAILED
        }
    }
}

fn fs_error(what: &str) -> impl Fn(FsError) -> String + '_ {
    move |error| alloc::format!("{what}: {}", error.message())
}

/// A whole file, through a shared buffer.
fn read_file(root: &Node, path: &str, limit: u64) -> Result<Vec<u8>, String> {
    let (file, kind) = root.walk(path, 0).map_err(fs_error(path))?;
    let result = (|| {
        if kind != Kind::File {
            return Err(alloc::format!("{path}: not a file"));
        }
        let size = file.stat().map_err(fs_error(path))?.size;
        if size > limit {
            return Err(alloc::format!("{path}: too large"));
        }
        let shared = file.attach(CHUNK).map_err(fs_error(path))?;
        let mut bytes = alloc::vec![0u8; size as usize];
        let mut done = 0;
        while done < bytes.len() {
            let got = file
                .read_shared(&shared, done as u64, &mut bytes[done..])
                .map_err(fs_error(path))?;
            if got == 0 {
                return Err(alloc::format!("{path}: shorter than its size"));
            }
            done += got;
        }
        Ok(bytes)
    })();
    file.close();
    result
}

/// A text file from the boot image (`/bin`): it is published from a memory
/// object, so it is padded with zeros to a whole page.
fn image_text(root: &Node, path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = read_file(root, path, 64 << 10)?;
    let end = bytes
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |last| last + 1);
    bytes.truncate(end);
    Ok(bytes)
}

/// Writes a whole file, durably.
fn write_file(
    root: &Node,
    path: &str,
    bytes: &[u8],
    shared: Option<&Shared>,
) -> Result<(), String> {
    let (file, kind) = root
        .walk(path, flags::CREATE_FILE | flags::WRITE)
        .map_err(fs_error(path))?;
    let result = (|| {
        if kind != Kind::File {
            return Err(alloc::format!("{path}: not a file"));
        }
        file.truncate(0).map_err(fs_error(path))?;
        match shared {
            Some(shared) => {
                file.share(shared).map_err(fs_error(path))?;
                file.write_shared(shared, 0, bytes)
                    .map_err(fs_error(path))?;
            }
            None => file.write_all(0, bytes).map_err(fs_error(path))?,
        }
        file.sync().map_err(fs_error(path))
    })();
    file.close();
    result
}

/// The running release from `/bin/release`: its version, and the whole
/// line (`VERSION CHANNEL`).
fn running(root: &Node) -> Result<(Version, String), String> {
    let text = image_text(root, "bin/release")?;
    let text = core::str::from_utf8(&text).map_err(|_| "bin/release is not text")?;
    let version = text
        .split_whitespace()
        .next()
        .and_then(Version::parse)
        .ok_or("bin/release names no version")?;
    Ok((version, text.trim().to_string()))
}

fn other(slot: char) -> char {
    if slot == 'a' { 'b' } else { 'a' }
}

/// The release a slot holds, if any.
fn slot_release(root: &Node, slot: char) -> Option<String> {
    let bytes = read_file(root, &alloc::format!("{ESP}/boot/{slot}/release"), 256).ok()?;
    String::from_utf8(bytes)
        .ok()
        .map(|text| text.trim().to_string())
}

/// The slot the boot configuration starts first (`a` or `b`).
fn first_slot(root: &Node) -> Result<char, String> {
    let conf = read_file(root, &alloc::format!("{ESP}/limine.conf"), 64 << 10)
        .or_else(|_| {
            read_file(
                root,
                &alloc::format!("{ESP}/boot/limine/limine.conf"),
                64 << 10,
            )
        })
        .map_err(|_| "no boot configuration on /usb: was Oceans started from its USB image?")?;
    let conf = core::str::from_utf8(&conf).map_err(|_| "the boot configuration is not text")?;
    conf.lines()
        .find_map(|line| line.trim().strip_prefix("path: boot():/boot/"))
        .and_then(|rest| rest.chars().next())
        .filter(|slot| *slot == 'a' || *slot == 'b')
        .ok_or_else(|| "the boot configuration has no slot a or b".to_string())
}

/// The slot Oceans is running from: the one holding the running release
/// (the first one, if both do). Someone may have chosen "previous" in the
/// boot menu, so it is not always the first.
fn running_slot(root: &Node, release: &str) -> Result<char, String> {
    let first = first_slot(root)?;
    [first, other(first)]
        .into_iter()
        .find(|&slot| slot_release(root, slot).as_deref() == Some(release))
        .ok_or_else(|| alloc::format!("the running release ({release}) is in neither slot"))
}

/// On a Secure Boot stick: the stick itself, which `update` must be given.
fn secure_stick(root: &Node, stick: Option<Handle>) -> Result<Option<Handle>, String> {
    let Ok((marker, _)) = root.walk(SECURE_BOOT_MARKER, 0) else {
        return Ok(None);
    };
    marker.close();
    stick.map(Some).ok_or_else(|| {
        "this system boots with Secure Boot: give update the stick too          (run update out use:fs use:usbdisk -- ...)"
            .into()
    })
}

fn status(out: &mut Out, root: &Node, stick: Option<Handle>) -> Result<(), String> {
    let (_, release) = running(root)?;
    if let Some(stick) = secure_stick(root, stick)? {
        return secure::status(out, stick, &release);
    }
    let slot = running_slot(root, &release)?;
    let _ = writeln!(out, "update: running Oceans {release} (slot {slot})");
    let first = first_slot(root)?;
    let first_release = slot_release(root, first).unwrap_or_else(|| "?".to_string());
    let _ = writeln!(out, "update: starts first: slot {first} ({first_release})");
    match slot_release(root, other(first)) {
        Some(previous) => {
            let _ = writeln!(out, "update: previous: slot {} ({previous})", other(first));
        }
        None => {
            let _ = writeln!(out, "update: slot {} is empty", other(first));
        }
    }
    Ok(())
}

fn apply(out: &mut Out, root: &Node, stick: Option<Handle>, path: &str) -> Result<(), String> {
    let secure = secure_stick(root, stick)?;
    let path = path.trim_start_matches('/');
    let bytes = read_file(root, path, MAX_UPDATE)?;
    let keys_text = image_text(root, "bin/update.keys")?;
    let keys_text = core::str::from_utf8(&keys_text).map_err(|_| "bin/update.keys is not text")?;
    let keys: Vec<TrustedKey<'_>> = oceans_package::trusted_keys(keys_text).collect();
    if keys.is_empty() {
        return Err("this system trusts no update keys".into());
    }
    let package =
        Package::open(&bytes, &keys).map_err(|e| alloc::format!("refused: {}", e.message()))?;
    let manifest = &package.manifest;
    if manifest.id != SYSTEM_ID {
        return Err(alloc::format!(
            "refused: {} is an app, not a system update",
            manifest.id
        ));
    }
    let (kernel, initrd) = match (package.file("oceans-kernel"), package.file("initrd")) {
        (Some(kernel), Some(initrd)) => (kernel, initrd),
        _ => return Err("refused: the update lacks the kernel or the boot archive".into()),
    };
    let (running_version, previous) = running(root)?;
    if manifest.version <= running_version {
        return Err(alloc::format!(
            "refused: {} is not newer than the running {running_version}",
            manifest.version
        ));
    }
    let release = alloc::format!("{} {}", manifest.version, manifest.channel);
    if let Some(stick) = secure {
        let _ = writeln!(
            out,
            "update: Oceans {release} from {}, verified",
            manifest.publisher
        );
        let update = secure::Update {
            release: &release,
            kernel,
            initrd,
            efi: package.file(secure::UPDATE_EFI),
            config: package.file(secure::UPDATE_CONFIG),
        };
        return secure::apply(out, stick, &previous, &update);
    }
    // The running slot is never written.
    let slot = running_slot(root, &previous)?;
    let target = other(slot);
    let _ = writeln!(
        out,
        "update: Oceans {release} from {}, verified; writing slot {target}",
        manifest.publisher
    );
    let _ = out.flush();

    // 1. The new release, into the slot not in use.
    let dir = alloc::format!("{ESP}/boot/{target}");
    root.walk(&dir, flags::CREATE_DIRECTORY | flags::WRITE)
        .map(|(node, _)| node.close())
        .map_err(fs_error(&dir))?;
    let shared = Shared::new(CHUNK).map_err(fs_error("memory"))?;
    write_file(
        root,
        &alloc::format!("{dir}/oceans-kernel"),
        kernel,
        Some(&shared),
    )?;
    write_file(root, &alloc::format!("{dir}/initrd"), initrd, Some(&shared))?;
    write_file(
        root,
        &alloc::format!("{dir}/release"),
        release.as_bytes(),
        None,
    )?;

    // 2. The configuration: the second location first, then the first, so
    // one complete configuration is always there.
    let conf = boot_configuration(target, &release, slot, &previous);
    for (dir, name) in [
        (alloc::format!("{ESP}/boot/limine"), "limine.conf"),
        (ESP.to_string(), "limine.conf"),
    ] {
        let new = alloc::format!("{dir}/limine.new");
        write_file(root, &new, conf.as_bytes(), None)?;
        let (parent, _) = root.walk(&dir, flags::WRITE).map_err(fs_error(&dir))?;
        let renamed = parent
            .rename("limine.new", name)
            .and_then(|()| parent.sync());
        parent.close();
        renamed.map_err(fs_error(&alloc::format!("{dir}/{name}")))?;
    }
    let _ = writeln!(
        out,
        "update: Oceans {release} installed in slot {target}; it starts at the next boot \
         (the previous, {previous}, stays in the boot menu)"
    );
    Ok(())
}

/// Limine's configuration: the new release first, the previous one after.
fn boot_configuration(new: char, release: &str, old: char, previous: &str) -> String {
    let entry = |name: &str, slot: char| {
        alloc::format!(
            "/{name}\n    protocol: limine\n    path: boot():/boot/{slot}/oceans-kernel\n    \
             module_path: boot():/boot/{slot}/initrd\n"
        )
    };
    alloc::format!(
        "# Oceans boot configuration (ADR-0071), written by `update`.\n\
         timeout: 3\n\n{}\n{}",
        entry(&alloc::format!("Oceans {release}"), new),
        entry(&alloc::format!("Oceans {previous} (previous)"), old)
    )
}
