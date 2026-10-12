//! What Oceans Setup asks of the PC and does to its disk (ADR-0128), as
//! PowerShell scripts and the answers parsed: pure text, tested anywhere.

use oceans_install::{ESP_MIB, MIN_ROOT_MIB};

/// The staging partition: room for the release twice (to boot the
/// installer, and the package it installs) with margin.
pub const STAGING_MIB: u64 = 1024;
/// Windows keeps this much free on C: after shrinking, to be safe.
const MARGIN_MIB: u64 = 512;

/// The disk Windows runs from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemDisk {
    pub number: u32,
    pub model: String,
    pub bus: String,
    pub style: String,
    pub logical_sector: u64,
    pub size: u64,
    /// C:'s size and the least Windows can shrink it to.
    pub c_size: u64,
    pub c_min: u64,
}

pub const SYSTEM_DISK_SCRIPT: &str = "$ErrorActionPreference = 'Stop'\n\
$p = Get-Partition -DriveLetter C\n\
$d = Get-Disk -Number $p.DiskNumber\n\
$b = (Get-PhysicalDisk | Where-Object DeviceId -eq \"$($p.DiskNumber)\").BusType\n\
$s = Get-PartitionSupportedSize -DriveLetter C\n\
\"number=$($p.DiskNumber)\"\n\
\"model=$($d.FriendlyName)\"\n\
\"bus=$b\"\n\
\"style=$($d.PartitionStyle)\"\n\
\"logical=$($d.LogicalSectorSize)\"\n\
\"size=$($d.Size)\"\n\
\"csize=$($p.Size)\"\n\
\"cmin=$($s.SizeMin)\"\n";

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
}

fn number(text: &str, key: &str) -> Result<u64, String> {
    field(text, key)
        .and_then(|v| v.trim().parse().ok())
        .ok_or(format!("Windows did not say the disk's {key}"))
}

pub fn parse_system_disk(text: &str) -> Result<SystemDisk, String> {
    Ok(SystemDisk {
        number: number(text, "number")? as u32,
        model: field(text, "model").unwrap_or("").trim().to_string(),
        bus: field(text, "bus").unwrap_or("").trim().to_string(),
        style: field(text, "style").unwrap_or("").trim().to_string(),
        logical_sector: number(text, "logical")?,
        size: number(text, "size")?,
        c_size: number(text, "csize")?,
        c_min: number(text, "cmin")?,
    })
}

/// Why Oceans cannot be installed on `disk`, if it cannot.
pub fn refusals(disk: &SystemDisk) -> Vec<String> {
    let mib = 1 << 20;
    let mut why = Vec::new();
    if !disk.bus.eq_ignore_ascii_case("NVMe") {
        why.push(format!(
            "Windows is on a {} disk: Oceans installs on an NVMe SSD only, for now",
            disk.bus
        ));
    }
    if !disk.style.eq_ignore_ascii_case("GPT") {
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
    if disk.c_size.saturating_sub(disk.c_min) < (STAGING_MIB + MARGIN_MIB) * mib {
        why.push(format!(
            "Windows cannot free {STAGING_MIB} MiB on C: for the installer (free some space first)"
        ));
    }
    why
}

/// Shrinks C: and makes the staging partition, FAT32, with a drive letter
/// for writing; says where it is.
pub fn staging_script(disk: u32) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $c = Get-Partition -DriveLetter C\n\
         Resize-Partition -DriveLetter C -Size ($c.Size - {STAGING_MIB}MB)\n\
         $n = New-Partition -DiskNumber {disk} -Size {STAGING_MIB}MB \
         -GptType '{{ebd0a0a2-b9e5-4433-87c0-68b6b72699c7}}' -AssignDriveLetter\n\
         Format-Volume -Partition $n -FileSystem FAT32 -NewFileSystemLabel 'OCEANSSETUP' \
         -Confirm:$false | Out-Null\n\
         $n = Get-Partition -DiskNumber {disk} -PartitionNumber $n.PartitionNumber\n\
         \"letter=$($n.DriveLetter)\"\n\
         \"number=$($n.PartitionNumber)\"\n\
         \"offset=$($n.Offset)\"\n\
         \"size=$($n.Size)\"\n\
         \"guid=$($n.Guid)\"\n"
    )
}

/// Hides the staging partition again once written (no drive letter).
pub fn hide_script(disk: u32, number: u32, letter: char) -> String {
    format!(
        "Remove-PartitionAccessPath -DiskNumber {disk} -PartitionNumber {number} \
         -AccessPath '{letter}:\\'"
    )
}

/// The staging partition, as made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staging {
    pub letter: char,
    pub number: u32,
    pub offset: u64,
    pub size: u64,
    pub guid: [u8; 16],
}

pub fn parse_staging(text: &str) -> Result<Staging, String> {
    let letter = field(text, "letter")
        .and_then(|v| v.trim().chars().next())
        .filter(char::is_ascii_alphabetic)
        .ok_or("Windows gave the new partition no drive letter")?;
    let guid = field(text, "guid")
        .map(|v| v.trim().trim_start_matches('{').trim_end_matches('}'))
        .and_then(oceans_install::parse_guid)
        .ok_or("Windows did not say the new partition's GUID")?;
    Ok(Staging {
        letter,
        number: number(text, "number")? as u32,
        offset: number(text, "offset")?,
        size: number(text, "size")?,
        guid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: &str = "number=0\r\nmodel=Samsung SSD 980\r\nbus=NVMe\r\nstyle=GPT\r\n\
                        logical=512\r\nsize=512110190592\r\ncsize=511000000000\r\n\
                        cmin=40000000000\r\n";

    #[test]
    fn a_windows_nvme_disk_is_fine() {
        let disk = parse_system_disk(DISK).unwrap();
        assert_eq!(disk.number, 0);
        assert_eq!(disk.model, "Samsung SSD 980");
        assert!(refusals(&disk).is_empty(), "{:?}", refusals(&disk));
    }

    #[test]
    fn what_is_refused() {
        let disk = SystemDisk {
            bus: "SATA".into(),
            style: "MBR".into(),
            logical_sector: 4096,
            c_min: 510_000_000_000,
            ..parse_system_disk(DISK).unwrap()
        };
        let why = refusals(&disk);
        assert_eq!(why.len(), 4, "{why:?}");
        assert!(parse_system_disk("model=x").is_err());
    }

    #[test]
    fn the_staging_partition_from_windows() {
        let script = staging_script(2);
        assert!(script.contains("New-Partition -DiskNumber 2 -Size 1024MB"));
        assert!(script.contains("-FileSystem FAT32"));
        let staging = parse_staging(
            "letter=E\nnumber=5\noffset=511000000000\nsize=1073741824\n\
             guid={0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093}\n",
        )
        .unwrap();
        assert_eq!(staging.letter, 'E');
        assert_eq!(staging.number, 5);
        assert_eq!(staging.guid, oceans_gpt_root());
        assert!(parse_staging("letter=\nnumber=5\n").is_err());
        assert!(hide_script(2, 5, 'E').contains("-AccessPath 'E:\\'"));
    }

    fn oceans_gpt_root() -> [u8; 16] {
        oceans_install::parse_guid("0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093").unwrap()
    }
}
