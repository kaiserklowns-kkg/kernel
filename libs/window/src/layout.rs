//! Keyboard layouts (ADR-0114): what a key types in the layout chosen.
//!
//! Keyboards send the bytes of the US layout (ADR-0029); the desktop turns
//! a printable one into the character of the layout in use. Thai is
//! Kedmanee (TIS 820-2538), as on Windows, macOS and Linux: each key's
//! character, and with Shift its other, by the key's US character.

/// The layouts to type in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    English,
    Thai,
}

impl Layout {
    /// The other one: Super+Space goes round them.
    pub fn next(self) -> Self {
        match self {
            Self::English => Self::Thai,
            Self::Thai => Self::English,
        }
    }

    /// As the menu bar shows it.
    pub fn short_name(self) -> &'static str {
        match self {
            Self::English => "EN",
            Self::Thai => "TH",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Thai => "Thai",
        }
    }

    /// What `byte` (a key's US character) types in this layout: `None`
    /// for a byte the layout leaves as it is (English: all of them; Thai:
    /// all but the printable ones, and the space).
    pub fn character(self, byte: u8) -> Option<char> {
        match self {
            Self::English => None,
            Self::Thai => thai(byte),
        }
    }
}

/// Kedmanee: the US characters, unshifted then shifted, and the Thai
/// characters on the same keys.
const US: &str = concat!(
    "`1234567890-=",
    "qwertyuiop[]\\",
    "asdfghjkl;'",
    "zxcvbnm,./",
    "~!@#$%^&*()_+",
    "QWERTYUIOP{}|",
    "ASDFGHJKL:\"",
    "ZXCVBNM<>?",
);
const THAI: &str = concat!(
    "_ๅ/-ภถุึคตจขช",
    "ๆไำพะัีรนยบลฃ",
    "ฟหกดเ้่าสวง",
    "ผปแอิืทมใฝ",
    "%+๑๒๓๔ู฿๕๖๗๘๙",
    "๐\"ฎฑธํ๊ณฯญฐ,ฅ",
    "ฤฆฏโฌ็๋ษศซ.",
    "()ฉฮฺ์?ฒฬฦ",
);

/// The Thai character on the key whose US character is `byte`.
pub fn thai(byte: u8) -> Option<char> {
    let index = US.bytes().position(|b| b == byte)?;
    THAI.chars().nth(index)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::string::String;

    #[test]
    fn the_tables_line_up() {
        assert_eq!(US.chars().count(), THAI.chars().count());
        assert_eq!(US.len(), US.chars().count(), "US is ASCII");
    }

    #[test]
    fn kedmanee_types_thai() {
        // สวัสดี: l ; y l f u.
        let typed: String = "l;ylfb".bytes().filter_map(thai).collect();
        assert_eq!(typed, "สวัสดิ");
        let typed: String = "l;ylf".bytes().filter_map(thai).chain(['ี']).collect();
        assert_eq!(typed, "สวัสดี");
        assert_eq!(thai(b'u'), Some('ี'));
        // ภาษาไทย: 4 k K k w m p (ษ with Shift).
        let word: String = "4kKkwmp".bytes().filter_map(thai).collect();
        assert_eq!(word, "ภาษาไทย");
        assert_eq!(thai(b'h'), Some('้'));
        // With Shift.
        assert_eq!(thai(b'A'), Some('ฤ'));
        assert_eq!(thai(b'@'), Some('๑'));
        assert_eq!(thai(b'\\'), Some('ฃ'));
        assert_eq!(thai(b'|'), Some('ฅ'));
        // Not printable keys, and the space, stay.
        assert_eq!(thai(b' '), None);
        assert_eq!(thai(b'\r'), None);
        assert_eq!(thai(0x80), None);
    }

    #[test]
    fn layouts_go_round() {
        assert_eq!(Layout::default().next(), Layout::Thai);
        assert_eq!(Layout::Thai.next(), Layout::English);
        assert_eq!(Layout::English.character(b'a'), None);
        assert_eq!(Layout::Thai.character(b'd'), Some('ก'));
        assert_eq!(Layout::Thai.short_name(), "TH");
    }
}
