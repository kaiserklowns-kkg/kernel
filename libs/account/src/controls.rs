//! Control Center's controls (ADR-0151): each has an id, and Core keeps,
//! for each account, the ones it shows in the order it placed them.

use alloc::string::String;
use alloc::vec::Vec;

pub const NETWORK: u8 = 0;
pub const NOW_PLAYING: u8 = 1;
pub const DARK_MODE: u8 = 2;
pub const KEYBOARD: u8 = 3;
pub const LOCK_SCREEN: u8 = 4;
pub const SETTINGS: u8 = 5;
/// The screen's brightness (ADR-0147): shown only where it can be set.
pub const DISPLAY: u8 = 6;
pub const SOUND: u8 = 7;
pub const ACCENT: u8 = 8;
pub const LAUNCHPAD: u8 = 9;
pub const NOTIFICATIONS: u8 = 10;
/// The administrator's console (ADR-0115): offered only to it.
pub const TERMINAL: u8 = 11;
/// How many there are: every id is below.
pub const COUNT: u8 = 12;

/// What Control Center shows until the account chooses, as macOS's
/// starts: the network and what plays at the top, then Dark Mode, the
/// keyboard, Lock Screen and Settings, then the sliders.
pub const DEFAULT: [u8; 8] = [
    NETWORK,
    NOW_PLAYING,
    DARK_MODE,
    KEYBOARD,
    LOCK_SCREEN,
    SETTINGS,
    DISPLAY,
    SOUND,
];

/// Whether `ids` is a list `SET_CONTROLS` takes: known ids, each at most
/// once.
pub fn valid(ids: &[u8]) -> bool {
    ids.len() <= usize::from(COUNT)
        && ids
            .iter()
            .enumerate()
            .all(|(i, &id)| id < COUNT && !ids[..i].contains(&id))
}

/// The list as Core keeps it in a file: the ids in decimal, by spaces.
pub fn to_text(ids: &[u8]) -> String {
    let words: Vec<String> = ids.iter().map(|id| alloc::format!("{id}")).collect();
    let mut text = words.join(" ");
    text.push('\n');
    text
}

/// The list kept in a file, if it is one.
pub fn parse(text: &[u8]) -> Option<Vec<u8>> {
    let text = core::str::from_utf8(text).ok()?;
    let ids = text
        .split_ascii_whitespace()
        .map(|word| word.parse::<u8>().ok())
        .collect::<Option<Vec<u8>>>()?;
    valid(&ids).then_some(ids)
}

/// `ids` with `id` put where `onto` is (at the end without one), as a
/// control dragged onto another lands: before it when it came from after
/// it or from outside, after it when it came from before it.
pub fn moved(ids: &[u8], id: u8, onto: Option<u8>) -> Vec<u8> {
    let at = onto
        .and_then(|onto| ids.iter().position(|&i| i == onto))
        .unwrap_or(ids.len());
    let mut out: Vec<u8> = ids.iter().copied().filter(|&i| i != id).collect();
    out.insert(at.min(out.len()), id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists() {
        assert!(valid(&DEFAULT));
        assert!(valid(&[]));
        assert!(!valid(&[NETWORK, NETWORK]));
        assert!(!valid(&[COUNT]));
        assert_eq!(
            parse(b"0 1 7\n"),
            Some(alloc::vec![NETWORK, NOW_PLAYING, SOUND])
        );
        assert_eq!(parse(b"\n"), Some(alloc::vec![]));
        assert_eq!(parse(b"0 0"), None);
        assert_eq!(parse(b"0 x"), None);
        assert_eq!(parse(b"0 1 2 3 4 5 6 7 8 9 10 11 0"), None);
        assert_eq!(
            parse(to_text(&DEFAULT).as_bytes()).as_deref(),
            Some(&DEFAULT[..])
        );
    }

    #[test]
    fn moving() {
        let ids = [NETWORK, NOW_PLAYING, DARK_MODE, KEYBOARD];
        assert_eq!(
            moved(&ids, KEYBOARD, Some(NOW_PLAYING)),
            [NETWORK, KEYBOARD, NOW_PLAYING, DARK_MODE]
        );
        assert_eq!(
            moved(&ids, NETWORK, Some(DARK_MODE)),
            [NOW_PLAYING, DARK_MODE, NETWORK, KEYBOARD]
        );
        assert_eq!(
            moved(&ids, NOW_PLAYING, None),
            [NETWORK, DARK_MODE, KEYBOARD, NOW_PLAYING]
        );
        // A new one, from the gallery: before the one dropped on.
        assert_eq!(
            moved(&ids, SOUND, Some(DARK_MODE)),
            [NETWORK, NOW_PLAYING, SOUND, DARK_MODE, KEYBOARD]
        );
        assert_eq!(moved(&ids, KEYBOARD, Some(KEYBOARD)), ids);
    }
}
