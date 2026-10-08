//! The HID boot keyboard (HID 1.11 appendix B): 8-byte reports become
//! console bytes, the same ones the PS/2 keyboard sends (ADR-0029): ASCII
//! with Shift, Caps Lock and Ctrl, Enter as CR, Backspace as DEL; keys
//! without an ASCII meaning are ignored, but for the arrows, Home, End,
//! Delete and the page keys, which send `oceans_abi::display::KEY_*`
//! (ADR-0084), with Shift as a byte of their own (ADR-0095). Ctrl+Tab
//! sends the desktop's "next window" byte (ADR-0059); Ctrl+Shift+C, X and
//! V (and the Copy, Cut and Paste keys) the clipboard's (ADR-0095).

const LEFT_CTRL: u8 = 1 << 0;
const LEFT_SHIFT: u8 = 1 << 1;
const RIGHT_CTRL: u8 = 1 << 4;
const RIGHT_SHIFT: u8 = 1 << 5;

/// Usage IDs (HID Usage Tables §10).
const CAPS_LOCK: u8 = 0x39;
/// "Too many keys pressed": the report carries no key information.
const ERROR_ROLL_OVER: u8 = 0x01;

/// Usages 0x04–0x38: letters, digits, Enter … `/`.
const NORMAL: &[u8; 0x35] = b"abcdefghijklmnopqrstuvwxyz1234567890\r\x1b\x7f\t -=[]\\\0;'`,./";
const SHIFTED: &[u8; 0x35] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\r\x1b\x7f\t _+{}|\0:\"~<>?";
/// What Ctrl+Tab sends: `oceans_abi::display::KEY_NEXT_WINDOW` (ADR-0059).
pub const NEXT_WINDOW: u8 = 0x1e;
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
/// Keypad usages 0x54–0x63.
const KEYPAD: &[u8; 0x10] = b"/*-+\r1234567890.";

pub const REPORT_SIZE: usize = 8;

/// A boot keyboard's state between reports.
#[derive(Default)]
pub struct Keyboard {
    pressed: [u8; 6],
    caps: bool,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            pressed: [0; 6],
            caps: false,
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
        for &key in keys {
            if key == 0 || self.pressed.contains(&key) {
                continue;
            }
            if key == CAPS_LOCK {
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
    fn short_reports_are_ignored() {
        let mut keyboard = Keyboard::new();
        let mut out = Vec::new();
        keyboard.report(&[0, 0, 0x04], |byte| out.push(byte));
        assert!(out.is_empty());
    }
}
