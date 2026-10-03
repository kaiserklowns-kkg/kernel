//! DNS messages (RFC 1035), ADR-0024: building A-record (and, ADR-0043,
//! AAAA-record, RFC 3596) queries and parsing the responses. No
//! allocation, so any program can resolve names.
//!
//! A response is untrusted network input. It must match the query's id and
//! question; every length, label and compression pointer is bounds-checked,
//! and pointer chains are limited, so a hostile packet cannot loop the
//! parser or read past the message.

#![no_std]

pub type Ipv4 = [u8; 4];
pub type Ipv6 = [u8; 16];

/// Longest name in text form (without a trailing dot).
pub const MAX_NAME: usize = 253;
/// Longest DNS message over UDP without EDNS.
pub const MAX_MESSAGE: usize = 512;
pub const PORT: u16 = 53;

const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const TYPE_AAAA: u16 = 28;
const CLASS_IN: u16 = 1;
const HEADER: usize = 12;
const MAX_POINTERS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsError {
    /// Not a valid host name.
    BadName,
    /// The output buffer is too small.
    TooLarge,
    /// The response does not belong to the query, or is malformed.
    Malformed,
    /// The response was truncated (TC): would need TCP.
    Truncated,
}

/// What a valid response says about the name; `A` is the kind of
/// address asked for ([`Ipv4`] for A records, [`Ipv6`] for AAAA).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer<A = Ipv4> {
    /// An address, and how long it may be cached (seconds).
    Address(A, u32),
    /// The name does not exist (NXDOMAIN).
    NotFound,
    /// The name exists but has no address of the kind asked for.
    NoAddress,
    /// The server failed or refused (another RCODE).
    ServerError(u8),
}

/// An address record type: A ([`Ipv4`]) or AAAA ([`Ipv6`]).
pub trait Record: Copy + Sized {
    /// The record (and query) type.
    const TYPE: u16;
    /// The record's data, if it has the right length.
    fn from_data(data: &[u8]) -> Option<Self>;
}

impl Record for Ipv4 {
    const TYPE: u16 = TYPE_A;
    fn from_data(data: &[u8]) -> Option<Self> {
        data.try_into().ok()
    }
}

impl Record for Ipv6 {
    const TYPE: u16 = TYPE_AAAA;
    fn from_data(data: &[u8]) -> Option<Self> {
        data.try_into().ok()
    }
}

/// Whether `name` is a host name we will ask for: dot-separated labels of
/// 1–63 letters, digits, hyphens or underscores (an optional trailing dot).
pub fn valid_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// Writes a recursive query for `name`'s A record; returns its length.
pub fn build_query(id: u16, name: &str, out: &mut [u8]) -> Result<usize, DnsError> {
    build_query_for::<Ipv4>(id, name, out)
}

/// Writes a recursive query for `name`'s record of type `R` (A or AAAA);
/// returns its length.
pub fn build_query_for<R: Record>(id: u16, name: &str, out: &mut [u8]) -> Result<usize, DnsError> {
    if !valid_name(name) {
        return Err(DnsError::BadName);
    }
    let name = name.strip_suffix('.').unwrap_or(name);
    let len = HEADER + name.len() + 2 + 4;
    let out = out.get_mut(..len).ok_or(DnsError::TooLarge)?;
    out[..HEADER].copy_from_slice(&[0, 0, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    out[..2].copy_from_slice(&id.to_be_bytes());
    let mut at = HEADER;
    for label in name.split('.') {
        out[at] = label.len() as u8;
        out[at + 1..at + 1 + label.len()].copy_from_slice(label.as_bytes());
        at += 1 + label.len();
    }
    out[at] = 0;
    out[at + 1..at + 3].copy_from_slice(&R::TYPE.to_be_bytes());
    out[at + 3..at + 5].copy_from_slice(&CLASS_IN.to_be_bytes());
    Ok(len)
}

fn be16(bytes: &[u8], at: usize) -> Result<u16, DnsError> {
    let b = bytes.get(at..at + 2).ok_or(DnsError::Malformed)?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

fn be32(bytes: &[u8], at: usize) -> Result<u32, DnsError> {
    let b = bytes.get(at..at + 4).ok_or(DnsError::Malformed)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Walks the (possibly compressed) name at `at`. Calls `label` for each
/// label in order; returns the offset just past the name where it starts.
fn walk_name(
    message: &[u8],
    mut at: usize,
    mut label: impl FnMut(&[u8]) -> Result<(), DnsError>,
) -> Result<usize, DnsError> {
    let mut end = None;
    let mut pointers = 0;
    let mut total = 0;
    loop {
        let len = *message.get(at).ok_or(DnsError::Malformed)?;
        match len >> 6 {
            0b00 => {
                if len == 0 {
                    return Ok(end.unwrap_or(at + 1));
                }
                let text = message
                    .get(at + 1..at + 1 + usize::from(len))
                    .ok_or(DnsError::Malformed)?;
                total += 1 + text.len();
                if total > MAX_NAME + 1 {
                    return Err(DnsError::Malformed);
                }
                label(text)?;
                at += 1 + text.len();
            }
            0b11 => {
                let target = usize::from(be16(message, at)? & 0x3fff);
                end.get_or_insert(at + 2);
                pointers += 1;
                // Pointers must go backwards, and chains are bounded.
                if pointers > MAX_POINTERS || target >= at {
                    return Err(DnsError::Malformed);
                }
                at = target;
            }
            _ => return Err(DnsError::Malformed),
        }
    }
}

/// Whether the name at `at` equals `name` (case-insensitively).
fn name_is(message: &[u8], at: usize, name: &str) -> Result<(bool, usize), DnsError> {
    let mut expected = name.strip_suffix('.').unwrap_or(name).split('.');
    let mut equal = true;
    let end = walk_name(message, at, |label| {
        match expected.next() {
            Some(want) if want.as_bytes().eq_ignore_ascii_case(label) => {}
            _ => equal = false,
        }
        Ok(())
    })?;
    Ok((equal && expected.next().is_none(), end))
}

/// Parses the response to query `id` for `name`'s A record.
pub fn parse_response(id: u16, name: &str, message: &[u8]) -> Result<Answer, DnsError> {
    parse_response_for::<Ipv4>(id, name, message)
}

/// Parses the response to query `id` for `name`'s record of type `R`.
pub fn parse_response_for<R: Record>(
    id: u16,
    name: &str,
    message: &[u8],
) -> Result<Answer<R>, DnsError> {
    if message.len() < HEADER || be16(message, 0)? != id {
        return Err(DnsError::Malformed);
    }
    let flags = be16(message, 2)?;
    let response = flags & 0x8000 != 0;
    let opcode = (flags >> 11) & 0xf;
    if !response || opcode != 0 {
        return Err(DnsError::Malformed);
    }
    if flags & 0x0200 != 0 {
        return Err(DnsError::Truncated);
    }
    if be16(message, 4)? != 1 {
        return Err(DnsError::Malformed);
    }
    let (asked, mut at) = name_is(message, HEADER, name)?;
    if !asked || be16(message, at)? != R::TYPE || be16(message, at + 2)? != CLASS_IN {
        return Err(DnsError::Malformed);
    }
    at += 4;
    match (flags & 0xf) as u8 {
        0 => {}
        3 => return Ok(Answer::NotFound),
        code => return Ok(Answer::ServerError(code)),
    }
    // Answers: follow the CNAME chain from the asked name to an address
    // record of the asked type.
    let mut owner_offset: Option<usize> = None; // None: the asked name
    let answers = be16(message, 6)?;
    let mut records = [(0usize, 0u16, 0usize, 0u16, 0u32); 16];
    let count = usize::from(answers).min(records.len());
    for record in records.iter_mut().take(count) {
        let start = at;
        at = walk_name(message, at, |_| Ok(()))?;
        let kind = be16(message, at)?;
        let class = be16(message, at + 2)?;
        let ttl = be32(message, at + 4)?;
        let length = usize::from(be16(message, at + 8)?);
        let data = at + 10;
        message
            .get(data..data + length)
            .ok_or(DnsError::Malformed)?;
        *record = (start, kind, data, class, ttl);
        at = data + length;
    }
    for _ in 0..=count {
        let mut next = None;
        for &(start, kind, data, class, ttl) in &records[..count] {
            if class != CLASS_IN {
                continue;
            }
            let owner_matches = match owner_offset {
                None => name_is(message, start, name)?.0,
                Some(target) => same_name(message, start, target)?,
            };
            if !owner_matches {
                continue;
            }
            match kind {
                kind if kind == R::TYPE => {
                    let length = usize::from(be16(message, data - 2)?);
                    let bytes = message
                        .get(data..data + length)
                        .ok_or(DnsError::Malformed)?;
                    let address = R::from_data(bytes).ok_or(DnsError::Malformed)?;
                    return Ok(Answer::Address(address, ttl));
                }
                TYPE_CNAME => next = Some(data),
                _ => {}
            }
        }
        match next {
            Some(target) => owner_offset = Some(target),
            None => break,
        }
    }
    Ok(Answer::NoAddress)
}

/// Whether the names at `a` and `b` are equal (case-insensitively).
fn same_name(message: &[u8], a: usize, b: usize) -> Result<bool, DnsError> {
    let mut left = [0u8; MAX_NAME + 1];
    let mut right = [0u8; MAX_NAME + 1];
    let flatten = |at: usize, out: &mut [u8; MAX_NAME + 1]| -> Result<usize, DnsError> {
        let mut len = 0;
        walk_name(message, at, |label| {
            let end = len + 1 + label.len();
            if end > out.len() {
                return Err(DnsError::Malformed);
            }
            out[len] = label.len() as u8;
            out[len + 1..end].copy_from_slice(label);
            len = end;
            Ok(())
        })?;
        Ok(len)
    };
    let l = flatten(a, &mut left)?;
    let r = flatten(b, &mut right)?;
    Ok(left[..l].eq_ignore_ascii_case(&right[..r]))
}

/// Answers `query` with `address` (or NXDOMAIN when `None`), for test
/// servers. Returns the response length. Only A queries get the address;
/// others are answered without records.
pub fn respond(query: &[u8], address: Option<Ipv4>, out: &mut [u8]) -> Result<usize, DnsError> {
    respond_with(query, address, None, out)
}

/// Answers `query` for a name with these addresses, for test servers:
/// NXDOMAIN if it has neither, else the record of the asked type (A or
/// AAAA) or, without one, no records. Returns the response length.
pub fn respond_with(
    query: &[u8],
    a: Option<Ipv4>,
    aaaa: Option<Ipv6>,
    out: &mut [u8],
) -> Result<usize, DnsError> {
    if query.len() < HEADER || be16(query, 4)? != 1 {
        return Err(DnsError::Malformed);
    }
    let name_end = walk_name(query, HEADER, |_| Ok(()))?;
    let question = query.get(HEADER..name_end + 4).ok_or(DnsError::Malformed)?;
    let qtype = be16(query, name_end)?;
    let mut data = [0u8; 16];
    let record: Option<&[u8]> = match (qtype, a, aaaa) {
        (TYPE_A, Some(address), _) => {
            data[..4].copy_from_slice(&address);
            Some(&data[..4])
        }
        (TYPE_AAAA, _, Some(address)) => {
            data.copy_from_slice(&address);
            Some(&data[..])
        }
        _ => None,
    };
    let len = HEADER + question.len() + record.map_or(0, |r| 12 + r.len());
    let out = out.get_mut(..len).ok_or(DnsError::TooLarge)?;
    out[..2].copy_from_slice(&query[..2]);
    let rcode = if a.is_some() || aaaa.is_some() { 0 } else { 3 };
    out[2..4].copy_from_slice(&(0x8180u16 | rcode).to_be_bytes());
    out[4..12].copy_from_slice(&[0, 1, 0, u8::from(record.is_some()), 0, 0, 0, 0]);
    out[HEADER..HEADER + question.len()].copy_from_slice(question);
    if let Some(record) = record {
        let at = HEADER + question.len();
        out[at..at + 2].copy_from_slice(&0xc00cu16.to_be_bytes()); // the question's name
        out[at + 2..at + 4].copy_from_slice(&qtype.to_be_bytes());
        out[at + 4..at + 6].copy_from_slice(&CLASS_IN.to_be_bytes());
        out[at + 6..at + 10].copy_from_slice(&60u32.to_be_bytes());
        out[at + 10..at + 12].copy_from_slice(&(record.len() as u16).to_be_bytes());
        out[at + 12..].copy_from_slice(record);
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn query(name: &str) -> Vec<u8> {
        let mut out = [0u8; MAX_MESSAGE];
        let len = build_query(0x1234, name, &mut out).unwrap();
        out[..len].to_vec()
    }

    #[test]
    fn builds_standard_queries() {
        let q = query("example.com");
        assert_eq!(
            q,
            [
                0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 7, b'e', b'x', b'a', b'm', b'p',
                b'l', b'e', 3, b'c', b'o', b'm', 0, 0, 1, 0, 1
            ]
        );
        assert_eq!(query("example.com."), q, "trailing dot");
        let mut small = [0u8; 10];
        assert_eq!(
            build_query(1, "example.com", &mut small),
            Err(DnsError::TooLarge)
        );
    }

    #[test]
    fn validates_names() {
        for good in ["a", "oceans.test", "x-1.example.com", "_srv.host", "a.b.c."] {
            assert!(valid_name(good), "{good}");
        }
        let long_label = "a".repeat(64);
        let long_name = ["abcdefghi"; 26].join(".");
        for bad in [
            "",
            ".",
            "a..b",
            "-a.com",
            "a-.com",
            "a b",
            "ä.com",
            &long_label,
            &long_name,
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn parses_answers_and_errors() {
        let q = query("oceans.test");
        let mut out = [0u8; MAX_MESSAGE];
        let len = respond(&q, Some([10, 1, 2, 3]), &mut out).unwrap();
        assert_eq!(
            parse_response(0x1234, "oceans.test", &out[..len]),
            Ok(Answer::Address([10, 1, 2, 3], 60))
        );
        assert_eq!(
            parse_response(0x1234, "OCEANS.Test", &out[..len]),
            Ok(Answer::Address([10, 1, 2, 3], 60)),
            "case-insensitive"
        );
        assert_eq!(
            parse_response(0x9999, "oceans.test", &out[..len]),
            Err(DnsError::Malformed),
            "id"
        );
        assert_eq!(
            parse_response(0x1234, "other.test", &out[..len]),
            Err(DnsError::Malformed),
            "question"
        );

        let len = respond(&q, None, &mut out).unwrap();
        assert_eq!(
            parse_response(0x1234, "oceans.test", &out[..len]),
            Ok(Answer::NotFound)
        );

        // The query itself is not a response.
        assert_eq!(
            parse_response(0x1234, "oceans.test", &q),
            Err(DnsError::Malformed)
        );
        let mut truncated = out[..len].to_vec();
        truncated[2] |= 0x02;
        assert_eq!(
            parse_response(0x1234, "oceans.test", &truncated),
            Err(DnsError::Truncated)
        );
        let mut failed = out[..len].to_vec();
        failed[3] = (failed[3] & 0xf0) | 2;
        assert_eq!(
            parse_response(0x1234, "oceans.test", &failed),
            Ok(Answer::ServerError(2))
        );
    }

    #[test]
    fn aaaa_queries_and_answers() {
        let mut out = [0u8; MAX_MESSAGE];
        let len = build_query_for::<Ipv6>(0x4321, "oceans.test", &mut out).unwrap();
        let q = out[..len].to_vec();
        assert_eq!(q[len - 4..], [0, 28, 0, 1], "type AAAA, class IN");
        let v6: Ipv6 = [0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3];
        let mut answer = [0u8; MAX_MESSAGE];
        let len = respond_with(&q, Some([10, 1, 2, 3]), Some(v6), &mut answer).unwrap();
        assert_eq!(
            parse_response_for::<Ipv6>(0x4321, "oceans.test", &answer[..len]),
            Ok(Answer::Address(v6, 60))
        );
        // The answer to an AAAA question is not one to an A question.
        assert_eq!(
            parse_response(0x4321, "oceans.test", &answer[..len]),
            Err(DnsError::Malformed)
        );
        // A name with only an IPv4 address: no AAAA records.
        let len = respond_with(&q, Some([10, 1, 2, 3]), None, &mut answer).unwrap();
        assert_eq!(
            parse_response_for::<Ipv6>(0x4321, "oceans.test", &answer[..len]),
            Ok(Answer::NoAddress)
        );
        let len = respond_with(&q, None, None, &mut answer).unwrap();
        assert_eq!(
            parse_response_for::<Ipv6>(0x4321, "oceans.test", &answer[..len]),
            Ok(Answer::NotFound)
        );
        // An AAAA record of the wrong length is refused.
        let len = respond_with(&q, None, Some(v6), &mut answer).unwrap();
        let mut short = answer[..len - 1].to_vec();
        let at = short.len() - 15 - 2;
        short[at..at + 2].copy_from_slice(&15u16.to_be_bytes());
        assert_eq!(
            parse_response_for::<Ipv6>(0x4321, "oceans.test", &short),
            Err(DnsError::Malformed)
        );
        // An A record (4 bytes) does not pass as AAAA even if typed so.
        let a_query = query("oceans.test");
        let len = respond(&a_query, Some([1, 2, 3, 4]), &mut answer).unwrap();
        let mut retyped = answer[..len].to_vec();
        // The record is the last 16 bytes: name pointer, then its type.
        retyped[len - 14..len - 12].copy_from_slice(&28u16.to_be_bytes());
        let question_type = a_query.len() - 4;
        retyped[question_type..question_type + 2].copy_from_slice(&28u16.to_be_bytes());
        assert_eq!(
            parse_response_for::<Ipv6>(0x1234, "oceans.test", &retyped),
            Err(DnsError::Malformed)
        );
        // Mutations never panic.
        let good = answer[..len].to_vec();
        for at in 0..good.len() {
            for value in [0u8, 4, 16, 28, 0xc0, 0xff] {
                let mut mutated = good.clone();
                mutated[at] = value;
                let _ = parse_response_for::<Ipv6>(0x1234, "oceans.test", &mutated);
            }
        }
    }

    #[test]
    fn follows_cname_chains() {
        // www.oceans.test CNAME host.oceans.test; host.oceans.test A 10.9.9.9
        let mut m = query("www.oceans.test");
        m[2] = 0x81;
        m[3] = 0x80;
        m[7] = 2; // two answers
        // CNAME owned by the question name (pointer to 12).
        m.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 30]);
        let target = [4, b'h', b'o', b's', b't', 0xc0, 16]; // "host" + pointer to "oceans.test"
        m.extend_from_slice(&(target.len() as u16).to_be_bytes());
        let host_at = m.len();
        m.extend_from_slice(&target);
        // A record owned by the CNAME target.
        m.extend_from_slice(&[
            0xc0,
            host_at as u8,
            0,
            1,
            0,
            1,
            0,
            0,
            0,
            30,
            0,
            4,
            10,
            9,
            9,
            9,
        ]);
        assert_eq!(
            parse_response(0x1234, "www.oceans.test", &m),
            Ok(Answer::Address([10, 9, 9, 9], 30))
        );
        // Without the A record: the name has no address.
        let cname_only_len = host_at + target.len();
        let mut cname_only = m[..cname_only_len].to_vec();
        cname_only[7] = 1;
        assert_eq!(
            parse_response(0x1234, "www.oceans.test", &cname_only),
            Ok(Answer::NoAddress)
        );
    }

    #[test]
    fn hostile_responses_are_rejected_without_panic() {
        let q = query("oceans.test");
        let mut out = [0u8; MAX_MESSAGE];
        let len = respond(&q, Some([1, 2, 3, 4]), &mut out).unwrap();
        let good = out[..len].to_vec();

        // A pointer loop, and a forward pointer.
        let mut looped = good.clone();
        let answer = q.len();
        looped[answer] = 0xc0;
        looped[answer + 1] = answer as u8;
        assert_eq!(
            parse_response(0x1234, "oceans.test", &looped),
            Err(DnsError::Malformed)
        );
        let mut forward = good.clone();
        forward[answer + 1] = 0xff;
        assert!(parse_response(0x1234, "oceans.test", &forward).is_err());
        // An A record claiming 5 bytes.
        let mut long_a = good.clone();
        long_a[len - 5] = 5;
        assert!(parse_response(0x1234, "oceans.test", &long_a).is_err());

        // Every single-byte mutation and every truncation: no panic.
        for at in 0..good.len() {
            for value in [0u8, 1, 0x3f, 0x40, 0xc0, 0xc0 | 12, 0xff] {
                let mut mutated = good.clone();
                mutated[at] = value;
                let _ = parse_response(0x1234, "oceans.test", &mutated);
            }
            let _ = parse_response(0x1234, "oceans.test", &good[..at]);
        }
    }
}
