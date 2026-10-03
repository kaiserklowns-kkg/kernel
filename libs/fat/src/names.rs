//! Names for new entries: what FAT accepts, the 8.3 short name, and the
//! long-name (VFAT) entries that carry the real name.

use alloc::vec::Vec;

use crate::{ATTR_LONG_NAME, ENTRY, Error, checksum};

/// Characters a long name may not contain (besides controls).
const FORBIDDEN: &[char] = &['"', '*', '/', ':', '<', '>', '?', '\\', '|'];
/// Punctuation allowed in short names.
const SHORT_PUNCTUATION: &[u8] = b"!#$%&'()-@^_`{}~";
/// Longest long name, in UTF-16 units.
const MAX_LONG: usize = 255;

/// Whether FAT can store `name` (and the file service can serve it).
pub fn validate(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name.len() <= crate::MAX_NAME
        && name.encode_utf16().count() <= MAX_LONG
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|c| c < ' ' || FORBIDDEN.contains(&c));
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidName)
    }
}

/// How a name is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The 8.3 name, space-padded: final, or the basis for a `~N` tail.
    pub short: [u8; 11],
    /// NT case flags (lowercase base 0x08, extension 0x10).
    pub case: u8,
    /// The name needs long-name entries (and a `~N` short name).
    pub long: bool,
}

/// The short form of `name`. A name that is exactly 8.3 in one case per
/// part is stored short (as Windows and Linux do); anything else gets long
/// entries and a generated short name.
pub fn plan(name: &str) -> Plan {
    let (base, extension) = match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at + 1..]),
        _ => (name, ""),
    };
    let (base_bytes, base_lossy, base_lower, base_upper) = convert(base);
    let (extension_bytes, extension_lossy, extension_lower, extension_upper) = convert(extension);
    let fits = base_bytes.len() <= 8 && extension_bytes.len() <= 3;
    let exact = !base_lossy && !extension_lossy && fits && !base_bytes.is_empty();
    let mixed = (base_lower && base_upper) || (extension_lower && extension_upper);
    let mut short = [b' '; 11];
    for (slot, &byte) in short[..8].iter_mut().zip(base_bytes.iter()) {
        *slot = byte;
    }
    for (slot, &byte) in short[8..].iter_mut().zip(extension_bytes.iter()) {
        *slot = byte;
    }
    if short[0] == b' ' {
        short[0] = b'_';
    }
    if exact && !mixed {
        let case = if base_lower { 0x08 } else { 0 } | if extension_lower { 0x10 } else { 0 };
        Plan {
            short,
            case,
            long: false,
        }
    } else {
        Plan {
            short,
            case: 0,
            long: true,
        }
    }
}

/// Uppercase short-name bytes of `part`, and whether it lost anything and
/// had lower or upper case letters.
fn convert(part: &str) -> (Vec<u8>, bool, bool, bool) {
    let (mut lossy, mut lower, mut upper) = (false, false, false);
    let mut out = Vec::new();
    for c in part.chars() {
        match c {
            ' ' | '.' => lossy = true,
            'a'..='z' => {
                lower = true;
                out.push(c.to_ascii_uppercase() as u8);
            }
            'A'..='Z' => {
                upper = true;
                out.push(c as u8);
            }
            '0'..='9' => out.push(c as u8),
            c if c.is_ascii() && SHORT_PUNCTUATION.contains(&(c as u8)) => out.push(c as u8),
            _ => {
                lossy = true;
                out.push(b'_');
            }
        }
    }
    (out, lossy, lower, upper)
}

/// The short name `basis` with tail `~n` (the basis cut to make room).
pub fn with_tail(basis: &[u8; 11], n: u32) -> [u8; 11] {
    let mut tail = [0u8; 8];
    let mut digits = n;
    let mut len = 0;
    while digits > 0 {
        tail[len] = b'0' + (digits % 10) as u8;
        digits /= 10;
        len += 1;
    }
    let keep = basis[..8]
        .iter()
        .take_while(|&&b| b != b' ')
        .count()
        .min(7 - len);
    let mut short = [b' '; 11];
    short[..keep].copy_from_slice(&basis[..keep]);
    short[keep] = b'~';
    for i in 0..len {
        short[keep + 1 + i] = tail[len - 1 - i];
    }
    short[8..].copy_from_slice(&basis[8..]);
    short
}

/// The long-name entries for `name`, in disk order (the last part first),
/// tied to `short` by its checksum.
pub fn long_entries(name: &str, short: &[u8; 11]) -> Vec<[u8; ENTRY]> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    let count = units.len().div_ceil(13);
    if units.len() < count * 13 {
        units.push(0);
    }
    units.resize(count * 13, 0xffff);
    let sum = checksum(short);
    let mut out = Vec::new();
    for sequence in (1..=count).rev() {
        let mut entry = [0u8; ENTRY];
        entry[0] = sequence as u8 | if sequence == count { 0x40 } else { 0 };
        entry[11] = ATTR_LONG_NAME;
        entry[13] = sum;
        let part = &units[(sequence - 1) * 13..sequence * 13];
        let places = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (&at, &unit) in places.iter().zip(part) {
            entry[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        out.push(entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(s: &[u8; 11]) -> &str {
        core::str::from_utf8(s).unwrap()
    }

    #[test]
    fn validates_names() {
        for good in [
            "a",
            "readme.txt",
            "A long name.md",
            "ไฟล์.txt",
            "x.y.z",
            "[brackets]",
        ] {
            assert_eq!(validate(good), Ok(()), "{good}");
        }
        for bad in [
            "", ".", "..", "a/b", "a:b", "what?", "dot.", "space ", "tab\t",
        ] {
            assert_eq!(validate(bad), Err(Error::InvalidName), "{bad:?}");
        }
    }

    #[test]
    fn plans_short_and_long_names() {
        let p = plan("README.TXT");
        assert_eq!((short(&p.short), p.case, p.long), ("README  TXT", 0, false));
        let p = plan("notes.txt");
        assert_eq!(
            (short(&p.short), p.case, p.long),
            ("NOTES   TXT", 0x18, false)
        );
        let p = plan("Makefile");
        assert!(p.long, "mixed case needs the long name");
        let p = plan("A long file name.txt");
        assert_eq!((short(&p.short), p.long), ("ALONGFILTXT", true));
        let p = plan("ไฟล์.txt");
        assert_eq!((short(&p.short), p.long), ("____    TXT", true));
        let p = plan(".bashrc");
        assert_eq!((short(&p.short), p.long), ("BASHRC     ", true));
        let p = plan("archive.tar.gz");
        assert_eq!((short(&p.short), p.long), ("ARCHIVETGZ ", true));
        let p = plan("toolongname.c");
        assert_eq!((short(&p.short), p.long), ("TOOLONGNC  ", true));
    }

    #[test]
    fn adds_numeric_tails() {
        let basis = plan("A long file name.txt").short;
        assert_eq!(short(&with_tail(&basis, 1)), "ALONGF~1TXT");
        assert_eq!(short(&with_tail(&basis, 42)), "ALONG~42TXT");
        assert_eq!(short(&with_tail(&plan("ab.c").short, 3)), "AB~3    C  ");
    }

    #[test]
    fn encodes_long_entries() {
        let name = "A long file name.txt"; // 20 units: two entries
        let short = with_tail(&plan(name).short, 1);
        let entries = long_entries(name, &short);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0][0], 0x42);
        assert_eq!(entries[1][0], 0x01);
        // Read back through the reader's own decoder.
        let mut long = crate::LongName::new();
        for entry in &entries {
            long.add(entry);
        }
        let mut raw = [0u8; ENTRY];
        raw[..11].copy_from_slice(&short);
        assert_eq!(long.take(checksum(&raw)).as_deref(), Some(name));
        // Exactly 13 units: no terminator, no padding.
        let entries = long_entries("abcdefghijklm", &short);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            u16::from_le_bytes([entries[0][30], entries[0][31]]),
            u16::from(b'm')
        );
    }
}
