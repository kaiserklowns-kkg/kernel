//! What a PC's hardware means for Oceans, before it is installed
//! (ADR-0161): each PCI function sorted into what it is for and whether
//! Oceans drives it, and the verdict Oceans Setup gives. Linux's lesson
//! (ADR-0146, P8): most bad first days come from the wrong machine, so say
//! so before anything is erased.
//!
//! The same [`MATRIX`](crate::MATRIX) as `sysreport` and the published
//! compatibility matrix: one table, so they cannot disagree.

use crate::{Support, lookup};

/// A PCI function as a PC's own system lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Function {
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

/// What a function is for, by its PCI class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Storage,
    /// Wired networking.
    Network,
    Wifi,
    Display,
    Sound,
    Usb,
    /// Bridges, the host bridge, and the rest of the chipset: nothing a
    /// user notices.
    Platform,
    Other,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Network => "wired network",
            Self::Wifi => "Wi-Fi",
            Self::Display => "display",
            Self::Sound => "sound",
            Self::Usb => "USB",
            Self::Platform => "platform",
            Self::Other => "other",
        }
    }
}

/// How Oceans gets on with a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Driven by Oceans: the driver, and its ADR.
    Works(&'static str),
    /// Works through the firmware, without a driver of its own (the
    /// display: no acceleration, the firmware's resolution).
    Firmware(&'static str),
    /// Part of the platform: nothing to drive.
    Platform,
    /// Oceans has no driver: it will not work.
    Unsupported,
}

/// The kind of `f`, by its class (and, for networking, its subclass).
pub fn kind(f: &Function) -> Kind {
    match (f.class, f.subclass) {
        (0x01, _) => Kind::Storage,
        // "Other network controller": what Wi-Fi cards say they are.
        (0x02, 0x80) | (0x0d, _) => Kind::Wifi,
        (0x02, _) => Kind::Network,
        (0x03, _) => Kind::Display,
        (0x04, 0x01 | 0x03) => Kind::Sound,
        (0x0c, 0x03) => Kind::Usb,
        (0x05 | 0x06 | 0x08 | 0x11, _) | (0x0c, 0x05) => Kind::Platform,
        _ => Kind::Other,
    }
}

/// What Oceans does with `f`.
pub fn verdict(f: &Function) -> Verdict {
    match lookup(f.vendor, f.device, f.class, f.subclass, f.prog_if).map(|row| row.support) {
        Some(Support::Driver(driver)) => Verdict::Works(driver),
        Some(Support::Firmware(how)) => Verdict::Firmware(how),
        Some(Support::Platform) => Verdict::Platform,
        Some(Support::None) | None if kind(f) == Kind::Platform => Verdict::Platform,
        Some(Support::None) | None => Verdict::Unsupported,
    }
}

/// What the row says the function is, if the table knows it.
pub fn what(f: &Function) -> Option<&'static str> {
    lookup(f.vendor, f.device, f.class, f.subclass, f.prog_if).map(|row| row.what)
}

/// The machine as a whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Assessment {
    /// A storage controller Oceans drives: there is somewhere to install.
    pub storage: bool,
    /// A wired network Oceans drives.
    pub wired: bool,
    /// Wi-Fi Oceans drives.
    pub wifi: bool,
    /// Functions a user would notice (not the platform) that will not work.
    pub unsupported: usize,
}

impl Assessment {
    /// Why Oceans must not be installed here, if it must not: only when it
    /// would have nowhere to live.
    pub fn refusal(&self) -> Option<&'static str> {
        (!self.storage).then_some(
            "Oceans has no driver for this PC's storage controller: it could not start from the disk",
        )
    }

    /// What the user should know before going on, if anything.
    pub fn warnings(&self) -> impl Iterator<Item = &'static str> {
        let network = (!self.wired && !self.wifi).then_some(
            "Oceans has no driver for this PC's network: no internet, and no updates but from a USB stick",
        );
        let wifi_only = (!self.wired && self.wifi).then_some(
            "only Wi-Fi works here, and Oceans' Wi-Fi is new (ADR-0129): keep a wired network or a USB stick at hand",
        );
        let others = (self.unsupported > 0)
            .then_some("some parts of this PC will not work in Oceans (listed above)");
        [network, wifi_only, others].into_iter().flatten()
    }
}

/// The verdict on a whole machine.
pub fn assess(functions: &[Function]) -> Assessment {
    let mut assessment = Assessment::default();
    for f in functions {
        let works = matches!(verdict(f), Verdict::Works(_));
        match kind(f) {
            Kind::Storage => assessment.storage |= works,
            Kind::Network => assessment.wired |= works,
            Kind::Wifi => assessment.wifi |= works,
            _ => {}
        }
        if verdict(f) == Verdict::Unsupported {
            assessment.unsupported += 1;
        }
    }
    assessment
}

/// A PCI class code as Windows and Linux give it, `0xCCSSPP` (class,
/// subclass, programming interface).
pub fn split_class(code: u32) -> (u8, u8, u8) {
    ((code >> 16) as u8, (code >> 8) as u8, code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(vendor: u16, device: u16, class: u32) -> Function {
        let (class, subclass, prog_if) = split_class(class);
        Function {
            vendor,
            device,
            class,
            subclass,
            prog_if,
        }
    }

    /// The HP ZBook 15u G3 Oceans runs on (lspci there, 2026-10-13).
    fn zbook() -> [Function; 9] {
        [
            f(0x8086, 0x1904, 0x060000), // host bridge
            f(0x8086, 0x1916, 0x030000), // HD Graphics 520
            f(0x8086, 0x9d2f, 0x0c0330), // xHCI
            f(0x8086, 0x9d70, 0x040300), // HD Audio
            f(0x8086, 0x9d48, 0x060100), // LPC
            f(0x8086, 0x9d23, 0x0c0500), // SMBus
            f(0x8086, 0x1570, 0x020000), // I219-V
            f(0x8086, 0x24f3, 0x028000), // Wireless 8260
            f(0x144d, 0xa802, 0x010802), // Samsung NVMe
        ]
    }

    #[test]
    fn the_zbook_has_everything_it_needs() {
        let machine = zbook();
        let a = assess(&machine);
        assert!(a.storage && a.wired && a.wifi);
        assert_eq!(a.unsupported, 0);
        assert_eq!(a.refusal(), None);
        assert_eq!(a.warnings().count(), 0);
        assert_eq!(kind(&machine[7]), Kind::Wifi);
        assert!(matches!(verdict(&machine[7]), Verdict::Works(d) if d.starts_with("iwm")));
        assert!(matches!(verdict(&machine[1]), Verdict::Firmware(_)));
        assert_eq!(verdict(&machine[5]), Verdict::Platform);
    }

    #[test]
    fn qemu_q35_is_the_reference() {
        let machine = [
            f(0x8086, 0x29c0, 0x060000),
            f(0x1234, 0x1111, 0x030000),
            f(0x1af4, 0x1041, 0x020000),
            f(0x1af4, 0x1042, 0x010000),
            f(0x8086, 0x2922, 0x010601),
            f(0x1b36, 0x000d, 0x0c0330),
        ];
        let a = assess(&machine);
        assert!(a.storage && a.wired && !a.wifi);
        assert_eq!(a.unsupported, 0);
        // Wired only: nothing to warn about.
        assert_eq!(a.warnings().count(), 0);
    }

    #[test]
    fn a_laptop_oceans_cannot_use_is_told_why() {
        let machine = [
            f(0x8086, 0x1904, 0x060000),
            // A RAID-mode storage controller (Intel RST): no driver.
            f(0x8086, 0x282a, 0x010400),
            // Broadcom Wi-Fi: no driver.
            f(0x14e4, 0x43a0, 0x028000),
            // Realtek card reader: not something Oceans drives.
            f(0x10ec, 0x522a, 0xff0000),
        ];
        let a = assess(&machine);
        assert!(!a.storage && !a.wired && !a.wifi);
        assert_eq!(a.unsupported, 3);
        assert!(a.refusal().is_some());
        let warnings: [&str; 2] = {
            let mut w = a.warnings();
            [w.next().unwrap(), w.next().unwrap()]
        };
        assert!(warnings[0].contains("network"));
        assert!(warnings[1].contains("will not work"));
    }

    #[test]
    fn wifi_alone_is_a_warning_not_a_refusal() {
        let machine = [f(0x144d, 0xa802, 0x010802), f(0x8086, 0x24f3, 0x028000)];
        let a = assess(&machine);
        assert_eq!(a.refusal(), None);
        assert!(a.warnings().any(|w| w.contains("only Wi-Fi")));
    }
}
