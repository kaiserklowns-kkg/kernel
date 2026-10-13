//! CCMP-128 (802.11-2020 §12.5.3): AES-CCM over a data frame's body, with
//! an 8-byte MIC, a 48-bit packet number that never repeats under a key,
//! and the frame's addresses in the nonce and the additional data.

use aes::Aes128;
use ccm::aead::generic_array::GenericArray;
use ccm::aead::{AeadInPlace, KeyInit};
use ccm::consts::{U8, U13};

type Cipher = ccm::Ccm<Aes128, U8, U13>;

/// The CCMP header between the MAC header and the body.
pub const HEADER: usize = 8;
pub const MIC: usize = 8;

/// The frame control bit for a protected frame.
const PROTECTED: u8 = 0x40;

/// The parts of a data frame's MAC header CCMP uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header<'a> {
    /// The whole MAC header (24 bytes, 26 with QoS, 30/32 with A4).
    pub bytes: &'a [u8],
}

impl Header<'_> {
    fn qos(&self) -> bool {
        // A data frame (type 2) of a QoS subtype (bit 7 of the first byte).
        self.bytes[0] & 0x0c == 0x08 && self.bytes[0] & 0x80 != 0
    }

    fn four_addresses(&self) -> bool {
        self.bytes[1] & 0x03 == 0x03
    }

    fn expected_len(&self) -> usize {
        24 + if self.four_addresses() { 6 } else { 0 } + if self.qos() { 2 } else { 0 }
    }

    /// The CCM nonce: priority, A2, the PN (big-endian).
    pub fn nonce(&self, pn: u64) -> [u8; 13] {
        let mut nonce = [0u8; 13];
        if self.qos() {
            let at = self.expected_len() - 2;
            nonce[0] = self.bytes[at] & 0x0f;
        }
        nonce[1..7].copy_from_slice(&self.bytes[10..16]);
        nonce[7..].copy_from_slice(&pn.to_be_bytes()[2..]);
        nonce
    }

    /// The additional data: the header with the bits that may change in
    /// transit masked, the sequence number dropped (§12.5.3.3.3).
    pub fn aad(&self, out: &mut [u8; 32]) -> usize {
        let mut len = 0;
        let mut put = |bytes: &[u8]| {
            out[len..len + bytes.len()].copy_from_slice(bytes);
            len += bytes.len();
        };
        // Frame control: subtype bits 4-6 of a data frame, retry, power
        // management, more data cleared; protected set; order cleared for
        // QoS frames.
        let fc0 = self.bytes[0] & 0x8f;
        let mut fc1 = (self.bytes[1] & !(0x08 | 0x10 | 0x20)) | PROTECTED;
        if self.qos() {
            fc1 &= !0x80;
        }
        put(&[fc0, fc1]);
        put(&self.bytes[4..22]);
        // Sequence control: the fragment number only.
        put(&[self.bytes[22] & 0x0f, 0]);
        if self.four_addresses() {
            put(&self.bytes[24..30]);
        }
        if self.qos() {
            let at = self.expected_len() - 2;
            put(&[self.bytes[at] & 0x0f, 0]);
        }
        len
    }
}

/// The CCMP header for packet number `pn` and key `key_id`.
pub fn ccmp_header(pn: u64, key_id: u8) -> [u8; HEADER] {
    let p = pn.to_le_bytes();
    [p[0], p[1], 0, 0x20 | (key_id << 6), p[2], p[3], p[4], p[5]]
}

/// The packet number and key ID of a CCMP header.
pub fn read_header(header: &[u8]) -> Option<(u64, u8)> {
    let h: &[u8; HEADER] = header.get(..HEADER)?.try_into().ok()?;
    // Extended IV must be set.
    if h[3] & 0x20 == 0 {
        return None;
    }
    let pn = u64::from_le_bytes([h[0], h[1], h[4], h[5], h[6], h[7], 0, 0]);
    Some((pn, h[3] >> 6))
}

/// Protects `body` in place (it grows by the MIC) for `header`, with the
/// temporal key `tk` and packet number `pn`. `body` must have room for
/// `MIC` more bytes after `len`.
pub fn seal(
    tk: &[u8; 16],
    header: &Header<'_>,
    pn: u64,
    body: &mut [u8],
    len: usize,
) -> Option<usize> {
    if header.bytes.len() < header.expected_len() || body.len() < len + MIC {
        return None;
    }
    let mut aad = [0u8; 32];
    let aad_len = header.aad(&mut aad);
    let nonce = header.nonce(pn);
    let cipher = Cipher::new(GenericArray::from_slice(tk));
    let tag = cipher
        .encrypt_in_place_detached(
            GenericArray::from_slice(&nonce),
            &aad[..aad_len],
            &mut body[..len],
        )
        .ok()?;
    body[len..len + MIC].copy_from_slice(&tag);
    Some(len + MIC)
}

/// Checks and decrypts `body` (ciphertext then MIC) in place; the
/// plaintext's length, or `None` if it was forged or damaged.
pub fn open(tk: &[u8; 16], header: &Header<'_>, pn: u64, body: &mut [u8]) -> Option<usize> {
    if header.bytes.len() < header.expected_len() || body.len() < MIC {
        return None;
    }
    let len = body.len() - MIC;
    let mut aad = [0u8; 32];
    let aad_len = header.aad(&mut aad);
    let nonce = header.nonce(pn);
    let (data, tag) = body.split_at_mut(len);
    let cipher = Cipher::new(GenericArray::from_slice(tk));
    cipher
        .decrypt_in_place_detached(
            GenericArray::from_slice(&nonce),
            &aad[..aad_len],
            data,
            GenericArray::from_slice(tag),
        )
        .ok()?;
    Some(len)
}

/// Replay detection: the next packet number must be above the last one
/// accepted (per key and traffic class; one counter here).
#[derive(Default)]
pub struct Replay {
    last: Option<u64>,
}

impl Replay {
    /// Whether `pn` is new; records it if so.
    pub fn accept(&mut self, pn: u64) -> bool {
        if self.last.is_some_and(|last| pn <= last) {
            return false;
        }
        self.last = Some(pn);
        true
    }
}
