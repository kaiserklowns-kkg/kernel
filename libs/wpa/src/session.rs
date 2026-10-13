//! A protected link's data frames: the keys the handshakes gave, the
//! packet number sent with each frame, and the replay counters of what
//! arrives. Frames are whole 802.11 data frames; encryption is done here,
//! in software, whatever the chip (ADR-0129).

use crate::ccmp::{self, Header, Replay};
use crate::supplicant::Keys;

/// The keys in use and their counters.
#[derive(Default)]
pub struct Session {
    pairwise: Option<[u8; 16]>,
    group: Option<(u8, [u8; 16])>,
    /// The last packet number sent under the pairwise key.
    pn: u64,
    pairwise_replay: Replay,
    group_replay: Replay,
}

impl Session {
    /// Whether unicast frames are protected now.
    pub fn protected(&self) -> bool {
        self.pairwise.is_some()
    }

    /// Takes keys from a handshake: a new pairwise key starts its packet
    /// numbers and replay counter afresh; a new group key its own. The key
    /// already in use is left as it is: installing it again would reuse
    /// packet numbers, so nonces, under it (KRACK).
    pub fn install(&mut self, keys: Keys) {
        if let Some(tk) = keys.pairwise
            && self.pairwise != Some(tk)
        {
            self.pairwise = Some(tk);
            self.pn = 0;
            self.pairwise_replay = Replay::default();
        }
        if let Some(group) = keys.group {
            if self.group != Some(group) {
                self.group_replay = Replay::default();
            }
            self.group = Some(group);
        }
    }

    /// `frame` (a data frame, its MAC header `header_len` long) protected
    /// with the pairwise key into `out`: the header with the Protected bit,
    /// the CCMP header, the encrypted body and its MIC. Its length.
    pub fn protect(&mut self, frame: &[u8], header_len: usize, out: &mut [u8]) -> Option<usize> {
        let tk = self.pairwise?;
        let body = frame.get(header_len..)?;
        let len = header_len + ccmp::HEADER + body.len() + ccmp::MIC;
        let out = out.get_mut(..len)?;
        out[..header_len].copy_from_slice(&frame[..header_len]);
        out[1] |= 0x40;
        self.pn += 1;
        out[header_len..header_len + ccmp::HEADER].copy_from_slice(&ccmp::ccmp_header(self.pn, 0));
        let (header, rest) = out.split_at_mut(header_len);
        let rest = &mut rest[ccmp::HEADER..];
        rest[..body.len()].copy_from_slice(body);
        ccmp::seal(&tk, &Header { bytes: header }, self.pn, rest, body.len())?;
        Some(len)
    }

    /// A protected frame checked, decrypted and its packet number checked
    /// against replays, into `out` without the Protected bit, the CCMP
    /// header or the MIC: as if it had come in the clear. `None` for a
    /// forged, damaged, replayed or unexpected frame.
    pub fn unprotect(&mut self, frame: &[u8], header_len: usize, out: &mut [u8]) -> Option<usize> {
        if frame.get(1)? & 0x40 == 0 {
            return None;
        }
        let (pn, key_id) = ccmp::read_header(frame.get(header_len..header_len + ccmp::HEADER)?)?;
        let group = frame.get(4)? & 1 != 0;
        let tk = if group {
            match self.group {
                Some((id, tk)) if id == key_id => tk,
                _ => return None,
            }
        } else {
            self.pairwise.filter(|_| key_id == 0)?
        };
        let body = frame.get(header_len + ccmp::HEADER..)?;
        let len = header_len + body.len().checked_sub(ccmp::MIC)?;
        let mut work = [0u8; 2400];
        let work = work.get_mut(..body.len())?;
        work.copy_from_slice(body);
        let plain = ccmp::open(
            &tk,
            &Header {
                bytes: &frame[..header_len],
            },
            pn,
            work,
        )?;
        // Only a frame that decrypted moves the replay counter.
        let replay = if group {
            &mut self.group_replay
        } else {
            &mut self.pairwise_replay
        };
        if !replay.accept(pn) {
            return None;
        }
        let out = out.get_mut(..len)?;
        out[..header_len].copy_from_slice(&frame[..header_len]);
        out[1] &= !0x40;
        out[header_len..].copy_from_slice(&work[..plain]);
        Some(len)
    }
}
