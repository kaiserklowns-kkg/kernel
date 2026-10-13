//! WPA2-Personal (IEEE 802.11-2020 §12.7, ADR-0129): what a Wi-Fi client
//! needs once it is associated, whatever the chip.
//!
//! - [`psk`]: the pairwise master key from a passphrase and the SSID.
//! - [`prf`] and [`Ptk`]: the keys of one association.
//! - [`eapol`]: EAPOL-Key frames, read and written, and their MIC.
//! - [`Supplicant`]: the 4-way handshake and the group key handshake, as
//!   the client side runs them; it checks every frame (replay counter,
//!   MIC, the access point's nonce) and installs keys only when it may.
//! - [`ccmp`]: protecting and checking data frames (CCMP-128).
//!
//! Frames come from the air: untrusted. Everything is length-checked; a
//! frame that fails a check is dropped, never acted on.
//!
//! Only WPA2-Personal with CCMP: the security every access point offers.
//! WPA3 (SAE) is later work.

#![cfg_attr(not(test), no_std)]

pub mod ccmp;
pub mod eapol;
mod session;
mod supplicant;

pub use session::Session;
pub use supplicant::{Action, Keys, Supplicant};

use hmac::{Hmac, Mac};
use sha1::Sha1;

pub type MacAddress = [u8; 6];

/// The RSN element the client sends: WPA2, CCMP for both, PSK.
pub const RSN_ELEMENT: [u8; 22] = [
    0x30, 20, // RSN, length
    1, 0, // version 1
    0x00, 0x0f, 0xac, 4, // group cipher: CCMP
    1, 0, 0x00, 0x0f, 0xac, 4, // one pairwise cipher: CCMP
    1, 0, 0x00, 0x0f, 0xac, 2, // one AKM: PSK
    0, 0, // capabilities
];

/// The pairwise master key of a WPA2-Personal network: PBKDF2-HMAC-SHA1
/// of the passphrase (8 to 63 printable ASCII characters) salted with the
/// SSID, 4096 rounds, 32 bytes. `None` for a passphrase outside that.
pub fn psk(passphrase: &str, ssid: &[u8]) -> Option<[u8; 32]> {
    let ok = (8..=63).contains(&passphrase.len())
        && passphrase.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && !ssid.is_empty()
        && ssid.len() <= 32;
    if !ok {
        return None;
    }
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase.as_bytes(), ssid, 4096, &mut key);
    Some(key)
}

fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// The 802.11 PRF (§12.7.1.2): HMAC-SHA1 of `label || 0 || data || i`
/// for i = 0, 1, ..., filling `out`.
pub fn prf(key: &[u8], label: &str, data: &[u8], out: &mut [u8]) {
    for (i, chunk) in out.chunks_mut(20).enumerate() {
        let block = hmac_sha1(key, &[label.as_bytes(), &[0], data, &[i as u8]]);
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
}

/// The pairwise transient key for CCMP (§12.7.1.3): the key confirmation
/// key (the MIC's), the key encryption key (the group key's), the temporal
/// key (the data's).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

impl core::fmt::Debug for Ptk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Ptk(..)")
    }
}

impl Ptk {
    /// From the PMK, the access point's address `aa`, ours `spa`, and the
    /// two nonces (each pair in ascending order, as the standard orders
    /// them).
    pub fn derive(
        pmk: &[u8; 32],
        aa: &MacAddress,
        spa: &MacAddress,
        anonce: &[u8; 32],
        snonce: &[u8; 32],
    ) -> Self {
        let mut data = [0u8; 6 + 6 + 32 + 32];
        let (lo, hi) = if aa <= spa { (aa, spa) } else { (spa, aa) };
        data[..6].copy_from_slice(lo);
        data[6..12].copy_from_slice(hi);
        let (lo, hi) = if anonce <= snonce {
            (anonce, snonce)
        } else {
            (snonce, anonce)
        };
        data[12..44].copy_from_slice(lo);
        data[44..].copy_from_slice(hi);
        let mut out = [0u8; 48];
        prf(pmk, "Pairwise key expansion", &data, &mut out);
        let mut ptk = Self {
            kck: [0; 16],
            kek: [0; 16],
            tk: [0; 16],
        };
        ptk.kck.copy_from_slice(&out[..16]);
        ptk.kek.copy_from_slice(&out[16..32]);
        ptk.tk.copy_from_slice(&out[32..]);
        ptk
    }
}

#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;
