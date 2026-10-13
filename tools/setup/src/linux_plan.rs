//! What Oceans Setup asks of a Linux PC and does to it (ADR-0131), as
//! text parsed and bytes built: tested on any host.
//!
//! On Linux nothing is shrunk: the installer is staged in the EFI system
//! partition Linux already has (mounted at `/boot/efi`, `/efi` or `/boot`),
//! and the UEFI boot entries are written through efivarfs.

use oceans_install::{ESP_MIB, MIN_ROOT_MIB};

/// Where Linux mounts the EFI system partition, in the order looked at.
pub const ESP_MOUNTS: [&str; 3] = ["/boot/efi", "/efi", "/boot"];
/// The variables' vendor GUID, as efivarfs names files.
pub const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
pub const EFIVARS: &str = "/sys/firmware/efi/efivars";
/// Non-volatile, boot service and runtime access.
const ATTRIBUTES: u32 = 7;
/// Kept free in the ESP after staging, for the firmware and Linux.
pub const ESP_MARGIN: u64 = 8 << 20;

/// The EFI system partition as `/proc/mounts` lists it: its device and
/// where it is mounted.
pub fn find_esp(mounts: &str) -> Option<(String, String)> {
    let vfat: Vec<(&str, &str)> = mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (device, at, kind) = (fields.next()?, fields.next()?, fields.next()?);
            (kind == "vfat").then_some((device, at))
        })
        .collect();
    ESP_MOUNTS.iter().find_map(|want| {
        vfat.iter()
            .find(|(_, at)| at == want)
            .map(|(device, at)| (device.to_string(), at.to_string()))
    })
}

/// The disk and the EFI system partition, as Linux says them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinuxDisk {
    /// `nvme0n1`.
    pub name: String,
    pub model: String,
    pub logical_sector: u64,
    /// Bytes.
    pub size: u64,
    /// The disk starts with a GPT header.
    pub gpt: bool,
    pub esp: Esp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Esp {
    /// `/dev/nvme0n1p1`.
    pub device: String,
    pub mount: String,
    /// The partition's number in the GPT (from 1).
    pub number: u32,
    /// In 512-byte sectors.
    pub start: u64,
    pub sectors: u64,
    /// As stored on disk.
    pub guid: [u8; 16],
    /// Bytes free on it.
    pub free: u64,
}

/// Why Oceans cannot be installed from `disk`, if it cannot; `needed` is
/// what the staged files take.
pub fn refusals(disk: &LinuxDisk, needed: u64) -> Vec<String> {
    let mib = 1 << 20;
    let mut why = Vec::new();
    if !disk.name.starts_with("nvme") {
        why.push(format!(
            "Linux is on {}: Oceans installs on an NVMe SSD only, for now",
            disk.name
        ));
    }
    if !disk.gpt {
        why.push("the disk is not GPT-partitioned (UEFI PCs use GPT)".into());
    }
    if disk.logical_sector != 512 {
        why.push(format!(
            "the disk has {}-byte sectors; Oceans needs 512",
            disk.logical_sector
        ));
    }
    if disk.size < (ESP_MIB + MIN_ROOT_MIB + 16) * mib {
        why.push("the disk is too small".into());
    }
    if disk.esp.free < needed + ESP_MARGIN {
        why.push(format!(
            "the EFI system partition ({}) has {} MiB free; the installer needs {} MiB",
            disk.esp.mount,
            disk.esp.free / mib,
            (needed + ESP_MARGIN).div_ceil(mib)
        ));
    }
    why
}

/// The disk a partition belongs to, from its sysfs path's parent
/// (`.../nvme0n1/nvme0n1p1`).
pub fn parent_disk(sys_path: &str) -> Option<&str> {
    let mut parts = sys_path.trim_end_matches('/').rsplit('/');
    parts.next()?;
    parts.next().filter(|name| !name.is_empty())
}

/// A number in a sysfs file.
pub fn sys_number(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// What `df -B1 --output=avail` says is free.
pub fn df_available(text: &str) -> Option<u64> {
    text.lines()
        .filter_map(|line| line.trim().parse().ok())
        .next()
}

/// efivarfs's file for variable `name` (`Boot0003`, `BootOrder`).
pub fn efivar_file(name: &str) -> String {
    format!("{EFIVARS}/{name}-{GLOBAL_GUID}")
}

/// What is written to an efivarfs file: the attributes, then the value.
pub fn efivar_bytes(value: &[u8]) -> Vec<u8> {
    let mut bytes = ATTRIBUTES.to_le_bytes().to_vec();
    bytes.extend_from_slice(value);
    bytes
}

/// A variable's value, from its efivarfs file (after the attributes).
pub fn efivar_value(file: &[u8]) -> Option<&[u8]> {
    file.get(4..)
}

/// Whether the `SecureBoot` variable's file says it is on.
pub fn secure_boot_on(file: &[u8]) -> bool {
    efivar_value(file).and_then(|v| v.first()) == Some(&1)
}

/// The `Boot####` numbers that exist, from efivarfs's file names.
pub fn existing_entries<'a>(names: impl Iterator<Item = &'a str>) -> Vec<u16> {
    let suffix = format!("-{GLOBAL_GUID}");
    names
        .filter_map(|name| name.strip_suffix(&suffix)?.strip_prefix("Boot"))
        .filter(|hex| hex.len() == 4)
        .filter_map(|hex| u16::from_str_radix(hex, 16).ok())
        .collect()
}

/// `count` entry numbers free of `used`, lowest first.
pub fn free_entries(used: &[u16], count: usize) -> Vec<u16> {
    (0..=0xffffu16)
        .filter(|n| !used.contains(n))
        .take(count)
        .collect()
}

/// The staged files to write into the existing ESP: all of them, but
/// the fallback loader only where there is none (another system's stays
/// untouched until the installer replaces the disk).
pub fn files_for_esp<'a>(
    files: &'a [(&'static str, Vec<u8>)],
    fallback_exists: bool,
) -> Vec<&'a (&'static str, Vec<u8>)> {
    files
        .iter()
        .filter(|(path, _)| !(fallback_exists && *path == oceans_install::STAGED_FALLBACK))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "sysfs /sys sysfs rw,nosuid 0 0\n\
        /dev/nvme0n1p2 / ext4 rw,relatime 0 0\n\
        /dev/sdb1 /media/usb vfat rw 0 0\n\
        /dev/nvme0n1p1 /boot/efi vfat rw,relatime,fmask=0077 0 0\n";

    fn disk() -> LinuxDisk {
        LinuxDisk {
            name: "nvme0n1".into(),
            model: "SAMSUNG MZVPV512HDGL".into(),
            logical_sector: 512,
            size: 512_110_190_592,
            gpt: true,
            esp: Esp {
                device: "/dev/nvme0n1p1".into(),
                mount: "/boot/efi".into(),
                number: 1,
                start: 2048,
                sectors: 1_048_576,
                guid: [7; 16],
                free: 500 << 20,
            },
        }
    }

    #[test]
    fn the_esp_from_proc_mounts() {
        assert_eq!(
            find_esp(MOUNTS),
            Some(("/dev/nvme0n1p1".to_string(), "/boot/efi".to_string()))
        );
        // A vfat stick is not the ESP; /efi and /boot are looked at too.
        assert_eq!(find_esp("/dev/sdb1 /media/usb vfat rw 0 0\n"), None);
        assert_eq!(
            find_esp("/dev/nvme0n1p1 /efi vfat rw 0 0\n").map(|e| e.1),
            Some("/efi".to_string())
        );
        // An ext4 /boot is not it.
        assert_eq!(find_esp("/dev/nvme0n1p2 /boot ext4 rw 0 0\n"), None);
    }

    #[test]
    fn a_debian_nvme_pc_is_fine() {
        assert!(refusals(&disk(), 300 << 20).is_empty());
        let small_esp = LinuxDisk {
            esp: Esp {
                free: 100 << 20,
                ..disk().esp
            },
            ..disk()
        };
        let why = refusals(&small_esp, 300 << 20);
        assert_eq!(why.len(), 1);
        assert!(why[0].contains("100 MiB free"), "{why:?}");
        let sata = LinuxDisk {
            name: "sda".into(),
            gpt: false,
            logical_sector: 4096,
            ..disk()
        };
        assert_eq!(refusals(&sata, 0).len(), 3);
    }

    #[test]
    fn sysfs_and_df() {
        assert_eq!(
            parent_disk("/sys/devices/pci0000:00/0000:00:1d.0/nvme/nvme0/nvme0n1/nvme0n1p1"),
            Some("nvme0n1")
        );
        assert_eq!(parent_disk("nvme0n1p1"), None);
        assert_eq!(sys_number("2048\n"), Some(2048));
        assert_eq!(df_available("     Avail\n 524288000\n"), Some(524_288_000));
        assert_eq!(df_available("Avail\n"), None);
    }

    #[test]
    fn efivarfs_files() {
        assert_eq!(
            efivar_file("BootOrder"),
            "/sys/firmware/efi/efivars/BootOrder-8be4df61-93ca-11d2-aa0d-00e098032b8c"
        );
        assert_eq!(efivar_bytes(&[1, 0]), [7, 0, 0, 0, 1, 0]);
        assert_eq!(efivar_value(&[6, 0, 0, 0, 3, 0]), Some(&[3u8, 0][..]));
        assert!(secure_boot_on(&[6, 0, 0, 0, 1]));
        assert!(!secure_boot_on(&[6, 0, 0, 0, 0]));
        assert!(!secure_boot_on(&[]));
        let names = [
            "Boot0000-8be4df61-93ca-11d2-aa0d-00e098032b8c",
            "Boot0002-8be4df61-93ca-11d2-aa0d-00e098032b8c",
            "BootOrder-8be4df61-93ca-11d2-aa0d-00e098032b8c",
            "Boot0001-other-guid",
            "SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c",
        ];
        let used = existing_entries(names.iter().copied());
        assert_eq!(used, [0, 2]);
        assert_eq!(free_entries(&used, 2), [1, 3]);
    }

    #[test]
    fn another_systems_fallback_loader_stays() {
        let files: Vec<(&'static str, Vec<u8>)> = vec![
            (oceans_install::STAGED_LIMINE, vec![1]),
            (oceans_install::STAGED_FALLBACK, vec![1]),
            ("limine.conf", vec![2]),
        ];
        assert_eq!(files_for_esp(&files, false).len(), 3);
        let kept = files_for_esp(&files, true);
        assert_eq!(kept.len(), 2);
        assert!(
            kept.iter()
                .all(|(p, _)| *p != oceans_install::STAGED_FALLBACK)
        );
    }
}
