//! Character references: `&amp;`, `&#3585;`, `&#x0E01;`. The named ones
//! are those pages use most; an unknown one stays as written, as browsers
//! leave it.

use alloc::string::String;

/// The named references known, by name.
const NAMED: [(&str, char); 34] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("copy", '©'),
    ("reg", '®'),
    ("trade", '™'),
    ("hellip", '…'),
    ("mdash", '—'),
    ("ndash", '–'),
    ("lsquo", '‘'),
    ("rsquo", '’'),
    ("ldquo", '“'),
    ("rdquo", '”'),
    ("laquo", '«'),
    ("raquo", '»'),
    ("bull", '•'),
    ("middot", '·'),
    ("deg", '°'),
    ("plusmn", '±'),
    ("times", '×'),
    ("divide", '÷'),
    ("euro", '€'),
    ("pound", '£'),
    ("yen", '¥'),
    ("cent", '¢'),
    ("sect", '§'),
    ("para", '¶'),
    ("larr", '←'),
    ("rarr", '→'),
    ("uarr", '↑'),
    ("darr", '↓'),
];

/// `text` with its character references replaced.
pub fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        match reference(rest) {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The reference `text` starts with (at its `&`), and its length.
fn reference(text: &str) -> Option<(char, usize)> {
    let body = &text[1..];
    if let Some(number) = body.strip_prefix('#') {
        let (digits, radix, skip) = match number.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16, 3),
            None => (number, 10, 2),
        };
        let len = digits
            .bytes()
            .take_while(|b| (*b as char).is_digit(radix))
            .count();
        if len == 0 {
            return None;
        }
        let value = u32::from_str_radix(&digits[..len.min(8)], radix).unwrap_or(0xfffd);
        let c = match value {
            0 => '\u{fffd}',
            _ => char::from_u32(value).unwrap_or('\u{fffd}'),
        };
        let semicolon = usize::from(digits[len..].starts_with(';'));
        return Some((c, skip + len + semicolon));
    }
    let len = body.bytes().take_while(u8::is_ascii_alphanumeric).count();
    let name = &body[..len];
    let (_, c) = NAMED.iter().find(|(n, _)| *n == name)?;
    let semicolon = usize::from(body[len..].starts_with(';'));
    Some((*c, 1 + len + semicolon))
}
