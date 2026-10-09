//! USB hubs (USB 2.0 §11, USB 3.2 §10): the hub descriptor, port status and
//! the class requests a hub driver needs, and route strings.

use crate::Speed;
use crate::request::Setup;

/// Hub descriptor types.
pub const DESCRIPTOR_USB2: u8 = 0x29;
pub const DESCRIPTOR_USB3: u8 = 0x2a;

/// Port features (USB 2.0 table 11-17, USB 3.2 table 10-9).
pub mod feature {
    pub const PORT_RESET: u16 = 4;
    pub const PORT_POWER: u16 = 8;
    pub const C_PORT_CONNECTION: u16 = 16;
    pub const C_PORT_ENABLE: u16 = 17;
    pub const C_PORT_SUSPEND: u16 = 18;
    pub const C_PORT_OVER_CURRENT: u16 = 19;
    pub const C_PORT_RESET: u16 = 20;
    pub const C_PORT_LINK_STATE: u16 = 25;
    pub const C_PORT_CONFIG_ERROR: u16 = 26;
    pub const C_BH_PORT_RESET: u16 = 29;
}

const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const SET_FEATURE: u8 = 3;
const GET_DESCRIPTOR: u8 = 6;
const SET_HUB_DEPTH: u8 = 12;
/// Class requests to the hub itself, and to one of its ports.
const TO_HUB: u8 = 0x20;
const TO_PORT: u8 = 0x23;
const FROM_HUB: u8 = 0xa0;
const FROM_PORT: u8 = 0xa3;

/// Route strings have five 4-bit tiers; a hub's ports past 15 share 15.
pub const MAX_DEPTH: u8 = 5;

pub fn get_descriptor(usb3: bool) -> Setup {
    let (kind, length) = if usb3 {
        (DESCRIPTOR_USB3, 12)
    } else {
        (DESCRIPTOR_USB2, 71)
    };
    Setup {
        request_type: FROM_HUB,
        request: GET_DESCRIPTOR,
        value: u16::from(kind) << 8,
        index: 0,
        length,
    }
}

pub fn get_port_status(port: u8) -> Setup {
    Setup {
        request_type: FROM_PORT,
        request: GET_STATUS,
        value: 0,
        index: u16::from(port),
        length: 4,
    }
}

pub fn set_port_feature(port: u8, feature: u16) -> Setup {
    Setup {
        request_type: TO_PORT,
        request: SET_FEATURE,
        value: feature,
        index: u16::from(port),
        length: 0,
    }
}

pub fn clear_port_feature(port: u8, feature: u16) -> Setup {
    Setup {
        request_type: TO_PORT,
        request: CLEAR_FEATURE,
        value: feature,
        index: u16::from(port),
        length: 0,
    }
}

/// USB 3 hubs must learn their depth (0 on a root port) before use.
pub fn set_hub_depth(depth: u8) -> Setup {
    Setup {
        request_type: TO_HUB,
        request: SET_HUB_DEPTH,
        value: u16::from(depth),
        index: 0,
        length: 0,
    }
}

/// What the driver needs from a hub descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub ports: u8,
    /// Time from power-on to power good, in milliseconds.
    pub power_good_ms: u16,
    /// USB 2 high-speed hubs: the transaction translator's think time
    /// (0–3, in units of 8 FS bit times, as the slot context wants it).
    pub think_time: u8,
}

impl Descriptor {
    pub fn parse(bytes: &[u8], usb3: bool) -> Option<Self> {
        let expected = if usb3 {
            DESCRIPTOR_USB3
        } else {
            DESCRIPTOR_USB2
        };
        if bytes.len() < 7 || usize::from(bytes[0]) < 7 || bytes[1] != expected {
            return None;
        }
        let characteristics = u16::from_le_bytes([bytes[3], bytes[4]]);
        Some(Self {
            ports: bytes[2],
            power_good_ms: u16::from(bytes[5]) * 2,
            think_time: if usb3 {
                0
            } else {
                ((characteristics >> 5) & 3) as u8
            },
        })
    }
}

/// `wPortStatus` and `wPortChange` of a hub port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortStatus {
    pub status: u16,
    pub change: u16,
}

impl PortStatus {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..4)?;
        Some(Self {
            status: u16::from_le_bytes([bytes[0], bytes[1]]),
            change: u16::from_le_bytes([bytes[2], bytes[3]]),
        })
    }

    pub fn connected(&self) -> bool {
        self.status & 1 != 0
    }

    pub fn enabled(&self) -> bool {
        self.status & 2 != 0
    }

    /// The connection changed since it was last acknowledged (`C_PORT_
    /// CONNECTION`): a device left, came, or both.
    pub fn connection_changed(&self) -> bool {
        self.change & 1 != 0
    }

    pub fn reset_done(&self) -> bool {
        self.change & (1 << 4) != 0
    }

    /// The attached device's speed (USB 3 hubs' ports are SuperSpeed).
    pub fn speed(&self, usb3: bool) -> Speed {
        if usb3 {
            Speed::Super
        } else if self.status & (1 << 9) != 0 {
            Speed::Low
        } else if self.status & (1 << 10) != 0 {
            Speed::High
        } else {
            Speed::Full
        }
    }

    /// The `C_PORT_*` features to clear for every change reported.
    pub fn changes(&self, usb3: bool) -> impl Iterator<Item = u16> + '_ {
        let table: &[(u16, u16)] = if usb3 {
            &[
                (0, feature::C_PORT_CONNECTION),
                (3, feature::C_PORT_OVER_CURRENT),
                (4, feature::C_PORT_RESET),
                (5, feature::C_BH_PORT_RESET),
                (6, feature::C_PORT_LINK_STATE),
                (7, feature::C_PORT_CONFIG_ERROR),
            ]
        } else {
            &[
                (0, feature::C_PORT_CONNECTION),
                (1, feature::C_PORT_ENABLE),
                (2, feature::C_PORT_SUSPEND),
                (3, feature::C_PORT_OVER_CURRENT),
                (4, feature::C_PORT_RESET),
            ]
        };
        table
            .iter()
            .filter(|(bit, _)| self.change & (1 << bit) != 0)
            .map(|&(_, feature)| feature)
    }
}

/// The route string of a device on `port` of a hub that has `route` and
/// sits `depth` tiers below the root (0: on a root port). `None` past the
/// fifth tier.
pub fn child_route(route: u32, depth: u8, port: u8) -> Option<u32> {
    if depth >= MAX_DEPTH || port == 0 {
        return None;
    }
    Some(route | u32::from(port.min(15)) << (4 * depth))
}

/// The ports a hub's status-change bitmap marks (bit 0 is the hub itself).
pub fn changed_ports(bitmap: &[u8], ports: u8) -> u32 {
    let mut out = 0;
    for port in 1..=ports.min(31) {
        let byte = usize::from(port / 8);
        if bitmap.get(byte).is_some_and(|b| b & (1 << (port % 8)) != 0) {
            out |= 1 << port;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    #[test]
    fn parses_hub_descriptors() {
        // QEMU's usb-hub: 8 ports, power good after 50 ms.
        let usb2 = [9, 0x29, 8, 0x29, 0, 25, 0, 0, 0xff];
        assert_eq!(
            Descriptor::parse(&usb2, false),
            Some(Descriptor {
                ports: 8,
                power_good_ms: 50,
                think_time: 1
            })
        );
        assert_eq!(Descriptor::parse(&usb2, true), None);
        let usb3 = [12, 0x2a, 4, 0, 0, 50, 0, 0, 0, 0, 0, 0];
        assert_eq!(Descriptor::parse(&usb3, true).map(|d| d.ports), Some(4));
        assert_eq!(Descriptor::parse(&usb2[..5], false), None);
    }

    #[test]
    fn decodes_port_status() {
        let status = PortStatus::parse(&[0x03, 0x05, 0x11, 0x00]).unwrap();
        assert!(status.connected() && status.enabled() && status.reset_done());
        assert!(status.connection_changed());
        assert!(
            !PortStatus::parse(&[0x03, 0x05, 0x10, 0x00])
                .unwrap()
                .connection_changed()
        );
        assert_eq!(status.speed(false), Speed::High);
        let low = PortStatus::parse(&[0x03, 0x03, 0, 0]).unwrap();
        assert_eq!(low.speed(false), Speed::Low);
        assert_eq!(low.speed(true), Speed::Super);
        let changes: Vec<u16> = status.changes(false).collect();
        assert_eq!(changes, [feature::C_PORT_CONNECTION, feature::C_PORT_RESET]);
        let changes: Vec<u16> = PortStatus {
            status: 0,
            change: 0xe0,
        }
        .changes(true)
        .collect();
        assert_eq!(
            changes,
            [
                feature::C_BH_PORT_RESET,
                feature::C_PORT_LINK_STATE,
                feature::C_PORT_CONFIG_ERROR
            ]
        );
        assert!(PortStatus::parse(&[0, 0, 0]).is_none());
    }

    #[test]
    fn builds_routes() {
        assert_eq!(child_route(0, 0, 2), Some(0x2));
        assert_eq!(child_route(0x2, 1, 3), Some(0x32));
        assert_eq!(child_route(0x2, 1, 20), Some(0xf2));
        assert_eq!(child_route(0x1_1111, 5, 1), None);
        assert_eq!(child_route(0, 0, 0), None);
    }

    #[test]
    fn reads_change_bitmaps() {
        assert_eq!(changed_ports(&[0b0000_0110], 8), 0b110);
        assert_eq!(changed_ports(&[0b0000_0001], 8), 0, "bit 0 is the hub");
        assert_eq!(changed_ports(&[0, 0b1], 8), 1 << 8);
        assert_eq!(changed_ports(&[0, 0b10], 8), 0, "beyond the hub's ports");
    }

    #[test]
    fn encodes_requests() {
        assert_eq!(
            get_descriptor(false).to_bytes(),
            [0xa0, 6, 0, 0x29, 0, 0, 71, 0]
        );
        assert_eq!(get_port_status(3).to_bytes(), [0xa3, 0, 0, 0, 3, 0, 4, 0]);
        assert_eq!(
            set_port_feature(2, feature::PORT_RESET).to_bytes(),
            [0x23, 3, 4, 0, 2, 0, 0, 0]
        );
        assert_eq!(
            clear_port_feature(1, feature::C_PORT_CONNECTION).to_bytes(),
            [0x23, 1, 16, 0, 1, 0, 0, 0]
        );
        assert_eq!(set_hub_depth(1).to_bytes(), [0x20, 12, 1, 0, 0, 0, 0, 0]);
    }
}
