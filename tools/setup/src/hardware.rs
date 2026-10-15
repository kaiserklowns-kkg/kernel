//! Know before you install (ADR-0161, ADR-0146 P8): the PC's PCI functions,
//! as Windows or Linux lists them, judged by the same table Oceans itself
//! uses (`oceans_hardware`), before anything is erased.

use oceans_hardware::assess::{self, Assessment, Function, Kind, Verdict};

/// What is said about a machine: a line per function a user would notice,
/// then the verdict.
pub struct Report {
    pub lines: Vec<String>,
    pub assessment: Assessment,
}

impl Report {
    /// The report as a text file (saved beside Oceans Setup, for the user
    /// to keep or send).
    pub fn text(&self) -> String {
        let mut text = String::from("Oceans Setup: this PC's hardware (ADR-0161)\n\n");
        for line in &self.lines {
            text.push_str(line);
            text.push('\n');
        }
        text
    }
}

/// Judges `functions`: works, works through the firmware, or not, each by
/// what it is for.
pub fn report(functions: &[Function]) -> Report {
    let mut lines = Vec::new();
    let mut sorted: Vec<&Function> = functions
        .iter()
        .filter(|f| assess::kind(f) != Kind::Platform)
        .collect();
    sorted.sort_by_key(|f| (kind_order(assess::kind(f)), f.vendor, f.device));
    for f in sorted {
        let ids = format!("{:04x}:{:04x}", f.vendor, f.device);
        let kind = assess::kind(f).name();
        let line = match assess::verdict(f) {
            Verdict::Works(driver) => format!(
                "  works         {kind}: {} ({ids}), {driver}",
                assess::what(f).unwrap_or("?")
            ),
            Verdict::Firmware(how) => {
                format!("  works (basic) {kind} ({ids}): through the firmware, {how}")
            }
            Verdict::Platform => continue,
            Verdict::Unsupported => format!(
                "  not supported {kind} ({ids}, class {:02x}{:02x}{:02x}): Oceans has no driver",
                f.class, f.subclass, f.prog_if
            ),
        };
        lines.push(line);
    }
    let assessment = assess::assess(functions);
    if lines.is_empty() {
        lines.push("  no PCI devices were listed".into());
    }
    lines.push(String::new());
    match assessment.refusal() {
        Some(why) => lines.push(format!("Oceans cannot be installed here: {why}.")),
        None => lines.push("Oceans can be installed here.".into()),
    }
    for warning in assessment.warnings() {
        lines.push(format!("Note: {warning}."));
    }
    Report { lines, assessment }
}

fn kind_order(kind: Kind) -> u8 {
    match kind {
        Kind::Storage => 0,
        Kind::Network => 1,
        Kind::Wifi => 2,
        Kind::Display => 3,
        Kind::Sound => 4,
        Kind::Usb => 5,
        Kind::Other => 6,
        Kind::Platform => 7,
    }
}

fn hex(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// One function from Windows' compatible IDs for a PCI device, joined by
/// `;` (`PCI\VEN_8086&DEV_1570&REV_21;PCI\VEN_8086&CC_020000;...`): the
/// vendor and device, and the class code from the `CC_` entry of six
/// digits.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn from_windows_ids(line: &str) -> Option<Function> {
    let mut vendor = None;
    let mut device = None;
    let mut class = None;
    for id in line.split(';') {
        let Some(rest) = id.trim().strip_prefix("PCI\\") else {
            continue;
        };
        for part in rest.split('&') {
            if let Some(v) = part.strip_prefix("VEN_") {
                vendor = vendor.or(hex(v).and_then(|v| u16::try_from(v).ok()));
            } else if let Some(d) = part.strip_prefix("DEV_") {
                device = device.or(hex(d).and_then(|d| u16::try_from(d).ok()));
            } else if let Some(c) = part.strip_prefix("CC_")
                && c.len() == 6
            {
                class = class.or(hex(c));
            }
        }
    }
    let (class, subclass, prog_if) = assess::split_class(class?);
    Some(Function {
        vendor: vendor?,
        device: device?,
        class,
        subclass,
        prog_if,
    })
}

/// Lists every present PCI device's compatible IDs, a line each (Windows'
/// PnP; no driver needed).
#[cfg_attr(not(windows), allow(dead_code))]
pub const WINDOWS_PCI_SCRIPT: &str = "$ErrorActionPreference = 'SilentlyContinue'
Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'PCI\\*' } | ForEach-Object {
  ((Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_CompatibleIds').Data) -join ';'
}";

/// One function from Linux's `/sys/bus/pci/devices/*`: its `vendor`,
/// `device` and `class` files (`0x8086`, `0x1570`, `0x020000`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn from_sysfs(vendor: &str, device: &str, class: &str) -> Option<Function> {
    let (class, subclass, prog_if) = assess::split_class(hex(class)?);
    Some(Function {
        vendor: u16::try_from(hex(vendor)?).ok()?,
        device: u16::try_from(hex(device)?).ok()?,
        class,
        subclass,
        prog_if,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_compatible_ids_give_the_ids_and_class() {
        let line = "PCI\\VEN_8086&DEV_1570&REV_21;PCI\\VEN_8086&DEV_1570;\
                    PCI\\VEN_8086&CC_020000;PCI\\VEN_8086&CC_0200;PCI\\VEN_8086;PCI\\CC_020000;PCI\\CC_0200";
        assert_eq!(
            from_windows_ids(line),
            Some(Function {
                vendor: 0x8086,
                device: 0x1570,
                class: 0x02,
                subclass: 0x00,
                prog_if: 0x00
            })
        );
        // Without a class code there is nothing to judge.
        assert_eq!(from_windows_ids("PCI\\VEN_8086&DEV_1570"), None);
        assert_eq!(from_windows_ids(""), None);
    }

    #[test]
    fn linux_sysfs_gives_the_ids_and_class() {
        let wifi = from_sysfs("0x8086\n", "0x24f3\n", "0x028000\n").unwrap();
        assert_eq!(
            (wifi.vendor, wifi.device, wifi.class, wifi.subclass),
            (0x8086, 0x24f3, 2, 0x80)
        );
        assert_eq!(from_sysfs("x", "0x1", "0x0"), None);
    }

    #[test]
    fn the_report_says_what_works_and_the_verdict() {
        let zbook = [
            from_sysfs("0x144d", "0xa802", "0x010802").unwrap(),
            from_sysfs("0x8086", "0x1570", "0x020000").unwrap(),
            from_sysfs("0x8086", "0x24f3", "0x028000").unwrap(),
            from_sysfs("0x8086", "0x1916", "0x030000").unwrap(),
            from_sysfs("0x10ec", "0x522a", "0xff0000").unwrap(),
            from_sysfs("0x8086", "0x9d48", "0x060100").unwrap(),
        ];
        let report = report(&zbook);
        let text = report.text();
        assert!(text.contains("works         storage: NVMe SSD (any vendor) (144d:a802), nvme"));
        assert!(text.contains("works (basic) display (8086:1916)"));
        assert!(text.contains("not supported other (10ec:522a, class ff0000)"));
        // The platform is not listed.
        assert!(!text.contains("9d48"));
        assert!(text.contains("Oceans can be installed here."));
        assert!(text.contains("Note: some parts of this PC will not work"));
        assert_eq!(report.assessment.refusal(), None);
    }
}
