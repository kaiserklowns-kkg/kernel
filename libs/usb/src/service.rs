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
///
/// Two kinds can be claimed: mass storage ([`crate::storage`]: class 8,
/// SCSI, Bulk-Only) and pointers ([`POINTER`], ADR-0042; the buffer may
/// then be as small as [`MIN_POINTER_BUFFER`]).
pub const CLAIM: u64 = 2;
/// On a session: data = `[endpoint address u8][0; 3][offset u32][len
/// u32]`: a bulk transfer of at most [`MAX_TRANSFER`] bytes between the
/// buffer at `offset` and the endpoint, IN or OUT by the address →
/// `[transferred u32]`.
pub const BULK: u64 = 3;
/// On a session: data = `[setup; 8][offset u32]`: a class request to the
/// claimed interface, with an IN data stage (into the buffer) or none →
/// `[transferred u32]`. On a pointer's session, also the standard request
/// for its report descriptor
/// ([`crate::request::Setup::hid_get_report_descriptor`]).
pub const CONTROL: u64 = 4;
/// On a session: data = `[endpoint address u8]`: clears a halt on the
/// device and in the controller.
pub const CLEAR_HALT: u64 = 5;
/// On a pointer's session (ADR-0042): data = `[]` → `[lost u8]` and the
/// reports received since the last call, oldest first, as many as fit
/// ([`ReportWriter`], [`Reports`]); `lost` counts reports dropped because
/// the driver's queue was full. The first call starts polling the
/// interrupt endpoint (send `SET_PROTOCOL` and `SET_IDLE` before it). The
/// claim's notification is signalled when reports arrive.
pub const REPORTS: u64 = 6;

/// The pseudo interface kind for `CLAIM` that names a pointer: a HID
/// interface the driver identified as a mouse or a tablet (a boot mouse,
/// or a report descriptor with a mouse or pointer collection). HID has no
/// subclass or protocol `0xff`.
pub const POINTER: (u8, u8, u8) = (crate::descriptor::CLASS_HID, 0xff, 0xff);

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
/// A pointer's buffer only receives its report descriptor.
pub const MIN_POINTER_BUFFER: usize = 4096;
/// Largest `REPORTS` reply.
pub const MAX_REPORTS_REPLY: usize = 240;

/// What a claim hands over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Claim {
    pub interface: u8,
    /// Mass storage: the bulk endpoints (0 for a pointer).
    pub bulk_in: u8,
    pub bulk_out: u8,
    pub port: u8,
    pub route: u32,
    pub vendor: u16,
    pub product: u16,
    /// A pointer: its interrupt IN endpoint (0 for mass storage).
    pub interrupt_in: u8,
    /// The interface's subclass and protocol (a boot mouse is 1 and 2).
    pub subclass: u8,
    pub protocol: u8,
    /// A pointer: the length of its report descriptor.
    pub report_length: u16,
}

impl Claim {
    pub const SIZE: usize = 20;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0] = self.interface;
        out[1] = self.bulk_in;
        out[2] = self.bulk_out;
        out[3] = self.port;
        out[4..8].copy_from_slice(&self.route.to_le_bytes());
        out[8..10].copy_from_slice(&self.vendor.to_le_bytes());
        out[10..12].copy_from_slice(&self.product.to_le_bytes());
        out[12] = self.interrupt_in;
        out[13] = self.subclass;
        out[14] = self.protocol;
        out[16..18].copy_from_slice(&self.report_length.to_le_bytes());
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
            interrupt_in: bytes[12],
            subclass: bytes[13],
            protocol: bytes[14],
            report_length: u16::from_le_bytes([bytes[16], bytes[17]]),
        })
    }

    /// A boot protocol mouse (HID 1.11 §4.2–4.3).
    pub fn is_boot_mouse(&self) -> bool {
        self.subclass == crate::descriptor::HID_SUBCLASS_BOOT
            && self.protocol == crate::descriptor::HID_PROTOCOL_MOUSE
    }

    pub fn path(&self) -> Path {
        Path {
            port: self.port,
            route: self.route,
        }
    }
}

/// One report in a `REPORTS` reply: `[len u8][time_ms u64][bytes; len]`.
pub const REPORT_HEADER: usize = 9;

/// Builds a `REPORTS` reply in `out`: `[lost u8]`, then reports.
pub struct ReportWriter<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> ReportWriter<'a> {
    /// `out` must hold at least the `lost` byte.
    pub fn new(out: &'a mut [u8], lost: u8) -> Self {
        out[0] = lost;
        Self { out, len: 1 }
    }

    /// Appends a report received at `time_ms`; `false` (and nothing
    /// written) when it does not fit or is longer than 255 bytes.
    pub fn push(&mut self, time_ms: u64, report: &[u8]) -> bool {
        let end = self.len + REPORT_HEADER + report.len();
        if report.len() > usize::from(u8::MAX) || end > self.out.len() {
            return false;
        }
        let at = self.len;
        self.out[at] = report.len() as u8;
        self.out[at + 1..at + REPORT_HEADER].copy_from_slice(&time_ms.to_le_bytes());
        self.out[at + REPORT_HEADER..end].copy_from_slice(report);
        self.len = end;
        true
    }

    /// The reply's length so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// No reports yet.
    pub fn is_empty(&self) -> bool {
        self.len == 1
    }
}

/// Reads a `REPORTS` reply: `lost`, then `(time_ms, report)` pairs. A
/// truncated entry ends the iteration.
pub struct Reports<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reports<'a> {
    pub fn new(reply: &'a [u8]) -> Self {
        Self {
            bytes: reply,
            at: 1,
        }
    }

    pub fn lost(&self) -> u8 {
        self.bytes.first().copied().unwrap_or(0)
    }
}

impl<'a> Iterator for Reports<'a> {
    type Item = (u64, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let header = self.bytes.get(self.at..self.at + REPORT_HEADER)?;
        let len = usize::from(header[0]);
        let time = u64::from_le_bytes(header[1..].try_into().ok()?);
        let start = self.at + REPORT_HEADER;
        let report = self.bytes.get(start..start + len)?;
        self.at = start + len;
        Some((time, report))
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
    /// A pointer with relative motion (ADR-0042).
    Mouse,
    Storage,
    Hub,
    /// A pointer with absolute positions (ADR-0042).
    Tablet,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Self::Other => 0,
            Self::Keyboard => 1,
            Self::Mouse => 2,
            Self::Storage => 3,
            Self::Hub => 4,
            Self::Tablet => 5,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Keyboard,
            2 => Self::Mouse,
            3 => Self::Storage,
            4 => Self::Hub,
            5 => Self::Tablet,
            _ => Self::Other,
        }
    }

    /// What the device is, as the driver found it.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Other => "no driver",
            Self::Keyboard => "keyboard (console input)",
            Self::Mouse => "mouse",
            Self::Storage => "mass storage",
            Self::Hub => "hub",
            Self::Tablet => "tablet",
        }
    }

    /// Whether the input class driver claims devices of this kind.
    pub fn is_pointer(self) -> bool {
        matches!(self, Self::Mouse | Self::Tablet)
    }
}

/// [`Record`] kind byte: the device is claimed by a class driver.
const CLAIMED: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    /// The root port the device is reached through.
    pub port: u8,
    /// Hub ports below it, 4 bits per tier ([`crate::hub::child_route`]).
    pub route: u32,
    /// xHCI speed ID ([`crate::Speed::from_id`]).
    pub speed: u8,
    pub kind: Kind,
    /// A class driver holds the device (ADR-0034).
    pub claimed: bool,
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
        out[2] = self.kind.code() | if self.claimed { CLAIMED } else { 0 };
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
            kind: Kind::from_code(bytes[2] & !CLAIMED),
            claimed: bytes[2] & CLAIMED != 0,
            name_len: bytes[3].min(MAX_NAME as u8),
            vendor: u16::from_le_bytes([bytes[4], bytes[5]]),
            product: u16::from_le_bytes([bytes[6], bytes[7]]),
            name,
        })
    }

    /// What the device is and what drives it, for `lsusb`.
    pub fn role(&self) -> &'static str {
        match (self.kind, self.claimed) {
            (Kind::Mouse, true) => "mouse (pointer input)",
            (Kind::Mouse, false) => "mouse (no driver)",
            (Kind::Tablet, true) => "tablet (pointer input)",
            (Kind::Tablet, false) => "tablet (no driver)",
            (kind, _) => kind.describe(),
        }
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
            claimed: false,
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
    fn records_say_who_drives_a_pointer() {
        let mut record = Record {
            port: 6,
            route: 1,
            speed: 1,
            kind: Kind::Tablet,
            claimed: true,
            vendor: 0x0627,
            product: 0x0001,
            name: [0; MAX_NAME],
            name_len: 0,
        };
        let mut bytes = [0u8; RECORD_SIZE];
        record.encode(&mut bytes);
        assert_eq!(Record::decode(&bytes), Some(record));
        assert_eq!(record.role(), "tablet (pointer input)");
        record.claimed = false;
        assert_eq!(record.role(), "tablet (no driver)");
        record.kind = Kind::Mouse;
        assert_eq!(record.role(), "mouse (no driver)");
        record.encode(&mut bytes);
        assert_eq!(Record::decode(&bytes), Some(record));
        record.kind = Kind::Storage;
        record.claimed = true;
        assert_eq!(record.role(), "mass storage");
        assert!(Kind::Mouse.is_pointer() && !Kind::Keyboard.is_pointer());
    }

    #[test]
    fn report_replies_round_trip() {
        let mut out = [0u8; 30];
        let mut writer = ReportWriter::new(&mut out, 3);
        assert!(writer.is_empty());
        assert!(writer.push(10, &[1, 2, 3, 4]));
        assert!(writer.push(20, &[]));
        // 1 + 13 + 9 = 23 used; 7 bytes left: no room for a header.
        assert!(!writer.push(30, &[5]));
        let len = writer.len();
        assert_eq!(len, 23);
        let reports = Reports::new(&out[..len]);
        assert_eq!(reports.lost(), 3);
        let all: std::vec::Vec<_> = reports.collect();
        assert_eq!(all, [(10, &[1u8, 2, 3, 4][..]), (20, &[][..])]);
        // A truncated entry ends the walk.
        assert_eq!(Reports::new(&out[..len - 9]).count(), 1);
        assert_eq!(Reports::new(&[]).lost(), 0);
        let mut long = [0u8; 300];
        assert!(!ReportWriter::new(&mut long, 0).push(0, &[0; 256]));
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
            ..Claim::default()
        };
        assert_eq!(Claim::decode(&claim.encode()), Some(claim));
        assert_eq!(std::format!("{}", claim.path()), "7.1.2");
        assert!(Claim::decode(&[0; Claim::SIZE - 1]).is_none());
        let mouse = Claim {
            interface: 0,
            interrupt_in: 0x81,
            subclass: 1,
            protocol: 2,
            report_length: 0x134,
            ..Claim::default()
        };
        assert_eq!(Claim::decode(&mouse.encode()), Some(mouse));
        assert!(mouse.is_boot_mouse());
        assert!(!claim.is_boot_mouse());
    }
}
