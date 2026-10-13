//! The firmware's commands after ALIVE (the "MVM" interface), scanning, and
//! received frames: what the driver sends and reads once the REGULAR image
//! runs. Layouts are the packed structures of OpenBSD's `iwm(4)` for this
//! firmware's API versions (spec: SPEC-mvm.md, sections 1, 2 and 4).

use alloc::vec::Vec;

/// Command and notification codes (group 0 unless said).
pub mod code {
    pub const PHY_CONTEXT: u8 = 0x08;
    pub const ADD_STA: u8 = 0x18;
    pub const SCD_QUEUE_CFG: u8 = 0x1d;
    pub const POWER_TABLE: u8 = 0x77;
    pub const MCC_UPDATE: u8 = 0xc8;
    pub const BEACON_FILTERING: u8 = 0xd2;
    /// Group 4 (PHY operations).
    pub const TEMP_REPORTING_THRESHOLDS: u8 = 0x04;
    pub const PHY_OPS_GROUP: u8 = 4;
    /// Group 1 (long).
    pub const SCAN_CFG: u8 = 0x0c;
    pub const SCAN_REQ_UMAC: u8 = 0x0d;
    pub const SCAN_ABORT_UMAC: u8 = 0x0e;

    /// Notifications.
    pub const SCAN_COMPLETE_UMAC: u8 = 0x0f;
    pub const SCAN_ITERATION_COMPLETE_UMAC: u8 = 0xb5;
    pub const RX_PHY: u8 = 0xc0;
    pub const RX_MPDU: u8 = 0xc1;
}

/// Station and queue numbers in client mode (DQA).
pub mod ids {
    pub const AP_STA: u8 = 0;
    pub const AUX_STA: u8 = 1;
    pub const AUX_QUEUE: u8 = 1;
    /// Management and data, best effort (5); 6..8 for the other ACs.
    pub const DATA_QUEUE: u8 = 5;
    /// TX FIFOs: per AC (BE, BK, VI, VO), and multicast.
    pub const AC_FIFOS: [u8; 4] = [1, 0, 2, 3];
    pub const MCAST_FIFO: u8 = 5;
    /// The TID management frames use.
    pub const MGMT_TID: u8 = 8;
    pub const MAC_INDEX_AUX: u32 = 4;
    pub const PHY_CONTEXTS: u32 = 3;
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// `SCD_QUEUE_CFG`: TX queue `queue` for station `sta`, `tid`, on `fifo`,
/// enabled, not aggregated, starting at `ssn`.
pub fn scd_queue_cfg(sta: u8, queue: u8, tid: u8, fifo: u8, ssn: u16) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[1] = sta;
    out[2] = tid;
    out[3] = queue;
    out[4] = 1;
    out[6] = fifo;
    out[7] = 64;
    put16(&mut out, 8, ssn);
    out
}

/// A station for `ADD_STA` (v10, 48 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Station {
    pub id: u8,
    /// 0 link (the access point), 4 auxiliary (scanning).
    pub kind: u8,
    pub mac_id_and_color: u32,
    pub address: [u8; 6],
    pub queues: u32,
    /// Modify an existing station (with `modify_mask`), not add.
    pub modify: Option<u8>,
    pub assoc_id: u16,
}

impl Station {
    /// The auxiliary station scans use, on queue 1.
    pub const AUX: Self = Self {
        id: ids::AUX_STA,
        kind: 4,
        mac_id_and_color: ids::MAC_INDEX_AUX,
        address: [0; 6],
        queues: 1 << ids::AUX_QUEUE,
        modify: None,
        assoc_id: 0,
    };

    pub fn command(&self) -> [u8; 48] {
        let mut out = [0u8; 48];
        out[0] = u8::from(self.modify.is_some());
        // No aggregation on any TID.
        put16(&mut out, 2, 0xffff);
        put32(&mut out, 4, self.mac_id_and_color);
        out[8..14].copy_from_slice(&self.address);
        out[16] = self.id;
        out[17] = self.modify.unwrap_or(0);
        if self.kind == 0 && self.modify.is_none() {
            // Legacy rates only: 20 MHz, no MIMO (the mask says so).
            put32(&mut out, 24, (3 << 26) | (3 << 28));
        }
        out[35] = self.kind;
        put16(&mut out, 36, self.assoc_id);
        put32(&mut out, 40, self.queues);
        out
    }
}

/// The status of an `ADD_STA` answer: success is 1 in the low byte.
pub fn add_sta_ok(answer: &[u8]) -> bool {
    answer.first() == Some(&1)
}

/// `PHY_CONTEXT_CMD` (v1, 36 bytes): context `id`, `action` (1 add,
/// 2 modify), on `channel` at 20 MHz, one chain idle and one active.
pub fn phy_context(id: u32, action: u32, channel: u8, tx_ant: u8, rx_ant: u8) -> [u8; 36] {
    let mut out = [0u8; 36];
    put32(&mut out, 0, id);
    put32(&mut out, 4, action);
    out[16] = u8::from(channel <= 14);
    out[17] = channel;
    put32(&mut out, 20, u32::from(tx_ant));
    put32(
        &mut out,
        24,
        (u32::from(rx_ant) << 1) | (1 << 10) | (1 << 12),
    );
    out
}

pub mod action {
    pub const ADD: u32 = 1;
    pub const MODIFY: u32 = 2;
    pub const REMOVE: u32 = 3;
}

/// `TEMP_REPORTING_THRESHOLDS`, empty: the firmware handles CT-kill.
pub const TEMP_THRESHOLDS_NONE: [u8; 20] = [0; 20];
/// The device `POWER_TABLE`: power save allowed (the radio stays awake).
pub const POWER_TABLE_DEVICE: [u8; 4] = [1, 0, 0, 0];
/// `BEACON_FILTERING`, all zeros: off.
pub const BEACON_FILTER_OFF: [u8; 44] = [0; 44];

/// `MCC_UPDATE` (v2, 28 bytes) for country `code` ("ZZ": the NVM's own
/// profile), from the "current" source.
pub fn mcc_update(code: [u8; 2]) -> [u8; 28] {
    let mut out = [0u8; 28];
    put16(&mut out, 0, u16::from(code[0]) << 8 | u16::from(code[1]));
    out[2] = 0x10;
    out
}

/// The country an `MCC_UPDATE` answer settled on (v3: status, mcc, ...).
pub fn mcc_answer(answer: &[u8]) -> Option<(u32, [u8; 2])> {
    let status = u32::from_le_bytes(answer.get(..4)?.try_into().ok()?);
    let mcc = u16::from_le_bytes(answer.get(4..6)?.try_into().ok()?);
    Some((status, [(mcc >> 8) as u8, mcc as u8]))
}

// ---- Scanning -------------------------------------------------------------

/// Every rate, CCK and OFDM, as `SCAN_CFG` wants them (supported and
/// basic).
const SCAN_RATES: u32 = 0xfff;

/// `SCAN_CFG_CMD`: 36 bytes and one byte per scan channel (`slots`, the
/// firmware's `n_scan_channels`), listing `channels`.
pub fn scan_config(tx_ant: u8, rx_ant: u8, mac: [u8; 6], channels: &[u8], slots: usize) -> Vec<u8> {
    let mut out = alloc::vec![0u8; 36 + slots];
    let n = channels.len().min(slots);
    let flags = (1 << 0) // activate
        | (1 << 3) // allow CHUB requests
        | (1 << 8) | (1 << 9) // TX and RX chains
        | (1 << 10) // the auxiliary station
        | (1 << 11) // all times
        | (1 << 13) // channel flags
        | (1 << 14) // legacy rates
        | (1 << 15) // MAC address
        | (1 << 17) // clear fragmented
        | ((n as u32) << 26);
    put32(&mut out, 0, flags);
    put32(&mut out, 4, u32::from(tx_ant));
    put32(&mut out, 8, u32::from(rx_ant));
    put32(&mut out, 12, SCAN_RATES | SCAN_RATES << 16);
    out[24..28].copy_from_slice(&[10, 110, 44, 90]);
    out[28..34].copy_from_slice(&mac);
    out[34] = ids::AUX_STA;
    out[36..36 + n].copy_from_slice(&channels[..n]);
    out
}

/// The probe request template's buffer.
const PROBE_BUF: usize = 512;
/// Rates elements: 802.11g (basic 1, 2, 5.5, 11; then 6 ... 54) and
/// 802.11a (basic 6, 12, 24).
const RATES_G: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
const XRATES_G: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];
const RATES_A: [u8; 8] = [0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c];

/// The probe request template (v1: four segments, then the frame): the
/// header with an empty SSID element (the firmware fills it in), the
/// 2.4 GHz part, the 5 GHz part, nothing common (no HT).
fn probe_template(mac: [u8; 6], band_5ghz: bool) -> [u8; 16 + PROBE_BUF] {
    let mut out = [0u8; 16 + PROBE_BUF];
    let buf = 16;
    out[buf] = 0x40;
    out[buf + 4..buf + 10].fill(0xff);
    out[buf + 10..buf + 16].copy_from_slice(&mac);
    out[buf + 16..buf + 22].fill(0xff);
    // SSID element, empty.
    let mut at = 24 + 2;
    put16(&mut out, 0, 0);
    put16(&mut out, 2, at as u16);
    let element = |out: &mut [u8], at: &mut usize, id: u8, value: &[u8]| {
        out[buf + *at] = id;
        out[buf + *at + 1] = value.len() as u8;
        out[buf + *at + 2..buf + *at + 2 + value.len()].copy_from_slice(value);
        *at += 2 + value.len();
    };
    let start = at;
    element(&mut out, &mut at, 1, &RATES_G);
    element(&mut out, &mut at, 50, &XRATES_G);
    // DS parameter (the firmware writes the channel).
    element(&mut out, &mut at, 3, &[0]);
    put16(&mut out, 4, start as u16);
    put16(&mut out, 6, (at - start) as u16);
    if band_5ghz {
        let start = at;
        element(&mut out, &mut at, 1, &RATES_A);
        put16(&mut out, 8, start as u16);
        put16(&mut out, 10, (at - start) as u16);
    }
    put16(&mut out, 12, at as u16);
    put16(&mut out, 14, 0);
    out
}

/// The head of `SCAN_REQ_UMAC` v7, and the room for each channel.
const SCAN_HEAD: usize = 48;
const SCAN_CHANNEL: usize = 8;
/// The tail (v1): schedule, delay, probe template, direct SSIDs.
const SCAN_TAIL: usize = 12 + 16 + PROBE_BUF + 20 * 34;

/// `SCAN_REQ_UMAC` (v7, tail v1): one pass over `channels`, passive unless
/// `ssid` is given (then probing for it). `slots` is the firmware's
/// `n_scan_channels`: the channel array always takes that much room.
pub fn scan_request(
    channels: &[u8],
    ssid: Option<&[u8]>,
    mac: [u8; 6],
    band_5ghz: bool,
    slots: usize,
) -> Vec<u8> {
    let mut out = alloc::vec![0u8; SCAN_HEAD + SCAN_CHANNEL * slots + SCAN_TAIL];
    let n = channels.len().min(slots);
    let ssid = ssid.filter(|s| !s.is_empty() && s.len() <= 32);
    put32(&mut out, 8, 2); // ooc priority: high
    let mut general = (1 << 2) | (1 << 5) | (1 << 13); // pass all, iteration complete, adaptive dwell
    general |= if ssid.is_some() { 1 << 4 } else { 1 << 3 }; // pre-connect / passive
    put16(&mut out, 12, general);
    out[16..21].copy_from_slice(&[10, 110, 44, 2, 10]);
    put16(&mut out, 22, if ssid.is_some() { 100 } else { 300 });
    put32(&mut out, 40, 2); // scan priority: high
    out[45] = n as u8;
    for (i, &channel) in channels[..n].iter().enumerate() {
        let at = SCAN_HEAD + SCAN_CHANNEL * i;
        put32(&mut out, at, u32::from(ssid.is_some()));
        out[at + 4] = channel;
        out[at + 5] = 1;
    }
    let tail = SCAN_HEAD + SCAN_CHANNEL * slots;
    // One iteration.
    out[tail + 2] = 1;
    let template = probe_template(mac, band_5ghz);
    out[tail + 12..tail + 12 + template.len()].copy_from_slice(&template);
    if let Some(ssid) = ssid {
        let direct = tail + 12 + template.len();
        out[direct] = 0;
        out[direct + 1] = ssid.len() as u8;
        out[direct + 2..direct + 2 + ssid.len()].copy_from_slice(ssid);
    }
    out
}

/// `SCAN_COMPLETE_UMAC`: the status (0 done, 1 aborted, ...).
pub fn scan_complete(payload: &[u8]) -> Option<u8> {
    payload.get(6).copied()
}

/// `SCAN_ITERATION_COMPLETE_UMAC`: channels scanned.
pub fn scan_iteration(payload: &[u8]) -> Option<u8> {
    payload.get(4).copied()
}

// ---- Received frames (legacy RX path) ------------------------------------

/// What `RX_PHY` says of the frame that follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phy {
    pub channel: u8,
    /// dBm, the strongest antenna.
    pub rssi: i8,
    /// The chip's clock (µs) and the TSF when it arrived.
    pub system_time: u32,
    pub tsf: u64,
}

pub fn rx_phy(payload: &[u8]) -> Option<Phy> {
    if payload.len() < 68 || payload[1] > 20 {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes(payload[i..i + 4].try_into().unwrap());
    let energy = u32_at(28);
    let rssi = (0..3)
        .map(|i| (energy >> (8 * i)) & 0xff)
        .map(|e| if e == 0 { -256 } else { -(e as i32) })
        .max()
        .unwrap_or(-256);
    let channel = u16::from_le_bytes([payload[22], payload[23]]);
    Some(Phy {
        channel: u8::try_from(channel).ok()?,
        rssi: rssi.max(-128) as i8,
        system_time: u32_at(4),
        tsf: u64::from_le_bytes(payload[8..16].try_into().unwrap()),
    })
}

/// `RX_MPDU`'s status bits.
pub mod rx_status {
    pub const CRC_OK: u32 = 1 << 0;
    pub const OVERRUN_OK: u32 = 1 << 1;
}

/// `RX_MPDU`: the 802.11 frame (no FCS), if it arrived whole.
pub fn rx_mpdu(payload: &[u8]) -> Option<&[u8]> {
    let len = usize::from(u16::from_le_bytes(payload.get(..2)?.try_into().ok()?));
    let frame = payload.get(4..4 + len)?;
    let status = u32::from_le_bytes(payload.get(4 + len..8 + len)?.try_into().ok()?);
    let whole = rx_status::CRC_OK | rx_status::OVERRUN_OK;
    (status & whole == whole && len >= 10).then_some(frame)
}
