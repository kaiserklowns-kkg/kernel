//! Control requests: 8-byte setup packets (USB 2.0 §9.3, HID 1.11 §7.2).

/// `bmRequestType` direction bit: device to host.
pub const DEVICE_TO_HOST: u8 = 0x80;
const STANDARD: u8 = 0x00;
const CLASS: u8 = 0x20;
const TO_DEVICE: u8 = 0x00;
const TO_INTERFACE: u8 = 0x01;

const GET_DESCRIPTOR: u8 = 6;
const SET_CONFIGURATION: u8 = 9;
const HID_SET_IDLE: u8 = 0x0a;
const HID_SET_PROTOCOL: u8 = 0x0b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    /// `GET_DESCRIPTOR` of `kind`/`index` (`language` for strings).
    pub const fn get_descriptor(kind: u8, index: u8, language: u16, length: u16) -> Self {
        Self {
            request_type: DEVICE_TO_HOST | STANDARD | TO_DEVICE,
            request: GET_DESCRIPTOR,
            value: (kind as u16) << 8 | index as u16,
            index: language,
            length,
        }
    }

    pub const fn set_configuration(value: u8) -> Self {
        Self {
            request_type: STANDARD | TO_DEVICE,
            request: SET_CONFIGURATION,
            value: value as u16,
            index: 0,
            length: 0,
        }
    }

    /// HID `SET_PROTOCOL`: 0 selects the boot protocol.
    pub const fn hid_set_boot_protocol(interface: u8) -> Self {
        Self {
            request_type: CLASS | TO_INTERFACE,
            request: HID_SET_PROTOCOL,
            value: 0,
            index: interface as u16,
            length: 0,
        }
    }

    /// HID `SET_IDLE` with duration 0: report only on changes.
    pub const fn hid_set_idle(interface: u8) -> Self {
        Self {
            request_type: CLASS | TO_INTERFACE,
            request: HID_SET_IDLE,
            value: 0,
            index: interface as u16,
            length: 0,
        }
    }

    pub fn is_in(&self) -> bool {
        self.request_type & DEVICE_TO_HOST != 0
    }

    pub fn to_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0] = self.request_type;
        out[1] = self.request;
        out[2..4].copy_from_slice(&self.value.to_le_bytes());
        out[4..6].copy_from_slice(&self.index.to_le_bytes());
        out[6..8].copy_from_slice(&self.length.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor;

    #[test]
    fn encodes_setup_packets() {
        assert_eq!(
            Setup::get_descriptor(descriptor::DEVICE, 0, 0, 18).to_bytes(),
            [0x80, 6, 0, 1, 0, 0, 18, 0]
        );
        assert_eq!(
            Setup::get_descriptor(descriptor::STRING, 2, 0x0409, 255).to_bytes(),
            [0x80, 6, 2, 3, 0x09, 0x04, 255, 0]
        );
        assert_eq!(
            Setup::set_configuration(1).to_bytes(),
            [0, 9, 1, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            Setup::hid_set_boot_protocol(2).to_bytes(),
            [0x21, 0x0b, 0, 0, 2, 0, 0, 0]
        );
        assert_eq!(
            Setup::hid_set_idle(0).to_bytes(),
            [0x21, 0x0a, 0, 0, 0, 0, 0, 0]
        );
        assert!(Setup::get_descriptor(1, 0, 0, 8).is_in());
        assert!(!Setup::set_configuration(1).is_in());
    }
}
