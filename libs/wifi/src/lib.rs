//! 802.11 frames (IEEE 802.11-2020 §9), the parts a client needs, whatever
//! the chip (ADR-0129):
//!
//! - [`Bss`]: a network, read from a beacon or a probe response: its SSID,
//!   channel, signal, and whether it offers WPA2-Personal with CCMP.
//! - The client's management frames: probe, authentication (open system),
//!   association requests; the access point's answers, read.
//! - Data frames to and from Ethernet II (the stack's frames): 802.11
//!   header and LLC/SNAP on the air, destination/source/EtherType at home.
//!
//! Frames come from the air: untrusted. Every element is length-checked;
//! anything malformed is dropped, never guessed at.
//!
//! - [`state`]: the files the driver and the desktop share (ADR-0130).

#![cfg_attr(not(test), no_std)]

pub mod state;

pub type MacAddress = [u8; 6];
pub const BROADCAST: MacAddress = [0xff; 6];

/// Frame control, first byte: type and subtype.
pub mod kind {
    pub const ASSOC_REQUEST: u8 = 0x00;
    pub const ASSOC_RESPONSE: u8 = 0x10;
    pub const PROBE_REQUEST: u8 = 0x40;
    pub const PROBE_RESPONSE: u8 = 0x50;
    pub const BEACON: u8 = 0x80;
    pub const DISASSOC: u8 = 0xa0;
    pub const AUTH: u8 = 0xb0;
    pub const DEAUTH: u8 = 0xc0;
    pub const DATA: u8 = 0x08;
    pub const QOS_DATA: u8 = 0x88;
}

/// Frame control, second byte.
pub mod flags {
    pub const TO_DS: u8 = 0x01;
    pub const FROM_DS: u8 = 0x02;
    pub const PROTECTED: u8 = 0x40;
}

/// Element IDs used.
mod element {
    pub const SSID: u8 = 0;
    pub const RATES: u8 = 1;
    pub const DS_PARAMETER: u8 = 3;
    pub const TIM: u8 = 5;
    pub const RSN: u8 = 48;
    pub const EXTENDED_RATES: u8 = 50;
}

/// A management frame's header: 24 bytes.
pub const MGMT_HEADER: usize = 24;
pub const MAX_SSID: usize = 32;

/// The elements of a management frame body, one at a time.
pub fn elements(mut body: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    core::iter::from_fn(move || {
        let (&id, rest) = body.split_first()?;
        let (&len, rest) = rest.split_first()?;
        let value = rest.get(..usize::from(len))?;
        body = &rest[usize::from(len)..];
        Some((id, value))
    })
}

/// What WPA2-Personal with CCMP needs of an RSN element: CCMP as the
/// group and a pairwise cipher, PSK among the AKMs.
pub fn rsn_offers_wpa2_psk(rsn: &[u8]) -> bool {
    const CCMP: [u8; 4] = [0x00, 0x0f, 0xac, 4];
    const PSK: [u8; 4] = [0x00, 0x0f, 0xac, 2];
    let suites = |at: usize| -> Option<(usize, &[u8])> {
        let count = usize::from(u16::from_le_bytes([*rsn.get(at)?, *rsn.get(at + 1)?]));
        let list = rsn.get(at + 2..at + 2 + 4 * count)?;
        Some((at + 2 + 4 * count, list))
    };
    if rsn.get(..2) != Some(&[1, 0]) || rsn.get(2..6) != Some(&CCMP) {
        return false;
    }
    let Some((after, pairwise)) = suites(6) else {
        return false;
    };
    let Some((_, akms)) = suites(after) else {
        return false;
    };
    pairwise.as_chunks::<4>().0.contains(&CCMP) && akms.as_chunks::<4>().0.contains(&PSK)
}

/// The security a network offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    /// WPA2-Personal, CCMP: what Oceans joins.
    Wpa2Personal,
    /// Something else (WEP, TKIP only, enterprise, WPA3-only): not joined.
    Unsupported,
}

/// A network, from a beacon or probe response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bss {
    pub bssid: MacAddress,
    pub ssid: [u8; MAX_SSID],
    pub ssid_len: u8,
    pub channel: u8,
    pub security: Security,
    pub beacon_interval: u16,
    pub capability: u16,
    /// Signal strength, dBm, as the chip reported it.
    pub rssi: i8,
    /// The access point's clock when it sent the frame (µs).
    pub tsf: u64,
    /// From a beacon's TIM: beacons to the next DTIM, and between DTIMs
    /// (0 if unknown, as in a probe response).
    pub dtim_count: u8,
    pub dtim_period: u8,
    /// The lowest basic rate (500 kb/s units; 0 if none is marked): what
    /// management frames are sent at.
    pub basic_rate: u8,
}

impl Bss {
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_len)]
    }

    /// From a beacon or probe response (the whole 802.11 frame, without
    /// FCS), heard on `channel` with `rssi`.
    pub fn parse(frame: &[u8], channel: u8, rssi: i8) -> Option<Self> {
        let kind = *frame.first()?;
        if kind != kind::BEACON && kind != kind::PROBE_RESPONSE {
            return None;
        }
        let body = frame.get(MGMT_HEADER..)?;
        // Timestamp 8, beacon interval 2, capability 2, then elements.
        let fixed = body.get(..12)?;
        let mut bss = Self {
            bssid: frame.get(16..22)?.try_into().ok()?,
            ssid: [0; MAX_SSID],
            ssid_len: 0,
            channel,
            security: Security::Open,
            beacon_interval: u16::from_le_bytes([fixed[8], fixed[9]]),
            capability: u16::from_le_bytes([fixed[10], fixed[11]]),
            rssi,
            tsf: u64::from_le_bytes(fixed[..8].try_into().ok()?),
            dtim_count: 0,
            dtim_period: 0,
            basic_rate: 0,
        };
        let privacy = bss.capability & (1 << 4) != 0;
        let mut rsn = None;
        let mut ssid_seen = false;
        for (id, value) in elements(&body[12..]) {
            match id {
                element::SSID if !ssid_seen => {
                    if value.len() > MAX_SSID {
                        return None;
                    }
                    bss.ssid[..value.len()].copy_from_slice(value);
                    bss.ssid_len = value.len() as u8;
                    ssid_seen = true;
                }
                element::DS_PARAMETER if value.len() == 1 => bss.channel = value[0],
                element::TIM if value.len() >= 2 => {
                    bss.dtim_count = value[0];
                    bss.dtim_period = value[1];
                }
                element::RATES | element::EXTENDED_RATES => {
                    for &rate in value.iter().filter(|&&r| r & 0x80 != 0) {
                        let rate = rate & 0x7f;
                        if bss.basic_rate == 0 || rate < bss.basic_rate {
                            bss.basic_rate = rate;
                        }
                    }
                }
                element::RSN => rsn = Some(value),
                _ => {}
            }
        }
        // Every beacon has an SSID element (empty for a hidden network).
        if !ssid_seen {
            return None;
        }
        bss.security = match (privacy, rsn) {
            (false, None) => Security::Open,
            (true, Some(rsn)) if rsn_offers_wpa2_psk(rsn) => Security::Wpa2Personal,
            _ => Security::Unsupported,
        };
        Some(bss)
    }
}

/// The rates every client offers: 802.11b/g basic and extended, in 500
/// kb/s units (1, 2, 5.5, 11 basic; 6 ... 54).
const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
const EXTENDED_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];
/// On 5 GHz: OFDM only.
const RATES_5GHZ: [u8; 8] = [0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c];

fn header(out: &mut [u8], kind: u8, to: &MacAddress, from: &MacAddress, bssid: &MacAddress) {
    out[..MGMT_HEADER].fill(0);
    out[0] = kind;
    out[4..10].copy_from_slice(to);
    out[10..16].copy_from_slice(from);
    out[16..22].copy_from_slice(bssid);
}

struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        self.out
            .get_mut(self.len..self.len + bytes.len())?
            .copy_from_slice(bytes);
        self.len += bytes.len();
        Some(())
    }

    fn element(&mut self, id: u8, value: &[u8]) -> Option<()> {
        self.put(&[id, u8::try_from(value.len()).ok()?])?;
        self.put(value)
    }

    fn rates(&mut self, channel: u8) -> Option<()> {
        if channel > 14 {
            self.element(element::RATES, &RATES_5GHZ)
        } else {
            self.element(element::RATES, &RATES)?;
            self.element(element::EXTENDED_RATES, &EXTENDED_RATES)
        }
    }
}

/// A probe request for `ssid` (empty: any), broadcast. Its length.
pub fn probe_request(out: &mut [u8], us: &MacAddress, ssid: &[u8], channel: u8) -> Option<usize> {
    if out.len() < MGMT_HEADER || ssid.len() > MAX_SSID {
        return None;
    }
    header(out, kind::PROBE_REQUEST, &BROADCAST, us, &BROADCAST);
    let mut w = Writer {
        out,
        len: MGMT_HEADER,
    };
    w.element(element::SSID, ssid)?;
    w.rates(channel)?;
    Some(w.len)
}

/// An open-system authentication request (sequence 1).
pub fn auth_request(out: &mut [u8], us: &MacAddress, bssid: &MacAddress) -> Option<usize> {
    if out.len() < MGMT_HEADER + 6 {
        return None;
    }
    header(out, kind::AUTH, bssid, us, bssid);
    // Algorithm 0 (open), sequence 1, status 0.
    out[MGMT_HEADER..MGMT_HEADER + 6].copy_from_slice(&[0, 0, 1, 0, 0, 0]);
    Some(MGMT_HEADER + 6)
}

/// An association request to `bss`, with `rsn` (the client's RSN element,
/// whole) for a protected network.
pub fn assoc_request(
    out: &mut [u8],
    us: &MacAddress,
    bss: &Bss,
    rsn: Option<&[u8]>,
) -> Option<usize> {
    if out.len() < MGMT_HEADER + 4 {
        return None;
    }
    header(out, kind::ASSOC_REQUEST, &bss.bssid, us, &bss.bssid);
    // Capability: ESS, privacy if protected, short slot time; listen
    // interval 10 beacons.
    let mut capability = 0x0001 | 0x0400;
    if rsn.is_some() {
        capability |= 0x0010;
    }
    let mut w = Writer {
        out,
        len: MGMT_HEADER,
    };
    w.put(&u16::to_le_bytes(capability))?;
    w.put(&10u16.to_le_bytes())?;
    w.element(element::SSID, bss.ssid())?;
    w.rates(bss.channel)?;
    if let Some(rsn) = rsn {
        w.put(rsn)?;
    }
    Some(w.len)
}

/// The access point's answer to authentication or association.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Authentication accepted.
    Authenticated,
    /// Association accepted, with this association ID.
    Associated(u16),
    /// Refused, with this status code.
    Refused(u16),
    /// Deauthenticated or disassociated, with this reason.
    Dropped(u16),
}

/// Reads the access point `bssid`'s answer in `frame`, if it is one, to us.
pub fn answer(frame: &[u8], us: &MacAddress, bssid: &MacAddress) -> Option<Answer> {
    if frame.get(4..10)? != us || frame.get(16..22)? != bssid {
        return None;
    }
    let body = frame.get(MGMT_HEADER..)?;
    let at =
        |i: usize| -> Option<u16> { Some(u16::from_le_bytes([*body.get(i)?, *body.get(i + 1)?])) };
    match frame[0] {
        kind::AUTH => {
            // Open system, sequence 2.
            if at(0)? != 0 || at(2)? != 2 {
                return None;
            }
            Some(match at(4)? {
                0 => Answer::Authenticated,
                status => Answer::Refused(status),
            })
        }
        kind::ASSOC_RESPONSE => Some(match at(2)? {
            0 => Answer::Associated(at(4)? & 0x3fff),
            status => Answer::Refused(status),
        }),
        kind::DEAUTH | kind::DISASSOC => Some(Answer::Dropped(at(0)?)),
        _ => None,
    }
}

/// LLC/SNAP before the EtherType on the air.
const SNAP: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];
/// A data frame's header to the access point (ToDS): 24 bytes.
pub const DATA_HEADER: usize = 24;

/// An Ethernet II frame (`dst src type payload`) as a data frame to the
/// access point `bssid`, sequence number `seq`: header, LLC/SNAP,
/// EtherType, payload. `protected` sets the bit for frames the chip
/// encrypts.
pub fn from_ethernet(
    ethernet: &[u8],
    bssid: &MacAddress,
    seq: u16,
    protected: bool,
    out: &mut [u8],
) -> Option<usize> {
    if ethernet.len() < 14 {
        return None;
    }
    let len = DATA_HEADER + SNAP.len() + ethernet.len() - 12;
    let out = out.get_mut(..len)?;
    out.fill(0);
    out[0] = kind::DATA;
    out[1] = flags::TO_DS | if protected { flags::PROTECTED } else { 0 };
    out[4..10].copy_from_slice(bssid);
    // Source: us; destination: the Ethernet destination.
    out[10..16].copy_from_slice(&ethernet[6..12]);
    out[16..22].copy_from_slice(&ethernet[..6]);
    out[22..24].copy_from_slice(&(seq << 4).to_le_bytes());
    out[DATA_HEADER..DATA_HEADER + 6].copy_from_slice(&SNAP);
    out[DATA_HEADER + 6..].copy_from_slice(&ethernet[12..]);
    Some(len)
}

/// A data frame the access point `bssid` sent (FromDS, it the
/// transmitter): its MAC header's length (24, or 26 with QoS) and whether
/// it is protected. `None` for any other frame.
pub fn from_access_point(frame: &[u8], bssid: &MacAddress) -> Option<(usize, bool)> {
    let kind = *frame.first()?;
    let fc1 = *frame.get(1)?;
    if kind & 0x0c != 0x08 || fc1 & 0x03 != flags::FROM_DS || frame.get(10..16)? != bssid {
        return None;
    }
    let header = if kind & 0x80 != 0 {
        DATA_HEADER + 2
    } else {
        DATA_HEADER
    };
    (frame.len() >= header).then_some((header, fc1 & flags::PROTECTED != 0))
}

/// A data frame from the access point (FromDS, already decrypted, the
/// protected bit as it came) as an Ethernet II frame. `None` for anything
/// else (null data, other directions, non-SNAP payloads).
pub fn to_ethernet(frame: &[u8], out: &mut [u8]) -> Option<usize> {
    let kind = *frame.first()?;
    let fc1 = *frame.get(1)?;
    if kind & 0x0c != 0x08 || kind & 0x40 != 0 || fc1 & 0x03 != flags::FROM_DS {
        return None;
    }
    let header = if kind & 0x80 != 0 {
        DATA_HEADER + 2
    } else {
        DATA_HEADER
    };
    let body = frame.get(header..)?;
    if body.get(..6)? != SNAP {
        return None;
    }
    let payload = &body[6..];
    if payload.len() < 2 {
        return None;
    }
    let len = 12 + payload.len();
    let out = out.get_mut(..len)?;
    // Destination: address 1; source: address 3.
    out[..6].copy_from_slice(&frame[4..10]);
    out[6..12].copy_from_slice(&frame[16..22]);
    out[12..].copy_from_slice(payload);
    Some(len)
}

#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tests;
