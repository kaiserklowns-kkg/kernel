//! PS/2 keyboard (i8042), ADR-0029: key presses become console input bytes,
//! so the machine is usable with a screen and keyboard and no serial line.
//!
//! The controller is set to interrupt on IRQ 1 with scancode translation
//! (set 1). Make codes are mapped to ASCII with Shift, Caps Lock and Ctrl;
//! Enter sends CR and Backspace DEL, like a serial terminal. The arrows,
//! Home, End, Delete and the page keys send `display::KEY_*` (ADR-0084);
//! other keys without an ASCII meaning (function keys) are ignored. Ctrl+Tab
//! sends the desktop's "next window" byte (ADR-0059). With Shift the moving
//! keys send `KEY_SHIFTED` added, and Ctrl+Shift+C, X and V send
//! `KEY_COPY`, `KEY_CUT` and `KEY_PASTE` (ADR-0095), and Shift+Tab
//! `KEY_BACK_TAB` (ADR-0106). The volume keys
//! (`0xe0 0x20`, `0x2e`, `0x30`) send `KEY_MUTE`, `KEY_VOLUME_DOWN` and
//! `KEY_VOLUME_UP` (ADR-0101); the media keys (`0xe0 0x22`, `0x24`, `0x10`,
//! `0x19`) `KEY_PLAY_PAUSE`, `KEY_STOP`, `KEY_PREVIOUS` and `KEY_NEXT`
//! (ADR-0102).

use core::sync::atomic::{AtomicU8, Ordering};

use ::x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;
const OUTPUT_FULL: u8 = 1 << 0;
const INPUT_FULL: u8 = 1 << 1;
/// The byte came from the auxiliary (mouse) port.
const FROM_AUX: u8 = 1 << 5;

const SHIFT: u8 = 1 << 0;
const CTRL: u8 = 1 << 1;
const CAPS: u8 = 1 << 2;
const EXTENDED: u8 = 1 << 3;

static STATE: AtomicU8 = AtomicU8::new(0);

/// Set 1 make codes 0x00..0x3a.
const NORMAL: &[u8; 0x3a] =
    b"\0\x1b1234567890-=\x7f\tqwertyuiop[]\r\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 0x3a] =
    b"\0\x1b!@#$%^&*()_+\x7f\tQWERTYUIOP{}\r\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

fn read(port: u16) -> u8 {
    // SAFETY: the i8042 ports are owned by this driver; reading has no
    // effect beyond consuming a byte the controller offers.
    unsafe { Port::<u8>::new(port).read() }
}

fn write(port: u16, value: u8) {
    // SAFETY: as for `read`; commands below are the documented i8042 ones.
    unsafe { Port::<u8>::new(port).write(value) }
}

fn wait(mask: u8, set: bool) -> bool {
    for _ in 0..100_000 {
        if (read(STATUS) & mask != 0) == set {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Programs the controller for keyboard interrupts. `false` if there is no
/// controller.
pub fn init() -> bool {
    if read(STATUS) == 0xff {
        return false; // nothing on the bus
    }
    // Drop stale bytes.
    for _ in 0..32 {
        if read(STATUS) & OUTPUT_FULL == 0 {
            break;
        }
        let _ = read(DATA);
    }
    // Configuration byte: keyboard interrupt on, keyboard clock on, keep
    // translation to set 1.
    if !wait(INPUT_FULL, false) {
        return false;
    }
    write(COMMAND, 0x20);
    if !wait(OUTPUT_FULL, true) {
        return false;
    }
    let config = (read(DATA) | 0x01 | 0x40) & !0x10;
    if !wait(INPUT_FULL, false) {
        return false;
    }
    write(COMMAND, 0x60);
    if !wait(INPUT_FULL, false) {
        return false;
    }
    write(DATA, config);
    if !wait(INPUT_FULL, false) {
        return false;
    }
    write(COMMAND, 0xae); // enable the keyboard port
    true
}

/// Interrupt context: decodes waiting scancodes into bytes for `sink`.
pub fn drain(sink: fn(u8)) {
    for _ in 0..16 {
        let status = read(STATUS);
        if status & OUTPUT_FULL == 0 {
            return;
        }
        let code = read(DATA);
        if status & FROM_AUX == 0
            && let Some(byte) = translate(code)
        {
            sink(byte);
        }
    }
}

/// The extended (0xe0) make codes of the moving and editing keys.
fn navigation(key: u8) -> Option<u8> {
    use oceans_abi::display as d;
    Some(match key {
        0x48 => d::KEY_UP,
        0x50 => d::KEY_DOWN,
        0x4b => d::KEY_LEFT,
        0x4d => d::KEY_RIGHT,
        0x47 => d::KEY_HOME,
        0x4f => d::KEY_END,
        0x53 => d::KEY_DELETE,
        0x49 => d::KEY_PAGE_UP,
        0x51 => d::KEY_PAGE_DOWN,
        _ => return None,
    })
}

fn translate(code: u8) -> Option<u8> {
    let state = STATE.load(Ordering::Relaxed);
    if code == 0xe0 {
        STATE.store(state | EXTENDED, Ordering::Relaxed);
        return None;
    }
    let extended = state & EXTENDED != 0;
    let state = state & !EXTENDED;
    let released = code & 0x80 != 0;
    let key = code & 0x7f;
    // An extended Shift (0xe0 0x2a) is a "fake shift" some keyboards wrap
    // the moving keys in; it is no Shift the user pressed (ADR-0095).
    if extended && (key == 0x2a || key == 0x36) {
        STATE.store(state, Ordering::Relaxed);
        return None;
    }
    let modifier = match key {
        0x2a | 0x36 => SHIFT,
        0x1d => CTRL, // left or (extended) right control
        _ => 0,
    };
    if modifier != 0 {
        let state = if released {
            state & !modifier
        } else {
            state | modifier
        };
        STATE.store(state, Ordering::Relaxed);
        return None;
    }
    if key == 0x3a && !released {
        STATE.store(state ^ CAPS, Ordering::Relaxed);
        return None;
    }
    STATE.store(state, Ordering::Relaxed);
    if released {
        return None;
    }
    if extended {
        use oceans_abi::display as d;
        // The volume keys (ADR-0101), on keyboards and laptops' Fn keys.
        match key {
            0x20 => return Some(d::KEY_MUTE),
            0x2e => return Some(d::KEY_VOLUME_DOWN),
            0x30 => return Some(d::KEY_VOLUME_UP),
            // The media keys (ADR-0102).
            0x22 => return Some(d::KEY_PLAY_PAUSE),
            0x24 => return Some(d::KEY_STOP),
            0x10 => return Some(d::KEY_PREVIOUS),
            0x19 => return Some(d::KEY_NEXT),
            _ => {}
        }
        // Shift selects as it moves (ADR-0095); Shift+Delete is Delete.
        return navigation(key).map(|byte| {
            if state & SHIFT != 0 && byte != d::KEY_DELETE {
                byte | d::KEY_SHIFTED
            } else {
                byte
            }
        });
    }
    let index = usize::from(key);
    let mut byte = *NORMAL.get(index)?;
    if byte == 0 {
        return None;
    }
    let letter = byte.is_ascii_lowercase();
    if (state & SHIFT != 0) != (letter && state & CAPS != 0) {
        byte = SHIFTED[index];
    }
    // Ctrl+Shift+C, X and V: the clipboard's keys (ADR-0095).
    if state & CTRL != 0 && state & SHIFT != 0 {
        use oceans_abi::display as d;
        match byte.to_ascii_lowercase() {
            b'c' => return Some(d::KEY_COPY),
            b'x' => return Some(d::KEY_CUT),
            b'v' => return Some(d::KEY_PASTE),
            _ => {}
        }
    }
    if state & CTRL != 0 && byte.is_ascii_alphabetic() {
        byte = byte.to_ascii_lowercase() & 0x1f;
    }
    // Ctrl+Tab: the desktop's "next window" (ADR-0059).
    if state & CTRL != 0 && byte == b'\t' {
        byte = oceans_abi::display::KEY_NEXT_WINDOW;
    } else if state & SHIFT != 0 && byte == b'\t' {
        // Shift+Tab: the focus back to the previous widget (ADR-0106).
        byte = oceans_abi::display::KEY_BACK_TAB;
    }
    Some(byte)
}
