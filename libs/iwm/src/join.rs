//! Joining a network and the data path: the commands of the join
//! (spec section 3), those after association (section 5), and TX_CMD.
//! Layouts are the packed structures of OpenBSD's `iwm(4)` for this
//! firmware's API versions.

use crate::mvm::{Station, action, ids};

/// Command and notification codes (group 0).
pub mod code {
    pub const TX_CMD: u8 = 0x1c;
    pub const MAC_CONTEXT: u8 = 0x28;
    pub const TIME_EVENT: u8 = 0x29;
    pub const TIME_EVENT_NOTIFICATION: u8 = 0x2a;
    pub const BINDING_CONTEXT: u8 = 0x2b;
    pub const TIME_QUOTA: u8 = 0x2c;
    pub const LQ: u8 = 0x4e;
    pub const MAC_PM_POWER_TABLE: u8 = 0xa9;
    pub const MCAST_FILTER: u8 = 0xd0;
    pub const SF_CFG: u8 = 0xd1;
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// A legacy rate (500 kb/s units, as in a Rates element) as the chip's
/// `rate_n_flags` on antenna A: its PLCP value, and the CCK flag for
/// 802.11b rates. `None` for a rate the chip does not have.
pub fn legacy_rate(rate: u8) -> Option<u32> {
    const ANT_A: u32 = 1 << 14;
    const CCK: u32 = 1 << 9;
    let (plcp, cck) = match rate {
        2 => (10, true),
        4 => (20, true),
        11 => (55, true),
        22 => (110, true),
        12 => (13, false),
        18 => (15, false),
        24 => (5, false),
        36 => (7, false),
        48 => (9, false),
        72 => (11, false),
        96 => (1, false),
        108 => (3, false),
        _ => return None,
    };
    Some(plcp | ANT_A | if cck { CCK } else { 0 })
}

/// The rate management frames go at: the network's lowest basic rate, or
/// the band's lowest.
pub fn management_rate(basic_rate: u8, band_24ghz: bool) -> u8 {
    match legacy_rate(basic_rate) {
        Some(_) => basic_rate,
        None if band_24ghz => 2,
        None => 12,
    }
}

/// `LQ_CMD` (88 bytes) for station `sta`: the retry table, fastest first
/// (every OFDM rate, then on 2.4 GHz the CCK ones) down to `lowest`, then
/// `lowest` again to fill its 16 entries.
pub fn link_quality(sta: u8, band_24ghz: bool, lowest: u8) -> [u8; 88] {
    let mut out = [0u8; 88];
    out[0] = sta;
    out[6] = 1; // single stream: antenna A
    out[7] = 3;
    put16(&mut out, 12, 4000);
    out[14] = 3;
    out[15] = 0x3f;
    let lowest = management_rate(lowest, band_24ghz);
    let ofdm = [108u8, 96, 72, 48, 36, 24, 18, 12];
    let cck = [22u8, 11, 4, 2];
    // 802.11b rates only where the network still has them (its lowest
    // basic rate one of them); OFDM rates down to the lowest otherwise.
    let lowest_cck = cck.contains(&lowest);
    let rates = ofdm
        .iter()
        .filter(move |&&rate| lowest_cck || rate >= lowest)
        .chain(
            cck.iter()
                .filter(move |&&rate| lowest_cck && rate >= lowest),
        )
        .copied();
    let pad = legacy_rate(lowest).unwrap_or(0);
    let mut table = rates.filter_map(legacy_rate).chain(core::iter::repeat(pad));
    for i in 0..16 {
        put32(&mut out, 20 + 4 * i, table.next().unwrap_or(pad));
    }
    out
}

/// The association, for the MAC context's station part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Association {
    pub aid: u16,
    pub beacon_interval: u16,
    pub dtim_count: u8,
    pub dtim_period: u8,
    /// The beacon's TSF, and when it arrived (the chip's clock, µs).
    pub tsf: u64,
    pub received: u32,
}

/// What the MAC context says of the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacContext {
    pub action: u32,
    pub us: [u8; 6],
    pub bssid: [u8; 6],
    pub band_24ghz: bool,
    /// Set once associated.
    pub assoc: Option<Association>,
}

fn reciprocal(value: u32) -> u32 {
    u32::MAX.checked_div(value).unwrap_or(0)
}

/// `MAC_CONTEXT_CMD` (148 bytes): context 0 as a client (BSS station).
pub fn mac_context(ctx: &MacContext) -> [u8; 148] {
    let mut out = [0u8; 148];
    put32(&mut out, 4, ctx.action);
    put32(&mut out, 8, 5); // BSS station
    out[16..22].copy_from_slice(&ctx.us);
    out[24..30].copy_from_slice(&ctx.bssid);
    // ACK rates: 1, 2, 5.5, 11 on 2.4 GHz; 6, 12, 24 always.
    put32(&mut out, 32, if ctx.band_24ghz { 0xf } else { 0 });
    put32(&mut out, 36, 0x15);
    put32(&mut out, 48, 0x10); // short slot
    // EDCA per TX FIFO (BK 0, BE 1, VI 2, VO 3): the 802.11 defaults
    // (ECWmin, ECWmax, AIFSN, TXOP in 32 µs).
    let edca: [(u8, u8, u8, u16); 4] = [(4, 10, 7, 0), (4, 10, 3, 0), (3, 4, 2, 94), (2, 3, 2, 47)];
    for (fifo, &(ecw_min, ecw_max, aifsn, txop)) in edca.iter().enumerate() {
        let at = 60 + 8 * fifo;
        put16(&mut out, at, (1 << ecw_min) - 1);
        put16(&mut out, at + 2, (1 << ecw_max) - 1);
        out[at + 4] = aifsn;
        out[at + 5] = 1 << fifo;
        put16(&mut out, at + 6, txop * 32);
    }
    let mut filter = 1 << 2; // accept group frames
    match ctx.assoc {
        Some(a) if a.aid != 0 && a.dtim_period != 0 && a.beacon_interval != 0 => {
            let bi = u32::from(a.beacon_interval);
            let dtim_offset = u32::from(a.dtim_count) * bi * 1024;
            let dtim_interval = bi * u32::from(a.dtim_period);
            put32(&mut out, 100, 1);
            put32(&mut out, 104, a.received.wrapping_add(dtim_offset));
            out[108..116]
                .copy_from_slice(&a.tsf.wrapping_add(u64::from(dtim_offset)).to_le_bytes());
            put32(&mut out, 116, bi);
            put32(&mut out, 120, reciprocal(bi));
            put32(&mut out, 124, dtim_interval);
            put32(&mut out, 128, reciprocal(dtim_interval));
            put32(&mut out, 132, 10);
            put32(&mut out, 136, u32::from(a.aid));
            put32(&mut out, 140, a.received);
        }
        // Beacons pass while the DTIM is unknown.
        _ => filter |= 1 << 6,
    }
    put32(&mut out, 52, filter);
    out
}

/// `BINDING_CONTEXT_CMD` v1 (24 bytes): PHY context 0 bound to MAC 0.
pub fn binding(action: u32) -> [u8; 24] {
    let mut out = [0u8; 24];
    put32(&mut out, 4, action);
    put32(&mut out, 12, u32::MAX);
    put32(&mut out, 16, u32::MAX);
    out
}

/// The status of a command answered with one (binding): 0 is success.
pub fn status_ok(answer: &[u8]) -> bool {
    answer.get(..4) == Some(&[0, 0, 0, 0])
}

/// `TIME_EVENT_CMD` v2 (36 bytes): stay on the channel for `duration` TU,
/// starting within `max_delay` TU, so the join's frames get through.
pub fn time_event(duration: u32, max_delay: u32) -> [u8; 36] {
    let mut out = [0u8; 36];
    put32(&mut out, 4, action::ADD);
    put32(&mut out, 16, max_delay);
    put32(&mut out, 24, 1);
    put32(&mut out, 28, duration);
    out[32] = 1;
    put16(&mut out, 34, 1 | 2 | (1 << 11));
    out
}

/// A `TIME_EVENT_NOTIFICATION`: (action, status); action bit 0 is the
/// start, bit 1 the end.
pub fn time_event_notification(payload: &[u8]) -> Option<(u32, u32)> {
    Some((get32(payload, 16)?, get32(payload, 20)?))
}

/// The access point as station 0, added for the join.
pub fn ap_station(bssid: [u8; 6]) -> Station {
    Station {
        id: ids::AP_STA,
        kind: 0,
        mac_id_and_color: 0,
        address: bssid,
        queues: 0x1e0,
        modify: None,
        assoc_id: 0,
    }
}

/// The same station once associated: its queues and TIDs updated.
pub fn ap_station_associated(aid: u16) -> Station {
    Station {
        modify: Some((1 << 7) | (1 << 1)),
        address: [0; 6],
        assoc_id: aid,
        ..ap_station([0; 6])
    }
}

/// `REPLY_SF_CFG_CMD` (92 bytes) once associated: full on, a legacy
/// station's watermark, the single-BSS timeouts.
pub fn sf_config_full_on() -> [u8; 92] {
    let mut words = [0u32; 23];
    words[0] = 1;
    words[1] = 4096;
    words[2] = 4096;
    for word in &mut words[3..13] {
        *word = 1_000_000;
    }
    // (aging, idle) for single unicast, aggregated, multicast, BA, TX-resp.
    let full = [
        (2016, 320),
        (2016, 320),
        (10016, 2016),
        (2016, 320),
        (2016, 320),
    ];
    for (i, (aging, idle)) in full.into_iter().enumerate() {
        words[13 + 2 * i] = aging;
        words[14 + 2 * i] = idle;
    }
    let mut out = [0u8; 92];
    for (i, word) in words.into_iter().enumerate() {
        put32(&mut out, 4 * i, word);
    }
    out
}

/// `MCAST_FILTER_CMD` (12 bytes): every multicast frame of `bssid` passes.
pub fn mcast_filter(bssid: [u8; 6]) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0] = 1;
    out[3] = 1;
    out[4..10].copy_from_slice(&bssid);
    out
}

/// `MAC_PM_POWER_TABLE` (40 bytes): power save allowed, not used; the
/// keep-alive the firmware needs (3 DTIMs, at least 25 s).
pub fn mac_power(beacon_interval: u16, dtim_period: u8) -> [u8; 40] {
    let dtim_ms = u32::from(dtim_period.max(1)) * u32::from(beacon_interval);
    let keep_alive = (3 * dtim_ms).max(25_000).div_ceil(1000);
    let mut out = [0u8; 40];
    put16(&mut out, 4, 1);
    put16(&mut out, 6, keep_alive as u16);
    out
}

/// `TIME_QUOTA_CMD` v2 (64 bytes): all air time to binding 0.
pub fn time_quota() -> [u8; 64] {
    let mut out = [0u8; 64];
    put32(&mut out, 4, 128);
    for i in 1..4 {
        put32(&mut out, 16 * i, u32::MAX);
    }
    out
}

// ---- Sending frames (TX_CMD) ------------------------------------------------

pub mod tx_flags {
    pub const ACK: u32 = 1 << 3;
    pub const STA_RATE: u32 = 1 << 4;
    pub const BT_DIS: u32 = 1 << 12;
    pub const SEQ_CTL: u32 = 1 << 13;
    pub const MH_PAD: u32 = 1 << 20;
}

/// The TX command (v6), before the 802.11 header copied after it.
pub const TX_CMD_LEN: usize = 56;
/// The scratch field's offset in it.
pub const TX_SCRATCH: usize = 8;

/// What a frame to send is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The whole frame's length (header included, no FCS).
    pub len: u16,
    pub header_len: usize,
    /// A data frame (else management).
    pub data: bool,
    pub unicast: bool,
    /// An association request (it waits longer for the answer).
    pub assoc_request: bool,
    /// The rate for management and group frames (legacy, 500 kb/s).
    pub rate: u8,
}

/// `TX_CMD` for `frame`, to station 0 (the access point). Management and
/// group frames go at `frame.rate` with few retries; unicast data uses the
/// station's rate table. `scratch` is the physical address of the
/// command's scratch field.
pub fn tx_command(frame: &Frame, scratch: u64) -> [u8; TX_CMD_LEN] {
    let mut out = [0u8; TX_CMD_LEN];
    put16(&mut out, 0, frame.len);
    let mut flags = tx_flags::BT_DIS | tx_flags::SEQ_CTL;
    if frame.unicast {
        flags |= tx_flags::ACK;
    }
    if !frame.header_len.is_multiple_of(4) {
        flags |= tx_flags::MH_PAD;
        put16(&mut out, 2, 1 << 13);
    }
    if frame.data && frame.unicast {
        flags |= tx_flags::STA_RATE;
        out[50] = 7;
    } else {
        put32(&mut out, 12, legacy_rate(frame.rate).unwrap_or(0));
        out[50] = 3;
    }
    put32(&mut out, 4, flags);
    out[16] = ids::AP_STA;
    put32(&mut out, 40, u32::MAX);
    put32(&mut out, 44, scratch as u32);
    out[48] = ((scratch >> 32) & 0xf) as u8;
    out[49] = 3;
    if !frame.data {
        out[51] = ids::MGMT_TID;
        put16(&mut out, 52, if frame.assoc_request { 3 } else { 2 });
    }
    out
}

/// A TX response: whether the frame went (status 1 or 2), how often it
/// was retried, and the scheduler's sequence number (slots before it are
/// done).
pub fn tx_response(payload: &[u8]) -> Option<(bool, u8, u16)> {
    let count = usize::from(*payload.first()?).max(1);
    let retries = *payload.get(3)?;
    let status = u16::from_le_bytes(payload.get(36..38)?.try_into().ok()?);
    let ssn = get32(payload, 36 + 4 * count)?;
    Some((
        matches!(status & 0xff, 1 | 2),
        retries,
        (ssn & 0xfff) as u16,
    ))
}

/// A TFD with several buffers (up to 20): `(address, length)` each.
pub fn tfd(buffers: &[(u64, u16)]) -> [u8; crate::cmd::TFD_SIZE] {
    let mut out = [0u8; crate::cmd::TFD_SIZE];
    let n = buffers.len().min(20);
    out[3] = n as u8;
    for (i, &(address, len)) in buffers[..n].iter().enumerate() {
        let at = 4 + 6 * i;
        out[at..at + 4].copy_from_slice(&(address as u32).to_le_bytes());
        let hi = (((address >> 32) & 0xf) as u16) | (len << 4);
        out[at + 4..at + 6].copy_from_slice(&hi.to_le_bytes());
    }
    out
}

/// The scheduler's byte-count entry for a frame of `len` bytes to `sta`
/// (CRC and delimiter added; in dwords when the firmware says so).
pub fn frame_byte_count(sta: u8, len: u16, dwords: bool) -> u16 {
    let len = len + 8;
    let len = if dwords { len.div_ceil(4) } else { len };
    (u16::from(sta) << 12) | len
}
