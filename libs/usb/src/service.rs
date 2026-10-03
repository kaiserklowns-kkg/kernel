//! The USB service's protocol (endpoint `usb`, ADR-0032): what the xHCI
//! driver tells clients such as `lsusb`.

/// `LIST`: data = `[skip u8]` (optional) → up to [`MAX_RECORDS`] device
/// records, after the first `skip`; fewer means the end.
pub const LIST: u64 = 1;

/// Class drivers (ADR-0034). On the `usb` endpoint: data = `[class u8]
/// [subclass u8][protocol u8][0 u8][bits u64]`, handles = a memory object
/// of [`MIN_BUFFER`] to [`MAX_BUFFER`] bytes (`READ`, `WRITE`, `MAP`,
/// `TRANSFER`) and a notification (`SIGNAL`, `TRANSFER`) → a session
/// handle and a [`Claim`]. The first unclaimed interface of that kind is
/// configured and handed over. The notification is signalled with
/// `bits` when such an interface appears (after [`NOT_FOUND`]) and when
/// the claimed device goes away.
pub const CLAIM: u64 = 2;
/// On a session: data = `[endpoint address u8][0; 3][offset u32][len
/// u32]`: a bulk transfer of at most [`MAX_TRANSFER`] bytes between the
/// buffer at `offset` and the endpoint, IN or OUT by the address →
/// `[transferred u32]`.
pub const BULK: u64 = 3;
/// On a session: data = `[setup; 8][offset u32]`: a class request to the
/// claimed interface, with an IN data stage (into the buffer) or none →
/// `[transferred u32]`.
pub const CONTROL: u64 = 4;
/// On a session: data = `[endpoint address u8]`: clears a halt on the
/// device and in the controller.
pub const CLEAR_HALT: u64 = 5;

/// Reply labels.
pub const OK: u64 = 0;
pub const BAD_REQUEST: u64 = 1;
/// No such interface now; the notification will say when one appears.
pub const NOT_FOUND: u64 = 2;
/// The endpoint stalled (clear the halt).
pub const STALL: u64 = 3;
pub const IO_ERROR: u64 = 4;
/// The device was unplugged; the session is dead.
pub const GONE: u64 = 5;

/// Largest bulk transfer per request.
pub const MAX_TRANSFER: usize = 64 * 1024;
pub const MIN_BUFFER: usize = MAX_TRANSFER;
pub const MAX_BUFFER: usize = 1024 * 1024;

/// What a claim hands over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Claim {
    pub interface: u8,
    pub bulk_in: u8,
    pub bulk_out: u8,
    pub port: u8,
    pub route: u32,
    pub vendor: u16,
    pub product: u16,
}

impl Claim {
    pub const SIZE: usize = 12;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0] = self.interface;
        out[1] = self.bulk_in;
        out[2] = self.bulk_out;
        out[3] = self.port;
        out[4..8].copy_from_slice(&self.route.to_le_bytes());
        out[8..10].copy_from_slice(&self.vendor.to_le_bytes());
        out[10..12].copy_from_slice(&self.product.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..Self::SIZE)?;
        Some(Self {
            interface: bytes[0],
            bulk_in: bytes[1],
            bulk_out: bytes[2],
            port: bytes[3],
            route: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            vendor: u16::from_le_bytes([bytes[8], bytes[9]]),
            product: u16::from_le_bytes([bytes[10], bytes[11]]),
        })
    }

    pub fn path(&self) -> Path {
        Path {
            port: self.port,
            route: self.route,
        }
    }
}

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
            Self::Storage => "mass storage",
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

    #[test]
    fn claims_round_trip() {
        let claim = Claim {
            interface: 1,
            bulk_in: 0x81,
            bulk_out: 0x02,
            port: 7,
            route: 0x21,
            vendor: 0x46f4,
            product: 0x0001,
        };
        assert_eq!(Claim::decode(&claim.encode()), Some(claim));
        assert_eq!(std::format!("{}", claim.path()), "7.1.2");
        assert!(Claim::decode(&[0; 11]).is_none());
    }
}
