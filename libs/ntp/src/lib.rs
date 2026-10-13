//! SNTP (RFC 4330, the client side of RFC 5905) for Oceans' clock
//! (ADR-0143): requests, replies checked, and the time a reply gives.
//! No allocation, no I/O; host-tested.
//!
//! A reply is untrusted (it travels unauthenticated over UDP):
//! - it must answer our request: its origin timestamp is the random nonce
//!   we sent as our transmit timestamp (RFC 9109), so blind spoofing needs
//!   64 bits of luck;
//! - it must come from a synchronized server (leap indicator not 3,
//!   stratum 1 to 15) with times that make sense;
//! - the caller sets the clock only when two servers agree ([`agree`]).

#![no_std]

/// The NTP port.
pub const PORT: u16 = 123;
/// The size of a request and of the replies used.
pub const PACKET: usize = 48;
/// Seconds from 1900 (NTP's epoch) to 1970 (Unix's).
const NTP_TO_UNIX: u64 = 2_208_988_800;
/// Two servers agree if their times are within this much.
pub const AGREEMENT_MS: u64 = 2_000;

/// Version 4, mode 3 (client).
const CLIENT: u8 = (4 << 3) | 3;
const MODE_SERVER: u8 = 4;
const MODE_BROADCAST: u8 = 5;

/// A request whose transmit timestamp is `nonce`: random, not the time,
/// so the reply can be matched to it and our clock is not disclosed.
pub fn request(nonce: u64) -> [u8; PACKET] {
    let mut packet = [0u8; PACKET];
    packet[0] = CLIENT;
    packet[40..48].copy_from_slice(&nonce.to_be_bytes());
    packet
}

/// Why a reply was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Short,
    /// Not a server's answer (mode, version).
    NotAnAnswer,
    /// Not to our request (origin is not the nonce).
    NotOurs,
    /// The server says it is not synchronized (leap 3, stratum 0 or 16+;
    /// stratum 0 is also a "kiss of death").
    Unsynchronized,
    /// Its times are zero or out of order.
    BadTimes,
}

/// What a reply says: when the server got the request and when it sent
/// the answer, in Unix milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub stratum: u8,
    pub received_ms: u64,
    pub sent_ms: u64,
}

/// An NTP timestamp (seconds since 1900, 32-bit fraction) in Unix
/// milliseconds. Seconds below 2^31 are in the era after 2036 (RFC 4330
/// §3); zero and times before 1970 are refused.
fn unix_ms(bytes: &[u8]) -> Option<u64> {
    // Zero: not set.
    if bytes[..8].iter().all(|&b| b == 0) {
        return None;
    }
    let seconds = u64::from(u32::from_be_bytes(bytes[..4].try_into().ok()?));
    let fraction = u64::from(u32::from_be_bytes(bytes[4..8].try_into().ok()?));
    let seconds = if seconds < 1 << 31 {
        seconds + (1 << 32)
    } else {
        seconds
    };
    let unix = seconds.checked_sub(NTP_TO_UNIX)?;
    Some(unix * 1000 + ((fraction * 1000) >> 32))
}

/// Checks a reply to the request carrying `nonce`.
pub fn parse(reply: &[u8], nonce: u64) -> Result<Reply, Refused> {
    if reply.len() < PACKET {
        return Err(Refused::Short);
    }
    let leap = reply[0] >> 6;
    let version = (reply[0] >> 3) & 7;
    let mode = reply[0] & 7;
    if !matches!(mode, MODE_SERVER | MODE_BROADCAST) || !(3..=4).contains(&version) {
        return Err(Refused::NotAnAnswer);
    }
    if reply[24..32] != nonce.to_be_bytes() {
        return Err(Refused::NotOurs);
    }
    let stratum = reply[1];
    if leap == 3 || !(1..=15).contains(&stratum) {
        return Err(Refused::Unsynchronized);
    }
    let (Some(received_ms), Some(sent_ms)) = (unix_ms(&reply[32..40]), unix_ms(&reply[40..48]))
    else {
        return Err(Refused::BadTimes);
    };
    if received_ms == 0 || sent_ms < received_ms {
        return Err(Refused::BadTimes);
    }
    Ok(Reply {
        stratum,
        received_ms,
        sent_ms,
    })
}

/// The Unix time at the moment the reply arrived: the server's send time
/// plus half the network's round trip (what the exchange took on our
/// monotonic clock, `sent_at` to `arrived_at`, less the server's own
/// time).
pub fn time_at_arrival(reply: &Reply, sent_at: u64, arrived_at: u64) -> u64 {
    let round_trip = arrived_at.saturating_sub(sent_at);
    let at_server = reply.sent_ms - reply.received_ms;
    reply.sent_ms + round_trip.saturating_sub(at_server) / 2
}

/// Whether two times, taken at monotonic moments `a_at` and `b_at`, are
/// the same time within [`AGREEMENT_MS`].
pub fn agree(a: u64, a_at: u64, b: u64, b_at: u64) -> bool {
    // Both brought to the later moment.
    let later = a_at.max(b_at);
    let a = a + (later - a_at);
    let b = b + (later - b_at);
    a.abs_diff(b) <= AGREEMENT_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server's reply: stratum 2, received at `received`, sent at
    /// `sent` (NTP seconds), to `nonce`.
    fn reply(nonce: u64, received: u32, sent: u32) -> [u8; PACKET] {
        let mut packet = [0u8; PACKET];
        packet[0] = (4 << 3) | 4;
        packet[1] = 2;
        packet[24..32].copy_from_slice(&nonce.to_be_bytes());
        packet[32..36].copy_from_slice(&received.to_be_bytes());
        packet[40..44].copy_from_slice(&sent.to_be_bytes());
        // Half a second, in the sent timestamp's fraction.
        packet[44..48].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        packet
    }

    /// 2026-10-14 00:00:00 UTC.
    const OCT_14: u32 = (1_792_022_400 + NTP_TO_UNIX) as u32;

    #[test]
    fn a_good_reply() {
        let request = request(0x1234_5678_9abc_def0);
        assert_eq!(request[0], 0x23);
        assert_eq!(&request[40..], &0x1234_5678_9abc_def0u64.to_be_bytes());
        let parsed = parse(
            &reply(0x1234_5678_9abc_def0, OCT_14, OCT_14 + 1),
            0x1234_5678_9abc_def0,
        )
        .unwrap();
        assert_eq!(parsed.received_ms, 1_792_022_400_000);
        assert_eq!(parsed.sent_ms, 1_792_022_401_500);
        // 1.7 s from sending to the answer, 1.5 s of it at the server:
        // 100 ms each way on the wire.
        assert_eq!(time_at_arrival(&parsed, 10_000, 11_700), 1_792_022_401_600);
    }

    #[test]
    fn bad_replies_are_refused() {
        let good = reply(7, OCT_14, OCT_14);
        assert_eq!(parse(&good[..47], 7), Err(Refused::Short));
        assert_eq!(parse(&good, 8), Err(Refused::NotOurs));
        let mut client = good;
        client[0] = CLIENT;
        assert_eq!(parse(&client, 7), Err(Refused::NotAnAnswer));
        let mut unsynced = good;
        unsynced[0] |= 3 << 6;
        assert_eq!(parse(&unsynced, 7), Err(Refused::Unsynchronized));
        let mut kiss = good;
        kiss[1] = 0;
        assert_eq!(parse(&kiss, 7), Err(Refused::Unsynchronized));
        assert_eq!(
            parse(&reply(7, OCT_14 + 5, OCT_14), 7),
            Err(Refused::BadTimes)
        );
        assert_eq!(parse(&reply(7, 0, 0), 7), Err(Refused::BadTimes));
    }

    #[test]
    fn the_next_era_and_agreement() {
        // 2040: seconds have wrapped past 2^32.
        let wrapped = ((2_208_988_800u64 + 2_208_988_800) - (1 << 32)) as u32;
        let parsed = parse(&reply(1, wrapped, wrapped), 1).unwrap();
        assert_eq!(parsed.received_ms / 1000, 2_208_988_800);
        assert!(agree(1_000_000, 50, 1_001_500, 50));
        assert!(
            agree(1_000_000, 50, 1_000_000, 1_000),
            "brought to the same moment"
        );
        assert!(!agree(1_000_000, 50, 1_003_000, 50));
    }
}
