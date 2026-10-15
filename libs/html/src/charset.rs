//! A page's bytes into text: UTF-8, or the Thai single-byte encodings
//! older Thai pages are written in (TIS-620, and Windows-874 and
//! ISO-8859-11, which extend it), named by the response's `Content-Type`
//! or by a `<meta charset>` near the page's start.

use alloc::string::String;

/// How far into a page a `<meta charset>` is looked for (as WHATWG's
/// prescan, §13.2.3.2).
const PRESCAN: usize = 1024;

/// `bytes` as text, by `content_type`'s charset, else the page's own
/// `<meta>`, else UTF-8 (what cannot be read becomes U+FFFD).
pub fn decode_page(bytes: &[u8], content_type: Option<&str>) -> String {
    let named = content_type
        .and_then(charset_in)
        .or_else(|| charset_in(&String::from_utf8_lossy(&bytes[..bytes.len().min(PRESCAN)])));
    match named {
        Some(Charset::Thai) => thai(bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

enum Charset {
    Utf8,
    Thai,
}

/// The charset `text` names after `charset=`.
fn charset_in(text: &str) -> Option<Charset> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("charset=")? + "charset=".len();
    let name: String = lower[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    match name.as_str() {
        "tis-620" | "tis620" | "windows-874" | "cp874" | "iso-8859-11" | "x-windows-874" => {
            Some(Charset::Thai)
        }
        "utf-8" | "utf8" => Some(Charset::Utf8),
        _ => None,
    }
}

/// Windows-874: ASCII, Thai at 0xA1..=0xFB (U+0E01 on), and a few
/// punctuation marks at 0x80..=0x9F.
fn thai(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x00..=0x7f => char::from(b),
            0x80 => '€',
            0x85 => '…',
            0x91 => '‘',
            0x92 => '’',
            0x93 => '“',
            0x94 => '”',
            0x95 => '•',
            0x96 => '–',
            0x97 => '—',
            0xa0 => '\u{a0}',
            0xa1..=0xda | 0xdf..=0xfb => {
                char::from_u32(0x0e00 + u32::from(b) - 0xa0).unwrap_or('\u{fffd}')
            }
            _ => '\u{fffd}',
        })
        .collect()
}
