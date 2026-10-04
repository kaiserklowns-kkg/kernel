//! PS/2 keyboard (i8042), ADR-0029: key presses become console input bytes,
//! so the machine is usable with a screen and keyboard and no serial line.
//!
//! The controller is set to interrupt on IRQ 1 with scancode translation
//! (set 1). Make codes are mapped to ASCII with Shift, Caps Lock and Ctrl;
//! Enter sends CR and Backspace DEL, like a serial terminal. Keys without
//! an ASCII meaning (arrows, function keys) are ignored for now; Ctrl+Tab
//! sends the desktop's "next window" byte (ADR-0059).

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
    // Releases, extended keys (arrows, ...) and keys past the table.
    if released || extended {
        return None;
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
    if state & CTRL != 0 && byte.is_ascii_alphabetic() {
        byte = byte.to_ascii_lowercase() & 0x1f;
    }
    // Ctrl+Tab: the desktop's "next window" (ADR-0059).
    if state & CTRL != 0 && byte == b'\t' {
        byte = oceans_abi::display::KEY_NEXT_WINDOW;
    }
    Some(byte)
}
