//! The HID boot keyboard (HID 1.11 appendix B): 8-byte reports become
//! console bytes, the same ones the PS/2 keyboard sends (ADR-0029): ASCII
//! with Shift, Caps Lock and Ctrl, Enter as CR, Backspace as DEL; keys
//! without an ASCII meaning are ignored, but for the arrows, Home, End,
//! Delete and the page keys, which send `oceans_abi::display::KEY_*`
//! (ADR-0084), with Shift as a byte of their own (ADR-0095). Ctrl+Tab
//! sends the desktop's "next window" byte (ADR-0059); Ctrl+Shift+C, X and
//! V (and the Copy, Cut and Paste keys) the clipboard's (ADR-0095); Shift+Tab
//! the focus's "back" byte (ADR-0106). The
//! volume keys send the desktop's volume bytes (ADR-0101).

const LEFT_CTRL: u8 = 1 << 0;
const LEFT_SHIFT: u8 = 1 << 1;
const RIGHT_CTRL: u8 = 1 << 4;
const RIGHT_SHIFT: u8 = 1 << 5;
const LEFT_GUI: u8 = 1 << 3;
const RIGHT_GUI: u8 = 1 << 7;

/// Usage IDs (HID Usage Tables §10).
const CAPS_LOCK: u8 = 0x39;
/// "Too many keys pressed": the report carries no key information.
const ERROR_ROLL_OVER: u8 = 0x01;

/// Usages 0x04–0x38: letters, digits, Enter … `/`.
const NORMAL: &[u8; 0x35] = b"abcdefghijklmnopqrstuvwxyz1234567890\r\x1b\x7f\t -=[]\\\0;'`,./";
const SHIFTED: &[u8; 0x35] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\r\x1b\x7f\t _+{}|\0:\"~<>?";
/// What Ctrl+Tab sends: `oceans_abi::display::KEY_NEXT_WINDOW` (ADR-0059).
pub const NEXT_WINDOW: u8 = 0x1e;
/// What Shift+Tab sends: `oceans_abi::display::KEY_BACK_TAB` (ADR-0106).
pub const BACK_TAB: u8 = 0x8f;
/// The moving and editing keys, as `oceans_abi::display::KEY_*`
/// (ADR-0084), by usage: Home 0x4a, Page Up 0x4b, Delete 0x4c, End 0x4d,
/// Page Down 0x4e, Right 0x4f, Left 0x50, Down 0x51, Up 0x52.
const NAVIGATION: &[u8; 9] = &[0x84, 0x87, 0x86, 0x85, 0x88, 0x83, 0x82, 0x81, 0x80];
const DELETE: u8 = 0x86;
/// What Shift adds to a moving key: `display::KEY_SHIFTED` (ADR-0095).
const SHIFTED_MOVE: u8 = 0x10;
/// Ctrl+Shift+C, X, V and the Copy, Cut and Paste keys (usages 0x7c,
/// 0x7b, 0x7d): `display::KEY_COPY`, `KEY_CUT`, `KEY_PASTE` (ADR-0095).
const COPY: u8 = 0x89;
const CUT: u8 = 0x8a;
const PASTE: u8 = 0x8b;
/// Mute, Volume Up and Volume Down (usages 0x7f, 0x80, 0x81):
/// `display::KEY_MUTE`, `KEY_VOLUME_UP`, `KEY_VOLUME_DOWN` (ADR-0101).
const MUTE: u8 = 0x8c;
const VOLUME_DOWN: u8 = 0x8d;
const VOLUME_UP: u8 = 0x8e;
/// Keypad usages 0x54–0x63.
const KEYPAD: &[u8; 0x10] = b"/*-+\r1234567890.";
/// Super (the GUI key) with Left, Right, Up, Down and F:
/// `display::KEY_TILE_LEFT` … `KEY_FULL_SCREEN` (ADR-0107).
pub const TILE_LEFT: u8 = 0xa4;
pub const TILE_RIGHT: u8 = 0xa5;
pub const TILE_UP: u8 = 0xa6;
pub const TILE_DOWN: u8 = 0xa7;
pub const FULL_SCREEN: u8 = 0xa8;
/// Super pressed and let go alone: `display::KEY_SEARCH` (ADR-0108).
pub const SEARCH: u8 = 0xa9;

/// What a key pressed with Super sends: arranging windows; other keys
/// with Super send nothing.
fn arrange(key: u8) -> Option<u8> {
    match key {
        0x50 => Some(TILE_LEFT),
        0x4f => Some(TILE_RIGHT),
        0x52 => Some(TILE_UP),
        0x51 => Some(TILE_DOWN),
        0x09 => Some(FULL_SCREEN),
        _ => None,
    }
}

pub const REPORT_SIZE: usize = 8;

/// A boot keyboard's state between reports.
#[derive(Default)]
pub struct Keyboard {
    pressed: [u8; 6],
    caps: bool,
    /// Super is held, and whether another key was pressed meanwhile.
    super_held: bool,
    super_used: bool,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            pressed: [0; 6],
            caps: false,
            super_held: false,
            super_used: false,
        }
    }

    /// Feeds one report; calls `emit` with the byte of each newly pressed
    /// key, in report order.
    pub fn report(&mut self, report: &[u8], mut emit: impl FnMut(u8)) {
        let Some(report) = report.get(..REPORT_SIZE) else {
            return;
        };
        let modifiers = report[0];
        let keys = &report[2..8];
        if keys.contains(&ERROR_ROLL_OVER) {
            return;
        }
        let shift = modifiers & (LEFT_SHIFT | RIGHT_SHIFT) != 0;
        let ctrl = modifiers & (LEFT_CTRL | RIGHT_CTRL) != 0;
        let super_key = modifiers & (LEFT_GUI | RIGHT_GUI) != 0;
        if super_key && !self.super_held {
            self.super_used = false;
        }
        if super_key && keys.iter().any(|&k| k != 0 && !self.pressed.contains(&k)) {
            self.super_used = true;
        }
        // Super let go with no key pressed meanwhile: the launcher's search
        // (ADR-0108).
        if !super_key && self.super_held && !self.super_used {
            emit(SEARCH);
        }
        self.super_held = super_key;
        for &key in keys {
            if key == 0 || self.pressed.contains(&key) {
                continue;
            }
            if super_key {
                if let Some(byte) = arrange(key) {
                    emit(byte);
                }
            } else if key == CAPS_LOCK {
                self.caps = !self.caps;
            } else if let Some(byte) = self.translate(key, shift, ctrl) {
                emit(byte);
            }
        }
        self.pressed.copy_from_slice(keys);
    }

    fn translate(&self, key: u8, shift: bool, ctrl: bool) -> Option<u8> {
        let byte = match key {
            0x04..=0x38 => {
                let index = usize::from(key - 0x04);
                let letter = NORMAL[index].is_ascii_lowercase();
                if shift != (letter && self.caps) {
                    SHIFTED[index]
                } else {
                    NORMAL[index]
                }
            }
            0x4a..=0x52 => {
                let byte = NAVIGATION[usize::from(key - 0x4a)];
                return Some(if shift && byte != DELETE {
                    byte | SHIFTED_MOVE
                } else {
                    byte
                });
            }
            0x54..=0x63 => KEYPAD[usize::from(key - 0x54)],
            0x7b => return Some(CUT),
            0x7c => return Some(COPY),
            0x7d => return Some(PASTE),
            0x7f => return Some(MUTE),
            0x80 => return Some(VOLUME_UP),
            0x81 => return Some(VOLUME_DOWN),
            _ => 0,
        };
        if byte == 0 {
            return None;
        }
        if ctrl && shift {
            match byte.to_ascii_lowercase() {
                b'c' => return Some(COPY),
                b'x' => return Some(CUT),
                b'v' => return Some(PASTE),
                _ => {}
            }
        }
        if ctrl && byte.is_ascii_alphabetic() {
            return Some(byte.to_ascii_lowercase() & 0x1f);
        }
        if ctrl && byte == b'\t' {
            return Some(NEXT_WINDOW);
        }
        if shift && byte == b'\t' {
            return Some(BACK_TAB);
        }
        Some(byte)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn feed(keyboard: &mut Keyboard, reports: &[[u8; 8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for report in reports {
            keyboard.report(report, |byte| out.push(byte));
        }
        out
    }

    const fn keys(modifiers: u8, pressed: [u8; 6]) -> [u8; 8] {
        [
            modifiers, 0, pressed[0], pressed[1], pressed[2], pressed[3], pressed[4], pressed[5],
        ]
    }

    #[test]
    fn types_words_with_shift_and_enter() {
        let mut keyboard = Keyboard::new();
        // "Hi!" then Enter: h with shift, release, i, 1 with right shift.
        let out = feed(
            &mut keyboard,
            &[
                keys(LEFT_SHIFT, [0x0b, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(0, [0x0c, 0, 0, 0, 0, 0]),
                keys(RIGHT_SHIFT, [0x1e, 0, 0, 0, 0, 0]),
                keys(0, [0x28, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
            ],
        );
        assert_eq!(out, b"Hi!\r");
    }

    #[test]
    fn ctrl_tab_is_next_window_and_tab_stays_tab() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(0, [0x2b, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(LEFT_CTRL, [0x2b, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(RIGHT_CTRL, [0x2b, 0, 0, 0, 0, 0]),
            ],
        );
        assert_eq!(out, [b'\t', NEXT_WINDOW, NEXT_WINDOW]);
    }

    #[test]
    fn shift_tab_is_back_tab() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(LEFT_SHIFT, [0x2b, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(RIGHT_SHIFT, [0x2b, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(LEFT_CTRL | LEFT_SHIFT, [0x2b, 0, 0, 0, 0, 0]),
            ],
        );
        assert_eq!(out, [BACK_TAB, BACK_TAB, NEXT_WINDOW]);
    }

    #[test]
    fn super_alone_is_search() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                // Alone (held over several reports): search, once.
                keys(LEFT_GUI, [0; 6]),
                keys(LEFT_GUI, [0; 6]),
                keys(0, [0; 6]),
                // With an arrow: no search when it is let go.
                keys(RIGHT_GUI, [0; 6]),
                keys(RIGHT_GUI, [0x50, 0, 0, 0, 0, 0]),
                keys(RIGHT_GUI, [0; 6]),
                keys(0, [0; 6]),
                // Alone again.
                keys(LEFT_GUI, [0; 6]),
                keys(0, [0; 6]),
            ],
        );
        assert_eq!(out, [SEARCH, TILE_LEFT, SEARCH]);
    }

    #[test]
    fn super_with_the_arrows_and_f_arranges_windows() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(LEFT_GUI, [0x50, 0, 0, 0, 0, 0]),
                keys(LEFT_GUI, [0; 6]),
                keys(RIGHT_GUI, [0x4f, 0, 0, 0, 0, 0]),
                keys(RIGHT_GUI, [0; 6]),
                keys(LEFT_GUI, [0x52, 0, 0, 0, 0, 0]),
                keys(LEFT_GUI, [0x51, 0, 0, 0, 0, 0]),
                keys(LEFT_GUI, [0x09, 0, 0, 0, 0, 0]),
                // Other keys with Super: nothing; without it, as ever.
                keys(LEFT_GUI, [0x04, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(0, [0x50, 0x09, 0, 0, 0, 0]),
            ],
        );
        assert_eq!(
            out,
            [
                TILE_LEFT,
                TILE_RIGHT,
                TILE_UP,
                TILE_DOWN,
                FULL_SCREEN,
                0x82,
                b'f'
            ]
        );
    }

    #[test]
    fn held_keys_are_reported_once_and_rollover_is_ignored() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(0, [0x04, 0, 0, 0, 0, 0]),
                keys(0, [0x04, 0x05, 0, 0, 0, 0]),
                keys(0, [ERROR_ROLL_OVER; 6]),
                keys(0, [0x05, 0, 0, 0, 0, 0]),
                keys(0, [0x05, 0x04, 0, 0, 0, 0]),
            ],
        );
        assert_eq!(out, b"aba");
    }

    #[test]
    fn caps_lock_ctrl_and_the_keypad() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(0, [CAPS_LOCK, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(0, [0x04, 0, 0, 0, 0, 0]),
                keys(LEFT_SHIFT, [0x05, 0, 0, 0, 0, 0]),
                keys(LEFT_SHIFT, [0x1f, 0, 0, 0, 0, 0]),
                keys(LEFT_CTRL, [0x06, 0, 0, 0, 0, 0]),
                keys(0, [0x59, 0x2a, 0x3a, 0, 0, 0]),
                keys(0, [0; 6]),
            ],
        );
        // Caps: "A", Shift inverts it: "b", Shift+2: "@", Ctrl+C: 0x03,
        // keypad 1, Backspace, F1 ignored.
        assert_eq!(out, b"Ab@\x031\x7f");
    }

    #[test]
    fn moving_keys_send_their_bytes() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(0, [0x52, 0, 0, 0, 0, 0]),
                keys(0, [0x51, 0, 0, 0, 0, 0]),
                keys(0, [0x50, 0, 0, 0, 0, 0]),
                keys(0, [0x4f, 0, 0, 0, 0, 0]),
                keys(0, [0x4a, 0, 0, 0, 0, 0]),
                keys(0, [0x4d, 0, 0, 0, 0, 0]),
                keys(LEFT_CTRL, [0x4c, 0, 0, 0, 0, 0]),
                keys(0, [0x4b, 0, 0, 0, 0, 0]),
                keys(0, [0x4e, 0, 0, 0, 0, 0]),
                // Insert (0x49) has no byte.
                keys(0, [0x49, 0, 0, 0, 0, 0]),
            ],
        );
        // Up, Down, Left, Right, Home, End, Delete, Page Up, Page Down.
        assert_eq!(out, [0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88]);
    }

    #[test]
    fn shift_selects_and_ctrl_shift_copies_and_pastes() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                // Shift+End, Shift+Left, Shift+Delete (Delete).
                keys(LEFT_SHIFT, [0x4d, 0, 0, 0, 0, 0]),
                keys(RIGHT_SHIFT, [0x50, 0, 0, 0, 0, 0]),
                keys(LEFT_SHIFT, [0x4c, 0, 0, 0, 0, 0]),
                // Ctrl+Shift+C, X, V; Ctrl+V; the Paste key.
                keys(LEFT_CTRL | LEFT_SHIFT, [0x06, 0, 0, 0, 0, 0]),
                keys(LEFT_CTRL | LEFT_SHIFT, [0x1b, 0, 0, 0, 0, 0]),
                keys(RIGHT_CTRL | RIGHT_SHIFT, [0x19, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(LEFT_CTRL, [0x19, 0, 0, 0, 0, 0]),
                keys(0, [0x7d, 0, 0, 0, 0, 0]),
            ],
        );
        assert_eq!(out, [0x95, 0x92, 0x86, 0x89, 0x8a, 0x8b, 0x16, 0x8b]);
    }

    #[test]
    fn the_volume_keys_send_their_bytes() {
        let mut keyboard = Keyboard::new();
        let out = feed(
            &mut keyboard,
            &[
                keys(0, [0x7f, 0, 0, 0, 0, 0]),
                keys(0, [0; 6]),
                keys(0, [0x81, 0, 0, 0, 0, 0]),
                // With Shift or Ctrl, the same.
                keys(LEFT_SHIFT, [0x80, 0, 0, 0, 0, 0]),
                keys(LEFT_CTRL, [0x80, 0x81, 0, 0, 0, 0]),
            ],
        );
        // Mute, Down, Up; the held Up is not sent again, Down is.
        assert_eq!(out, [0x8c, 0x8d, 0x8e, 0x8d]);
    }

    #[test]
    fn short_reports_are_ignored() {
        let mut keyboard = Keyboard::new();
        let mut out = Vec::new();
        keyboard.report(&[0, 0, 0x04], |byte| out.push(byte));
        assert!(out.is_empty());
    }
}
