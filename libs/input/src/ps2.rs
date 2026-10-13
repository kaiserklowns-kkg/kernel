//! PS/2 mice, and laptop touchpads in their PS/2 mouse mode (ADR-0132):
//! the commands that set one up and the packets it sends, decoded into
//! [`Sample`]s.
//!
//! A standard mouse sends 3-byte packets: buttons and the signs, then X,
//! then Y (up positive). One that answers the IntelliMouse knock (sample
//! rates 200, 100, 80, then its ID is 3) sends a fourth byte, the wheel.
//! Byte 0 always has bit 3 set: a stream that lost a byte finds its packets
//! again from there. Motion that overflowed is dropped, not guessed.

use crate::Sample;

/// Commands to the device (sent through the controller's `0xd4`).
pub mod command {
    pub const SET_DEFAULTS: u8 = 0xf6;
    pub const SET_SAMPLE_RATE: u8 = 0xf3;
    pub const GET_ID: u8 = 0xf2;
    pub const ENABLE_REPORTING: u8 = 0xf4;
    pub const DISABLE_REPORTING: u8 = 0xf5;
    /// The device's answer to a command.
    pub const ACK: u8 = 0xfa;
}

/// The sample rates that, set in turn, ask for the wheel.
pub const WHEEL_KNOCK: [u8; 3] = [200, 100, 80];
/// The ID a mouse with a wheel answers after the knock.
pub const WHEEL_ID: u8 = 3;

/// Bits of a packet's first byte.
const LEFT: u8 = 1 << 0;
const RIGHT: u8 = 1 << 1;
const MIDDLE: u8 = 1 << 2;
const ALWAYS: u8 = 1 << 3;
const X_SIGN: u8 = 1 << 4;
const Y_SIGN: u8 = 1 << 5;
const X_OVERFLOW: u8 = 1 << 6;
const Y_OVERFLOW: u8 = 1 << 7;

/// Bytes into packets.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    len: usize,
    bytes: [u8; 4],
    at: usize,
}

impl Decoder {
    /// For a mouse with a wheel (4-byte packets) or without (3).
    pub const fn new(wheel: bool) -> Self {
        Self {
            len: if wheel { 4 } else { 3 },
            bytes: [0; 4],
            at: 0,
        }
    }

    /// Feeds one byte from the device; a whole packet gives its sample.
    pub fn feed(&mut self, byte: u8) -> Option<Sample> {
        // A first byte without its always-set bit is not one: wait for
        // the next that could be.
        if self.at == 0 && byte & ALWAYS == 0 {
            return None;
        }
        self.bytes[self.at] = byte;
        self.at += 1;
        if self.at < self.len {
            return None;
        }
        self.at = 0;
        let [flags, x, y, z] = self.bytes;
        let axis = |value: u8, negative: bool, overflow: bool| -> i32 {
            if overflow {
                0
            } else if negative {
                i32::from(value) - 256
            } else {
                i32::from(value)
            }
        };
        let dx = axis(x, flags & X_SIGN != 0, flags & X_OVERFLOW != 0);
        let dy = axis(y, flags & Y_SIGN != 0, flags & Y_OVERFLOW != 0);
        // The wheel's count is toward the user positive; a sample's is
        // away from the user (scrolling up).
        let wheel = if self.len == 4 {
            -i32::from(z as i8)
        } else {
            0
        };
        let mut buttons = 0;
        for (bit, button) in [(LEFT, 0), (RIGHT, 1), (MIDDLE, 2)] {
            if flags & bit != 0 {
                buttons |= 1 << button;
            }
        }
        Some(Sample {
            buttons,
            x: dx,
            // Up is positive on the wire, down on the screen.
            y: -dy,
            wheel,
            pan: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(decoder: &mut Decoder, bytes: &[u8]) -> Option<Sample> {
        bytes.iter().fold(None, |_, &b| decoder.feed(b))
    }

    #[test]
    fn motion_and_buttons() {
        let mut d = Decoder::new(false);
        // Right 5, up 3, left button.
        let s = packet(&mut d, &[0x09, 5, 3]).unwrap();
        assert_eq!((s.x, s.y, s.buttons, s.wheel), (5, -3, 1, 0));
        // Left 2 (sign), down 4 (sign), right and middle buttons.
        let s = packet(&mut d, &[0x08 | 0x10 | 0x20 | 0x02 | 0x04, 0xfe, 0xfc]).unwrap();
        assert_eq!((s.x, s.y, s.buttons), (-2, 4, 0b110));
        // Overflow: that axis dropped.
        let s = packet(&mut d, &[0x08 | 0x40, 0xff, 7]).unwrap();
        assert_eq!((s.x, s.y), (0, -7));
    }

    #[test]
    fn the_wheel() {
        let mut d = Decoder::new(true);
        assert_eq!(packet(&mut d, &[0x08, 0, 0]), None, "a fourth byte to come");
        let s = d.feed(0xff).unwrap();
        assert_eq!(s.wheel, 1, "toward the user is -1 on the wire: scroll up");
        let s = packet(&mut d, &[0x08, 0, 0, 2]).unwrap();
        assert_eq!(s.wheel, -2);
    }

    #[test]
    fn a_lost_byte_is_found_again() {
        let mut d = Decoder::new(false);
        // A stray byte without bit 3 is skipped, the packet after it read.
        assert_eq!(d.feed(0x05), None);
        let s = packet(&mut d, &[0x08, 1, 0]).unwrap();
        assert_eq!((s.x, s.y), (1, 0));
    }
}
