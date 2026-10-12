//! Laying a disk out as an installed Oceans (ADR-0126, ADR-0127),
//! host-tested and independent of where the disk is: the installer in
//! Oceans writes the PC's SSD with it, the tests a disk in memory.
//!
//! What it writes, over whatever the disk held:
//! 1. a new GPT: the EFI system partition (ESP) first, then the Oceans
//!    root, the rest of the disk;
//! 2. the ESP, FAT32: Limine at the fallback path, the release in slot
//!    `boot/a`, Limine's configuration in both places it looks;
//! 3. the root's first MiB zeroed, so Oceans formats it at first boot.
//!
//! The order keeps one thing true: until the last write, nothing on the
//! disk looks like a finished Oceans. A power cut before the end leaves a
//! disk that needs the installer (or a USB stick) again, never one that
//! half-boots.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;

use oceans_fat::{Disk, Fat, NodeId, ROOT, Window};
use oceans_gpt::{EFI_SYSTEM, OCEANS_ROOT, Partition, SECTOR};

/// Partitions start on 1 MiB boundaries, as every tool aligns them.
pub const ALIGN: u64 = 2048;
/// The ESP's size: two releases (A/B slots) with room to grow, and
/// whatever the firmware wants to keep there.
pub const ESP_MIB: u64 = 512;
/// The smallest root worth installing to.
pub const MIN_ROOT_MIB: u64 = 1024;
/// The kernel's file name in a slot (the boot configuration names it).
pub const KERNEL: &str = "oceans-kernel";
pub const INITRD: &str = "initrd";

/// What goes on the disk.
pub struct Payload<'a> {
    /// Limine's UEFI executable (`BOOTX64.EFI`).
    pub limine: &'a [u8],
    pub kernel: &'a [u8],
    pub initrd: &'a [u8],
    /// `VERSION CHANNEL`, as a slot's `release` file says it.
    pub release: &'a str,
}

/// The GUIDs to give the new table: the disk's, the ESP's (known to the
/// UEFI boot entry made before the install, ADR-0128) and the root's.
#[derive(Clone, Copy)]
pub struct Guids {
    pub disk: [u8; 16],
    pub esp: [u8; 16],
    pub root: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub esp: Partition,
    pub root: Partition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The disk is too small for the ESP and a root of `MIN_ROOT_MIB`.
    TooSmall,
    Io,
    /// The FAT volume could not be made or filled (what failed).
    Fat(String),
}

impl Error {
    pub fn message(&self) -> String {
        match self {
            Self::TooSmall => format!(
                "the disk is too small (at least {} MiB are needed)",
                ESP_MIB + MIN_ROOT_MIB + 2
            ),
            Self::Io => "the disk could not be written".to_string(),
            Self::Fat(what) => format!("the EFI system partition: {what}"),
        }
    }
}

/// A GUID as tools print it (`0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093`), as
/// GPT stores it: the first three fields little-endian.
pub fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let fields: alloc::vec::Vec<&str> = text.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    if fields.len() != 5
        || fields
            .iter()
            .zip(lens)
            .any(|(f, n)| f.len() != n || !f.is_ascii())
    {
        return None;
    }
    let hex = |text: &str| -> Option<alloc::vec::Vec<u8>> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
            .collect()
    };
    let mut guid = [0u8; 16];
    let mut a = hex(fields[0])?;
    a.reverse();
    let mut b = hex(fields[1])?;
    b.reverse();
    let mut c = hex(fields[2])?;
    c.reverse();
    guid[..4].copy_from_slice(&a);
    guid[4..6].copy_from_slice(&b);
    guid[6..8].copy_from_slice(&c);
    guid[8..10].copy_from_slice(&hex(fields[3])?);
    guid[10..].copy_from_slice(&hex(fields[4])?);
    Some(guid)
}

/// Where the partitions go on a disk of `sectors` (512 bytes).
pub fn plan(sectors: u64, guids: Guids) -> Result<Plan, Error> {
    let (_, last_usable) = oceans_gpt::usable(sectors.max(64));
    let esp_first = ALIGN;
    let esp_last = esp_first + ESP_MIB * 2048 - 1;
    let root_first = esp_last + 1;
    // The root ends on an aligned boundary, before the backup table.
    let root_last = ((last_usable + 1) / ALIGN * ALIGN).saturating_sub(1);
    if sectors < 64 || root_last < root_first || root_last - root_first + 1 < MIN_ROOT_MIB * 2048 {
        return Err(Error::TooSmall);
    }
    Ok(Plan {
        esp: Partition {
            kind: EFI_SYSTEM,
            guid: guids.esp,
            first_lba: esp_first,
            last_lba: esp_last,
            name: "EFI system partition".to_string(),
        },
        root: Partition {
            kind: OCEANS_ROOT,
            guid: guids.root,
            first_lba: root_first,
            last_lba: root_last,
            name: "Oceans".to_string(),
        },
    })
}

/// Limine's configuration for a new install: the release in slot `a`,
/// then the same in safe mode and in recovery mode (ADR-0122). The text
/// `update` writes later has the same shape.
pub fn boot_configuration(release: &str) -> String {
    let entry = |name: &str, cmdline: &str| {
        let cmdline = if cmdline.is_empty() {
            String::new()
        } else {
            format!("    cmdline: {cmdline}\n")
        };
        format!(
            "/{name}\n    protocol: limine\n    path: boot():/boot/a/{KERNEL}\n\
             {cmdline}    module_path: boot():/boot/a/{INITRD}\n"
        )
    };
    format!(
        "# Oceans boot configuration (ADR-0071), written by the installer (ADR-0127).\n\
         timeout: 3\n\n{}\n{}\n{}",
        entry(&format!("Oceans {release}"), ""),
        entry(&format!("Oceans {release} (safe mode)"), "oceans.mode=safe"),
        entry(
            &format!("Oceans {release} (recovery)"),
            "oceans.mode=recovery"
        ),
    )
}

/// Writes the installed layout of `plan` over `disk` of `sectors`.
pub fn lay_out<D: Disk>(
    mut disk: D,
    sectors: u64,
    guids: Guids,
    plan: &Plan,
    payload: &Payload<'_>,
) -> Result<D, Error> {
    // 1. The table.
    for (lba, bytes) in
        oceans_gpt::create(sectors, guids.disk, &[plan.esp.clone(), plan.root.clone()])
    {
        disk.write_at(lba * SECTOR, &bytes).map_err(|_| Error::Io)?;
    }
    // 2. The ESP.
    let window = Window::new(disk, plan.esp.start(), plan.esp.size()).ok_or(Error::Io)?;
    let window = fill_esp(window, payload)?;
    let mut disk = window.into_inner();
    // 3. The root's start: blank, so it is formatted at first boot.
    let zeros = vec![0u8; 64 * 1024];
    for chunk in 0..16 {
        disk.write_at(plan.root.start() + chunk * zeros.len() as u64, &zeros)
            .map_err(|_| Error::Io)?;
    }
    Ok(disk)
}

fn fat_error(what: &str) -> impl Fn(oceans_fat::Error) -> Error + '_ {
    move |e| Error::Fat(format!("{what}: {e:?}"))
}

fn fill_esp<D: Disk>(mut window: Window<D>, payload: &Payload<'_>) -> Result<Window<D>, Error> {
    oceans_fat::format(&mut window, "OCEANS", 0x0ce4_5126).map_err(fat_error("formatting"))?;
    let mut fat = Fat::open(window).map_err(fat_error("opening"))?;
    fat.enable_writes().map_err(fat_error("writing"))?;
    let root = ROOT;
    let conf = boot_configuration(payload.release);
    let efi = make_dir(&mut fat, root, "EFI")?;
    let fallback = make_dir(&mut fat, efi, "BOOT")?;
    put(&mut fat, fallback, "BOOTX64.EFI", payload.limine)?;
    fat.release(fallback);
    fat.release(efi);
    let boot = make_dir(&mut fat, root, "boot")?;
    let slot = make_dir(&mut fat, boot, "a")?;
    put(&mut fat, slot, KERNEL, payload.kernel)?;
    put(&mut fat, slot, INITRD, payload.initrd)?;
    put(
        &mut fat,
        slot,
        "release",
        format!("{}\n", payload.release).as_bytes(),
    )?;
    fat.release(slot);
    let limine = make_dir(&mut fat, boot, "limine")?;
    put(&mut fat, limine, "limine.conf", conf.as_bytes())?;
    fat.release(limine);
    fat.release(boot);
    // Last: the configuration Limine reads first makes it a system.
    put(&mut fat, root, "limine.conf", conf.as_bytes())?;
    fat.sync().map_err(fat_error("syncing"))?;
    Ok(fat.into_disk())
}

fn make_dir<D: Disk>(fat: &mut Fat<D>, dir: NodeId, name: &str) -> Result<NodeId, Error> {
    let node = fat.create(dir, name, true).map_err(fat_error(name))?;
    // Held while its children are made.
    fat.retain(node).map_err(fat_error(name))?;
    Ok(node)
}

fn put<D: Disk>(fat: &mut Fat<D>, dir: NodeId, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let node = fat.create(dir, name, false).map_err(fat_error(name))?;
    fat.retain(node).map_err(fat_error(name))?;
    let mut done = 0;
    while done < bytes.len() {
        let wrote = fat
            .write_file(node, done as u64, &bytes[done..])
            .map_err(fat_error(name))?;
        if wrote == 0 {
            return Err(Error::Fat(format!("{name}: the partition is full")));
        }
        done += wrote;
    }
    fat.release(node);
    Ok(())
}

#[cfg(test)]
mod tests;

// ---- Staging (ADR-0128) ----------------------------------------------------
//
// What the Windows program puts on the partition it makes for the install
// (and the tests on theirs): enough to start Oceans in install mode, and
// the signed release the installer then checks and installs.

/// Limine, where the UEFI boot entry for the install points.
pub const STAGED_LIMINE: &str = "EFI/oceans/BOOTX64.EFI";
/// Limine again at the fallback path: firmware that ignores the boot entry
/// (and the tests, which have none) still finds it.
pub const STAGED_FALLBACK: &str = "EFI/BOOT/BOOTX64.EFI";
pub const STAGED_KERNEL: &str = "oceans/oceans-kernel";
pub const STAGED_INITRD: &str = "oceans/initrd";
pub const STAGED_PACKAGE: &str = "oceans/system.opk";
/// `esp = GUID`: the ESP the installed system's boot entry names.
pub const STAGED_ORDER: &str = "oceans/install";

/// Limine's configuration on the staging partition: Oceans in install
/// mode, at once.
pub fn staging_configuration(release: &str) -> String {
    format!(
        "# Installing Oceans (ADR-0128): written by the Windows program.\n\
         timeout: 0\n\n/Install Oceans {release}\n    protocol: limine\n    \
         path: boot():/{STAGED_KERNEL}\n    cmdline: oceans.mode=install\n    \
         module_path: boot():/{STAGED_INITRD}\n"
    )
}

/// `oceans/install`'s text.
pub fn install_order(esp: &[u8; 16]) -> String {
    format!("esp = {}\n", format_guid(esp))
}

/// A GUID as tools print it, from the stored bytes.
pub fn format_guid(g: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

/// Every file of the staging partition, by path, for a release package
/// `package` holding `kernel`, `initrd` and `limine`.
pub fn staged_files<'a>(
    package: &'a [u8],
    kernel: &'a [u8],
    initrd: &'a [u8],
    limine: &'a [u8],
    configuration: &'a str,
    order: &'a str,
) -> [(&'static str, &'a [u8]); 7] {
    [
        (STAGED_LIMINE, limine),
        (STAGED_FALLBACK, limine),
        ("limine.conf", configuration.as_bytes()),
        (STAGED_KERNEL, kernel),
        (STAGED_INITRD, initrd),
        (STAGED_PACKAGE, package),
        (STAGED_ORDER, order.as_bytes()),
    ]
}
