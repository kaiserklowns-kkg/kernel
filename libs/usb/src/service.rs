//! The USB service's protocol (endpoint `usb`, ADR-0032): what the xHCI
//! driver tells clients such as `lsusb`.

/// `LIST`: data = `[skip u8]` (optional) → up to [`MAX_RECORDS`] device
/// records, after the first `skip`; fewer means the end.
pub const LIST: u64 = 1;

/// Reply labels.
pub const OK: u64 = 0;
pub const BAD_REQUEST: u64 = 1;

pub const RECORD_SIZE: usize = 40;
/// Records per reply (an IPC message carries 256 bytes).
pub const MAX_RECORDS: usize = 6;
pub const MAX_NAME: usize = 28;

/// What the driver does with a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Enumerated; no driver for it.
    Other,
    /// A boot keyboard feeding the console.
    Keyboard,
    Mouse,
    Storage,
    Hub,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Self::Other => 0,
            Self::Keyboard => 1,
            Self::Mouse => 2,
            Self::Storage => 3,
            Self::Hub => 4,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Keyboard,
            2 => Self::Mouse,
            3 => Self::Storage,
            4 => Self::Hub,
            _ => Self::Other,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Other => "no driver",
            Self::Keyboard => "keyboard (console input)",
            Self::Mouse => "mouse (no driver)",
            Self::Storage => "mass storage (no driver)",
            Self::Hub => "hub",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    /// The root port the device is reached through.
    pub port: u8,
    /// Hub ports below it, 4 bits per tier ([`crate::hub::child_route`]).
    pub route: u32,
    /// xHCI speed ID ([`crate::Speed::from_id`]).
    pub speed: u8,
    pub kind: Kind,
    pub vendor: u16,
    pub product: u16,
    /// The product string (printable ASCII), `name_len` bytes of it.
    pub name: [u8; MAX_NAME],
    pub name_len: u8,
}

impl Record {
    pub fn encode(&self, out: &mut [u8; RECORD_SIZE]) {
        out[0] = self.port;
        out[1] = self.speed;
        out[2] = self.kind.code();
        out[3] = self.name_len.min(MAX_NAME as u8);
        out[4..6].copy_from_slice(&self.vendor.to_le_bytes());
        out[6..8].copy_from_slice(&self.product.to_le_bytes());
        out[8..12].copy_from_slice(&self.route.to_le_bytes());
        out[12..].copy_from_slice(&self.name);
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RECORD_SIZE)?;
        let mut name = [0u8; MAX_NAME];
        name.copy_from_slice(&bytes[12..]);
        Some(Self {
            port: bytes[0],
            route: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            speed: bytes[1],
            kind: Kind::from_code(bytes[2]),
            name_len: bytes[3].min(MAX_NAME as u8),
            vendor: u16::from_le_bytes([bytes[4], bytes[5]]),
            product: u16::from_le_bytes([bytes[6], bytes[7]]),
            name,
        })
    }

    /// Where the device is: `ROOT[.HUBPORT]...`, e.g. `6.2`.
    pub fn path(&self) -> Path {
        Path {
            port: self.port,
            route: self.route,
        }
    }

    pub fn name(&self) -> &str {
        let name = &self.name[..usize::from(self.name_len)];
        // Printable ASCII by construction; anything else shows as empty.
        core::str::from_utf8(name).unwrap_or("")
    }
}

/// A device's place in the tree, displayed as `ROOT[.PORT]...`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Path {
    pub port: u8,
    pub route: u32,
}

impl core::fmt::Display for Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.port)?;
        for tier in 0..crate::hub::MAX_DEPTH {
            match (self.route >> (4 * tier)) & 0xf {
                0 => break,
                port => write!(f, ".{port}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn records_round_trip() {
        let mut name = [0u8; MAX_NAME];
        name[..18].copy_from_slice(b"QEMU USB Keyboard!");
        let record = Record {
            port: 5,
            route: 0x32,
            speed: 1,
            kind: Kind::Keyboard,
            vendor: 0x0627,
            product: 0x0001,
            name,
            name_len: 17,
        };
        let mut bytes = [0u8; RECORD_SIZE];
        record.encode(&mut bytes);
        let decoded = Record::decode(&bytes).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(decoded.name(), "QEMU USB Keyboard");
        assert_eq!(std::format!("{}", decoded.path()), "5.2.3");
        assert!(Record::decode(&bytes[..39]).is_none());
        bytes[3] = 200;
        assert_eq!(Record::decode(&bytes).unwrap().name_len, MAX_NAME as u8);
        assert!(MAX_RECORDS * RECORD_SIZE <= 248);
    }
}
