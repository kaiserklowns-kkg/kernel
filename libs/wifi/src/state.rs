//! The Wi-Fi files in `/system/wifi` (ADR-0130): how the driver and the
//! desktop's Wi-Fi panel talk, one line per fact, plain enough for the
//! shell (`cat /system/wifi/status`).
//!
//! The driver writes `status` (what it is doing) and `networks` (the last
//! scan); the desktop (or the shell) writes `ssid` and `password` (the
//! network to join; `ssid` removed: none) and `scan` (asks for a scan; the
//! driver removes it). Network names are hex in the driver's files: an
//! SSID is any 32 bytes, spaces and newlines included.

use crate::{Bss, MAX_SSID, Security};

pub const STATUS: &str = "status";
pub const NETWORKS: &str = "networks";
pub const SSID: &str = "ssid";
pub const PASSWORD: &str = "password";
pub const SCAN: &str = "scan";

/// The longest line either file holds.
pub const MAX_LINE: usize = 160;

/// A network name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ssid {
    bytes: [u8; MAX_SSID],
    len: u8,
}

impl Ssid {
    pub fn new(ssid: &[u8]) -> Option<Self> {
        let mut bytes = [0u8; MAX_SSID];
        bytes.get_mut(..ssid.len())?.copy_from_slice(ssid);
        Some(Self {
            bytes,
            len: ssid.len() as u8,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    fn hex(text: &[u8]) -> Option<Self> {
        if !text.len().is_multiple_of(2) || text.len() > 2 * MAX_SSID {
            return None;
        }
        let mut bytes = [0u8; MAX_SSID];
        for (i, pair) in text.chunks(2).enumerate() {
            bytes[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Some(Self {
            bytes,
            len: (text.len() / 2) as u8,
        })
    }
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Writes into a byte buffer, failing (not truncating) when it is full.
struct Line<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Line<'_> {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        self.out
            .get_mut(self.len..self.len + bytes.len())?
            .copy_from_slice(bytes);
        self.len += bytes.len();
        Some(())
    }

    fn hex(&mut self, bytes: &[u8]) -> Option<()> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        for &b in bytes {
            self.put(&[DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0xf)]])?;
        }
        Some(())
    }

    fn number(&mut self, value: i32) -> Option<()> {
        if value < 0 {
            self.put(b"-")?;
        }
        let mut digits = [0u8; 10];
        let mut n = value.unsigned_abs();
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.put(&digits[at..])
    }
}

fn number(text: &[u8]) -> Option<i32> {
    let (negative, digits) = match text.split_first()? {
        (b'-', rest) => (true, rest),
        _ => (false, text),
    };
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    let mut value = 0i32;
    for &c in digits {
        value = value * 10 + i32::from(digit(c).filter(|d| *d < 10)?);
    }
    Some(if negative { -value } else { value })
}

/// The non-empty lines of a file.
pub fn lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty())
}

// ---- networks ---------------------------------------------------------------

/// A network as the panel lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub ssid: Ssid,
    pub security: Security,
    pub rssi: i8,
    pub channel: u8,
}

/// `networks`' line for `bss`: `W -48 6 6f6365616e73` (security O, W or
/// U; signal, dBm; channel; the name in hex), with its newline.
pub fn network_line(bss: &Bss, out: &mut [u8]) -> Option<usize> {
    let mut line = Line { out, len: 0 };
    line.put(match bss.security {
        Security::Open => b"O ",
        Security::Wpa2Personal => b"W ",
        Security::Unsupported => b"U ",
    })?;
    line.number(i32::from(bss.rssi))?;
    line.put(b" ")?;
    line.number(i32::from(bss.channel))?;
    line.put(b" ")?;
    line.hex(bss.ssid())?;
    line.put(b"\n")?;
    Some(line.len)
}

pub fn parse_network(line: &[u8]) -> Option<Listed> {
    let mut fields = line.split(|&b| b == b' ');
    let security = match fields.next()? {
        b"O" => Security::Open,
        b"W" => Security::Wpa2Personal,
        b"U" => Security::Unsupported,
        _ => return None,
    };
    let rssi = i8::try_from(number(fields.next()?)?).ok()?;
    let channel = u8::try_from(number(fields.next()?)?).ok()?;
    let ssid = Ssid::hex(fields.next()?)?;
    if fields.next().is_some() || ssid.len == 0 {
        return None;
    }
    Some(Listed {
        ssid,
        security,
        rssi,
        channel,
    })
}

// ---- status -----------------------------------------------------------------

/// What the driver is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status<'a> {
    /// No Wi-Fi (the chip did not come up).
    Off,
    /// No network set.
    Idle,
    /// Scanning for networks to list.
    Scanning,
    Joining(Ssid),
    Joined {
        ssid: Ssid,
        rssi: i8,
        channel: u8,
    },
    /// The last attempt failed, and why (one line of text).
    Failed {
        ssid: Ssid,
        reason: &'a str,
    },
}

/// `status`' line for `status`, with its newline.
pub fn status_line(status: &Status<'_>, out: &mut [u8]) -> Option<usize> {
    let mut line = Line { out, len: 0 };
    match status {
        Status::Off => line.put(b"off")?,
        Status::Idle => line.put(b"idle")?,
        Status::Scanning => line.put(b"scanning")?,
        Status::Joining(ssid) => {
            line.put(b"joining ")?;
            line.hex(ssid.as_bytes())?;
        }
        Status::Joined {
            ssid,
            rssi,
            channel,
        } => {
            line.put(b"joined ")?;
            line.hex(ssid.as_bytes())?;
            line.put(b" ")?;
            line.number(i32::from(*rssi))?;
            line.put(b" ")?;
            line.number(i32::from(*channel))?;
        }
        Status::Failed { ssid, reason } => {
            line.put(b"failed ")?;
            line.hex(ssid.as_bytes())?;
            line.put(b" ")?;
            // One line, printable: anything else becomes a space.
            for &b in reason.as_bytes().iter().take(MAX_LINE - 80) {
                line.put(&[if (0x20..0x7f).contains(&b) { b } else { b' ' }])?;
            }
        }
    }
    line.put(b"\n")?;
    Some(line.len)
}

pub fn parse_status(text: &[u8]) -> Option<Status<'_>> {
    let line = lines(text).next()?;
    let (word, rest) = match line.iter().position(|&b| b == b' ') {
        Some(at) => (&line[..at], &line[at + 1..]),
        None => (line, &[][..]),
    };
    let mut fields = rest.splitn(2, |&b| b == b' ');
    Some(match word {
        b"off" => Status::Off,
        b"idle" => Status::Idle,
        b"scanning" => Status::Scanning,
        b"joining" => Status::Joining(Ssid::hex(fields.next()?)?),
        b"joined" => {
            let ssid = Ssid::hex(fields.next()?)?;
            let mut numbers = fields.next()?.split(|&b| b == b' ');
            let rssi = i8::try_from(number(numbers.next()?)?).ok()?;
            let channel = u8::try_from(number(numbers.next()?)?).ok()?;
            Status::Joined {
                ssid,
                rssi,
                channel,
            }
        }
        b"failed" => Status::Failed {
            ssid: Ssid::hex(fields.next()?)?,
            reason: core::str::from_utf8(fields.next().unwrap_or(&[])).ok()?,
        },
        _ => return None,
    })
}
