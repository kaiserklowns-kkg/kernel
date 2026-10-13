//! EAPOL-Key frames (IEEE 802.1X-2010 §11.3, 802.11-2020 §12.7.2): the
//! frames of the handshakes, inside 802.1X, inside data frames of
//! EtherType 0x888e.

use crate::hmac_sha1;

/// The EtherType that carries 802.1X.
pub const ETHERTYPE: u16 = 0x888e;
/// 802.1X: version 2, type 3 (EAPOL-Key).
const VERSION: u8 = 2;
const TYPE_KEY: u8 = 3;
/// The RSN key descriptor.
const DESCRIPTOR_RSN: u8 = 2;
/// Bytes before the key data: 802.1X header (4) and the descriptor (95).
pub const HEADER: usize = 4 + 95;
/// Where the MIC is, in the frame.
const MIC_AT: usize = 4 + 77;

/// Key information bits (§12.7.2).
pub mod info {
    /// Descriptor version 2: HMAC-SHA1-128 MIC, AES key wrap.
    pub const VERSION_HMAC_SHA1_AES: u16 = 2;
    pub const VERSION_MASK: u16 = 0b111;
    pub const PAIRWISE: u16 = 1 << 3;
    pub const INSTALL: u16 = 1 << 6;
    pub const ACK: u16 = 1 << 7;
    pub const MIC: u16 = 1 << 8;
    pub const SECURE: u16 = 1 << 9;
    pub const ERROR: u16 = 1 << 10;
    pub const REQUEST: u16 = 1 << 11;
    pub const ENCRYPTED: u16 = 1 << 12;
}

/// An EAPOL-Key frame, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key<'a> {
    pub info: u16,
    pub key_length: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub mic: [u8; 16],
    pub data: &'a [u8],
}

impl<'a> Key<'a> {
    /// Reads an EAPOL-Key frame (the 802.1X frame, from its version byte).
    /// `None` for anything else, or anything malformed.
    pub fn parse(frame: &'a [u8]) -> Option<Self> {
        if frame.len() < HEADER || frame[1] != TYPE_KEY || frame[4] != DESCRIPTOR_RSN {
            return None;
        }
        let body = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
        if 4 + body > frame.len() || body < HEADER - 4 {
            return None;
        }
        let at = |i: usize| u16::from_be_bytes([frame[i], frame[i + 1]]);
        let data_len = usize::from(at(HEADER - 2));
        let data = frame.get(HEADER..HEADER + data_len)?;
        if HEADER + data_len > 4 + body {
            return None;
        }
        let mut nonce = [0u8; 32];
        nonce.copy_from_slice(&frame[17..49]);
        let mut mic = [0u8; 16];
        mic.copy_from_slice(&frame[MIC_AT..MIC_AT + 16]);
        Some(Self {
            info: at(5),
            key_length: at(7),
            replay: u64::from_be_bytes(frame[9..17].try_into().ok()?),
            nonce,
            mic,
            data,
        })
    }
}

/// Writes an EAPOL-Key frame into `out` (its length), MIC zero; the caller
/// signs it with [`sign`].
pub fn build(
    out: &mut [u8],
    info: u16,
    replay: u64,
    nonce: &[u8; 32],
    data: &[u8],
) -> Option<usize> {
    let len = HEADER + data.len();
    let out = out.get_mut(..len)?;
    out.fill(0);
    out[0] = VERSION;
    out[1] = TYPE_KEY;
    out[2..4].copy_from_slice(&((len - 4) as u16).to_be_bytes());
    out[4] = DESCRIPTOR_RSN;
    out[5..7].copy_from_slice(&info.to_be_bytes());
    // Key length: 0 in the client's messages.
    out[9..17].copy_from_slice(&replay.to_be_bytes());
    out[17..49].copy_from_slice(nonce);
    out[HEADER - 2..HEADER].copy_from_slice(&(data.len() as u16).to_be_bytes());
    out[HEADER..].copy_from_slice(data);
    Some(len)
}

/// The MIC of `frame` with its MIC field zeroed: HMAC-SHA1-128 with the
/// key confirmation key.
pub fn mic(kck: &[u8; 16], frame: &[u8]) -> Option<[u8; 16]> {
    if frame.len() < HEADER {
        return None;
    }
    let zeros = [0u8; 16];
    let full = hmac_sha1(kck, &[&frame[..MIC_AT], &zeros, &frame[MIC_AT + 16..]]);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    Some(out)
}

/// Writes the MIC into a built frame.
pub fn sign(kck: &[u8; 16], frame: &mut [u8]) {
    if let Some(mic) = mic(kck, frame) {
        frame[MIC_AT..MIC_AT + 16].copy_from_slice(&mic);
    }
}

/// Whether `frame`'s MIC is right, compared in constant time.
pub fn verify(kck: &[u8; 16], frame: &[u8]) -> bool {
    let Some(want) = mic(kck, frame) else {
        return false;
    };
    let got = &frame[MIC_AT..MIC_AT + 16];
    want.iter().zip(got).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// The group temporal key in decrypted key data: the GTK KDE
/// (`dd len 00-0f-ac 01 keyid/tx reserved GTK`). Returns the key ID and
/// the key (16 bytes for CCMP).
pub fn gtk(data: &[u8]) -> Option<(u8, [u8; 16])> {
    let mut rest = data;
    while rest.len() >= 2 {
        let (kind, len) = (rest[0], usize::from(rest[1]));
        let element = rest.get(2..2 + len)?;
        if kind == 0xdd && len >= 6 + 16 && element[..4] == [0x00, 0x0f, 0xac, 1] {
            let mut key = [0u8; 16];
            key.copy_from_slice(&element[6..22]);
            return Some((element[4] & 0b11, key));
        }
        // Padding: `dd` then zeros.
        if kind == 0xdd && len == 0 {
            break;
        }
        rest = &rest[2 + len..];
    }
    None
}

/// Unwraps encrypted key data with the key encryption key (AES key wrap,
/// RFC 3394) into `out`; its length.
pub fn unwrap(kek: &[u8; 16], data: &[u8], out: &mut [u8]) -> Option<usize> {
    if data.len() < 24 || !data.len().is_multiple_of(8) || out.len() < data.len() - 8 {
        return None;
    }
    let len = data.len() - 8;
    aes_kw::KekAes128::from(*kek)
        .unwrap(data, &mut out[..len])
        .ok()?;
    Some(len)
}
