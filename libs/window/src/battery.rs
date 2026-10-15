//! The battery as the desktop shows it (ADR-0152): one line in
//! `/system/battery/status`, written by what reads the battery and read
//! by the desktop, as Wi-Fi's files are (ADR-0130). Host-tested.
//!
//! The line is `PERCENT STATE [MINUTES]`: the charge in percent, then
//! `charging`, `discharging` or `full`, then, if known, the minutes left
//! (to empty while discharging, to full while charging). `87 discharging
//! 192` is 87 %, 3 h 12 min left.

use alloc::format;
use alloc::string::String;

/// The status file's name in `/system/battery`.
pub const STATUS: &str = "status";

/// Whether the battery fills, empties or is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Charging,
    Discharging,
    Full,
}

/// The battery: its charge, what it is doing, and the minutes left if
/// known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub state: State,
    pub minutes: Option<u16>,
}

/// The charges a warning is given at, while discharging, highest first.
pub const LOW: [u8; 3] = [20, 10, 5];

impl Battery {
    /// The status line, if it is one.
    pub fn parse(text: &[u8]) -> Option<Battery> {
        let text = core::str::from_utf8(text).ok()?;
        let mut words = text.split_ascii_whitespace();
        let percent = words.next()?.parse::<u8>().ok().filter(|&p| p <= 100)?;
        let state = match words.next()? {
            "charging" => State::Charging,
            "discharging" => State::Discharging,
            "full" => State::Full,
            _ => return None,
        };
        let minutes = match words.next() {
            Some(word) => Some(word.parse::<u16>().ok()?),
            None => None,
        };
        if words.next().is_some() {
            return None;
        }
        Some(Battery {
            percent,
            state,
            minutes,
        })
    }

    /// The status line, as `parse` reads it.
    pub fn to_text(&self) -> String {
        let state = match self.state {
            State::Charging => "charging",
            State::Discharging => "discharging",
            State::Full => "full",
        };
        match self.minutes {
            Some(minutes) => format!("{} {state} {minutes}\n", self.percent),
            None => format!("{} {state}\n", self.percent),
        }
    }

    /// What it is doing, in words: "Charging", "2:45 left", "Fully
    /// charged".
    pub fn describe(&self) -> String {
        match (self.state, self.minutes) {
            (State::Full, _) => String::from("Fully charged"),
            (State::Charging, Some(m)) => format!("{}:{:02} until full", m / 60, m % 60),
            (State::Charging, None) => String::from("Charging"),
            (State::Discharging, Some(m)) => format!("{}:{:02} left", m / 60, m % 60),
            (State::Discharging, None) => String::from("On battery"),
        }
    }

    /// Whether it is low enough to show in red.
    pub fn low(&self) -> bool {
        self.state == State::Discharging && self.percent <= LOW[0]
    }

    /// The warning level crossed going from `before` to `self`, if any:
    /// discharging, down to or past one of [`LOW`] it was above.
    pub fn warning(&self, before: Option<&Battery>) -> Option<u8> {
        if self.state != State::Discharging {
            return None;
        }
        let was = before
            .filter(|b| b.state == State::Discharging)
            .map_or(u8::MAX, |b| b.percent);
        LOW.iter()
            .rev()
            .copied()
            .find(|&level| self.percent <= level && was > level)
    }
}

/// The events file's name in `/system/battery` (ADR-0160).
pub const EVENTS: &str = "events";

/// What the user did to the machine (ADR-0160), as the `acpi` service
/// writes it: the lid's state if there is one, and how many times the
/// power button has been pressed since it started (the desktop acts on
/// each change of the count).
///
/// The file is lines of `lid open` or `lid closed`, and `button N`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Events {
    pub lid_open: Option<bool>,
    pub button: u32,
}

impl Events {
    pub fn parse(text: &[u8]) -> Option<Events> {
        let text = core::str::from_utf8(text).ok()?;
        let mut events = Events::default();
        for line in text.lines() {
            let mut words = line.split_ascii_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some("lid"), Some("open"), None) => events.lid_open = Some(true),
                (Some("lid"), Some("closed"), None) => events.lid_open = Some(false),
                (Some("button"), Some(n), None) => events.button = n.parse().ok()?,
                (None, ..) => {}
                _ => return None,
            }
        }
        Some(events)
    }

    pub fn to_text(&self) -> String {
        let mut text = String::new();
        match self.lid_open {
            Some(true) => text.push_str("lid open\n"),
            Some(false) => text.push_str("lid closed\n"),
            None => {}
        }
        text.push_str(&format!("button {}\n", self.button));
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        let b = Battery::parse(b"87 discharging 192\n").unwrap();
        assert_eq!(
            b,
            Battery {
                percent: 87,
                state: State::Discharging,
                minutes: Some(192)
            }
        );
        assert_eq!(b.describe(), "3:12 left");
        assert_eq!(Battery::parse(b.to_text().as_bytes()), Some(b));
        let full = Battery::parse(b"100 full").unwrap();
        assert_eq!(full.describe(), "Fully charged");
        assert_eq!(Battery::parse(full.to_text().as_bytes()), Some(full));
        assert_eq!(
            Battery::parse(b"40 charging 65").unwrap().describe(),
            "1:05 until full"
        );
        for bad in [
            &b""[..],
            b"101 full",
            b"50 resting",
            b"50 charging x",
            b"50 charging 10 11",
            b"-1 full",
        ] {
            assert_eq!(Battery::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn warnings_once_each() {
        let at = |percent, state| Battery {
            percent,
            state,
            minutes: None,
        };
        let d = State::Discharging;
        assert_eq!(at(20, d).warning(Some(&at(21, d))), Some(20));
        assert_eq!(at(19, d).warning(Some(&at(20, d))), None);
        assert_eq!(at(10, d).warning(Some(&at(19, d))), Some(10));
        // Skipping past two: the lower is said.
        assert_eq!(at(4, d).warning(Some(&at(12, d))), Some(5));
        // Unplugged when low, or the first reading: said at once.
        assert_eq!(at(9, d).warning(Some(&at(9, State::Charging))), Some(10));
        assert_eq!(at(15, d).warning(None), Some(20));
        assert_eq!(at(15, State::Charging).warning(None), None);
        assert!(at(20, d).low());
        assert!(!at(20, State::Charging).low());
    }

    #[test]
    fn events_lines() {
        let e = Events {
            lid_open: Some(false),
            button: 3,
        };
        assert_eq!(Events::parse(e.to_text().as_bytes()), Some(e));
        assert_eq!(
            Events::parse(b"button 1\n"),
            Some(Events {
                lid_open: None,
                button: 1
            })
        );
        assert_eq!(Events::parse(b"lid ajar\n"), None);
        assert_eq!(Events::parse(b"button x\n"), None);
        assert_eq!(Events::parse(b""), Some(Events::default()));
    }
}
