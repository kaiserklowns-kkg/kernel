//! Input events for Oceans (ADR-0042), host-tested and without allocation.
//!
//! - [`Event`]: what an input service delivers to its clients: relative
//!   motion, an absolute position with its range, a button going down or
//!   up, a wheel turning. Every event carries the time it happened and the
//!   device it came from.
//! - The wire format: [`Event::encode`] / [`Event::decode`], fixed
//!   [`EVENT_SIZE`]-byte records, so several fit one IPC message.
//! - [`Tracker`]: turns a pointer's successive [`Sample`]s (what a device
//!   reports: buttons held, motion or position, wheel turns) into events,
//!   reporting only what changed.
//!
//! Devices are untrusted: nothing here panics on any input, and decoding
//! rejects unknown event kinds instead of guessing.

#![no_std]

/// Bytes per encoded event.
pub const EVENT_SIZE: usize = 32;

/// Buttons a pointer may have: bit `n - 1` of [`Sample::buttons`] is
/// button `n` (HID button usages: 1 primary, 2 secondary, 3 tertiary).
pub const MAX_BUTTONS: u8 = 32;

/// One input event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// Milliseconds since boot (the system clock, ADR-0020).
    pub time_ms: u64,
    /// The source device, numbered by the input service in the order
    /// devices arrive (1, 2, …, wrapping after 255; never 0).
    pub device: u8,
    pub kind: Kind,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The pointer moved by `dx`, `dy` device units (a mouse); positive is
    /// right and down.
    Motion { dx: i32, dy: i32 },
    /// The pointer is at `x`, `y`, in `0..=x_max` and `0..=y_max` (a
    /// tablet or touch screen); scale to the screen by the range.
    Absolute {
        x: u32,
        y: u32,
        x_max: u32,
        y_max: u32,
    },
    /// Button `button` (1 to [`MAX_BUTTONS`]) went down or up.
    Button { button: u8, pressed: bool },
    /// Wheel detents: `vertical` positive away from the user (scroll up),
    /// `horizontal` positive to the right.
    Wheel { vertical: i32, horizontal: i32 },
}

mod code {
    pub const MOTION: u8 = 1;
    pub const ABSOLUTE: u8 = 2;
    pub const BUTTON: u8 = 3;
    pub const WHEEL: u8 = 4;
}

impl Event {
    /// `[time u64][kind u8][device u8][button u8][pressed u8][a u32][b
    /// u32][c u32][d u32][0; 4]`, little-endian; `a`–`d` are the kind's
    /// numbers in declaration order (signed ones as two's complement).
    pub fn encode(&self) -> [u8; EVENT_SIZE] {
        let mut out = [0u8; EVENT_SIZE];
        out[..8].copy_from_slice(&self.time_ms.to_le_bytes());
        out[9] = self.device;
        let (kind, numbers) = match self.kind {
            Kind::Motion { dx, dy } => (code::MOTION, [dx as u32, dy as u32, 0, 0]),
            Kind::Absolute { x, y, x_max, y_max } => (code::ABSOLUTE, [x, y, x_max, y_max]),
            Kind::Button { button, pressed } => {
                out[10] = button;
                out[11] = u8::from(pressed);
                (code::BUTTON, [0; 4])
            }
            Kind::Wheel {
                vertical,
                horizontal,
            } => (code::WHEEL, [vertical as u32, horizontal as u32, 0, 0]),
        };
        out[8] = kind;
        for (i, number) in numbers.iter().enumerate() {
            out[12 + 4 * i..16 + 4 * i].copy_from_slice(&number.to_le_bytes());
        }
        out
    }

    /// The inverse of [`Event::encode`]; `None` for a short record, an
    /// unknown kind, or values no encoder produces.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; EVENT_SIZE] = bytes.get(..EVENT_SIZE)?.try_into().ok()?;
        let number = |i: usize| {
            let at = 12 + 4 * i;
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let kind = match bytes[8] {
            code::MOTION => Kind::Motion {
                dx: number(0) as i32,
                dy: number(1) as i32,
            },
            code::ABSOLUTE => {
                let (x, y, x_max, y_max) = (number(0), number(1), number(2), number(3));
                if x > x_max || y > y_max {
                    return None;
                }
                Kind::Absolute { x, y, x_max, y_max }
            }
            code::BUTTON => {
                let button = bytes[10];
                if button == 0 || button > MAX_BUTTONS || bytes[11] > 1 {
                    return None;
                }
                Kind::Button {
                    button,
                    pressed: bytes[11] == 1,
                }
            }
            code::WHEEL => Kind::Wheel {
                vertical: number(0) as i32,
                horizontal: number(1) as i32,
            },
            _ => return None,
        };
        Some(Self {
            time_ms: u64::from_le_bytes(bytes[..8].try_into().ok()?),
            device: bytes[9],
            kind,
        })
    }
}

/// A conventional name for a button, if it has one.
pub fn button_name(button: u8) -> Option<&'static str> {
    Some(match button {
        1 => "left",
        2 => "right",
        3 => "middle",
        4 => "back",
        5 => "forward",
        _ => return None,
    })
}

/// How a pointer reports its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axes {
    /// Motion since the last report (a mouse).
    Relative,
    /// A position in `0..=x_max`, `0..=y_max` (a tablet).
    Absolute { x_max: u32, y_max: u32 },
}

/// One report of a pointer, decoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// Buttons held: bit `n - 1` is button `n`.
    pub buttons: u32,
    /// Motion (relative axes) or position (absolute axes, already within
    /// the range).
    pub x: i32,
    pub y: i32,
    /// Wheel detents since the last report.
    pub wheel: i32,
    pub pan: i32,
}

/// A pointer's state between samples.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tracker {
    buttons: u32,
    position: Option<(u32, u32)>,
}

impl Tracker {
    pub const fn new() -> Self {
        Self {
            buttons: 0,
            position: None,
        }
    }

    /// Feeds one sample; calls `emit` for each change, in this order:
    /// motion or position (so a click lands where the pointer now is),
    /// buttons in ascending order, the wheel. An absolute pointer's first
    /// sample always reports its position.
    pub fn update(&mut self, sample: &Sample, axes: Axes, mut emit: impl FnMut(Kind)) {
        match axes {
            Axes::Relative => {
                if sample.x != 0 || sample.y != 0 {
                    emit(Kind::Motion {
                        dx: sample.x,
                        dy: sample.y,
                    });
                }
            }
            Axes::Absolute { x_max, y_max } => {
                let x = (sample.x.max(0) as u32).min(x_max);
                let y = (sample.y.max(0) as u32).min(y_max);
                if self.position != Some((x, y)) {
                    self.position = Some((x, y));
                    emit(Kind::Absolute { x, y, x_max, y_max });
                }
            }
        }
        let changed = self.buttons ^ sample.buttons;
        for bit in 0..u32::from(MAX_BUTTONS) {
            if changed & 1 << bit != 0 {
                emit(Kind::Button {
                    button: bit as u8 + 1,
                    pressed: sample.buttons & 1 << bit != 0,
                });
            }
        }
        self.buttons = sample.buttons;
        if sample.wheel != 0 || sample.pan != 0 {
            emit(Kind::Wheel {
                vertical: sample.wheel,
                horizontal: sample.pan,
            });
        }
    }

    /// The buttons held now (bit `n - 1` is button `n`).
    pub fn buttons(&self) -> u32 {
        self.buttons
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn track(tracker: &mut Tracker, sample: Sample, axes: Axes) -> Vec<Kind> {
        let mut out = Vec::new();
        tracker.update(&sample, axes, |kind| out.push(kind));
        out
    }

    #[test]
    fn events_round_trip() {
        let kinds = [
            Kind::Motion { dx: -3, dy: 127 },
            Kind::Absolute {
                x: 100,
                y: 32767,
                x_max: 32767,
                y_max: 32767,
            },
            Kind::Button {
                button: 1,
                pressed: true,
            },
            Kind::Button {
                button: 32,
                pressed: false,
            },
            Kind::Wheel {
                vertical: -1,
                horizontal: 2,
            },
        ];
        for (i, kind) in kinds.into_iter().enumerate() {
            let event = Event {
                time_ms: 0x0102_0304_0506_0708 + i as u64,
                device: i as u8 + 1,
                kind,
            };
            let bytes = event.encode();
            assert_eq!(Event::decode(&bytes), Some(event));
            assert!(Event::decode(&bytes[..EVENT_SIZE - 1]).is_none());
        }
        assert!(EVENT_SIZE * 7 + 4 <= 248, "a reply carries seven events");
    }

    #[test]
    fn decoding_rejects_what_no_encoder_makes() {
        let button = Event {
            time_ms: 5,
            device: 1,
            kind: Kind::Button {
                button: 2,
                pressed: true,
            },
        }
        .encode();
        let mut bad = button;
        bad[8] = 0;
        assert!(Event::decode(&bad).is_none(), "unknown kind");
        bad = button;
        bad[10] = 0;
        assert!(Event::decode(&bad).is_none(), "button 0");
        bad[10] = MAX_BUTTONS + 1;
        assert!(Event::decode(&bad).is_none(), "button 33");
        bad = button;
        bad[11] = 2;
        assert!(Event::decode(&bad).is_none(), "pressed is a flag");
        let mut absolute = Event {
            time_ms: 5,
            device: 1,
            kind: Kind::Absolute {
                x: 10,
                y: 10,
                x_max: 10,
                y_max: 10,
            },
        }
        .encode();
        assert!(Event::decode(&absolute).is_some());
        absolute[12] = 11;
        assert!(Event::decode(&absolute).is_none(), "outside the range");
    }

    #[test]
    fn relative_samples_become_motion_buttons_and_wheel() {
        let mut tracker = Tracker::new();
        let axes = Axes::Relative;
        assert!(track(&mut tracker, Sample::default(), axes).is_empty());
        assert_eq!(
            track(
                &mut tracker,
                Sample {
                    buttons: 0b101,
                    x: 10,
                    y: -5,
                    ..Sample::default()
                },
                axes
            ),
            [
                Kind::Motion { dx: 10, dy: -5 },
                Kind::Button {
                    button: 1,
                    pressed: true
                },
                Kind::Button {
                    button: 3,
                    pressed: true
                },
            ]
        );
        // Held buttons are not repeated; the wheel and pan come last.
        assert_eq!(
            track(
                &mut tracker,
                Sample {
                    buttons: 0b100,
                    wheel: 1,
                    pan: -2,
                    ..Sample::default()
                },
                axes
            ),
            [
                Kind::Button {
                    button: 1,
                    pressed: false
                },
                Kind::Wheel {
                    vertical: 1,
                    horizontal: -2
                },
            ]
        );
        assert_eq!(tracker.buttons(), 0b100);
        assert_eq!(
            track(
                &mut tracker,
                Sample {
                    buttons: 1 << 31,
                    ..Sample::default()
                },
                axes
            ),
            [
                Kind::Button {
                    button: 3,
                    pressed: false
                },
                Kind::Button {
                    button: 32,
                    pressed: true
                },
            ]
        );
    }

    #[test]
    fn absolute_samples_report_positions_once_and_clamp() {
        let mut tracker = Tracker::new();
        let axes = Axes::Absolute {
            x_max: 100,
            y_max: 50,
        };
        let at = |x, y| Kind::Absolute {
            x,
            y,
            x_max: 100,
            y_max: 50,
        };
        // The first sample gives the position, even at the origin.
        assert_eq!(track(&mut tracker, Sample::default(), axes), [at(0, 0)]);
        assert!(track(&mut tracker, Sample::default(), axes).is_empty());
        assert_eq!(
            track(
                &mut tracker,
                Sample {
                    buttons: 0b10,
                    x: 40,
                    y: 20,
                    ..Sample::default()
                },
                axes
            ),
            [
                at(40, 20),
                Kind::Button {
                    button: 2,
                    pressed: true
                }
            ]
        );
        // Out-of-range positions are clamped, never passed on.
        assert_eq!(
            track(
                &mut tracker,
                Sample {
                    buttons: 0b10,
                    x: 500,
                    y: -7,
                    ..Sample::default()
                },
                axes
            ),
            [at(100, 0)]
        );
    }

    #[test]
    fn buttons_have_conventional_names() {
        assert_eq!(button_name(1), Some("left"));
        assert_eq!(button_name(2), Some("right"));
        assert_eq!(button_name(3), Some("middle"));
        assert_eq!(button_name(9), None);
    }
}
