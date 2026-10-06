//! Calculator: a basic app built on the Oceans app toolkit (ADR-0080).
//!
//! Buttons or the keyboard: digits, `.`, `+ - * /`, `%`, Enter or `=`,
//! Backspace, Escape or `c` to clear.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use core::fmt::Write;

use oceans_rt::{Directory, Start};
use oceans_ui::{Rect, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 280;
const HEIGHT: u16 = 420;
const KEYS: [[&str; 4]; 5] = [
    ["C", "+/-", "%", "/"],
    ["7", "8", "9", "*"],
    ["4", "5", "6", "-"],
    ["1", "2", "3", "+"],
    ["0", ".", "<", "="],
];

#[derive(Default)]
struct Calculator {
    /// What is being typed (or the last result).
    entry: String,
    /// The value before the operator, and the operator.
    pending: Option<(f64, u8)>,
    /// The entry shows a result: typing a digit starts a new number.
    fresh: bool,
    error: bool,
}

impl Calculator {
    fn value(&self) -> f64 {
        self.entry.parse().unwrap_or(0.0)
    }

    fn show(&mut self, value: f64) {
        self.entry.clear();
        if !value.is_finite() {
            self.error = true;
            return;
        }
        // Up to ten decimals, without trailing zeros.
        let _ = write!(self.entry, "{value:.10}");
        if self.entry.contains('.') {
            while self.entry.ends_with('0') {
                self.entry.pop();
            }
            if self.entry.ends_with('.') {
                self.entry.pop();
            }
        }
        if self.entry == "-0" {
            self.entry = String::from("0");
        }
    }

    fn press(&mut self, key: &str) {
        if self.error && key != "C" {
            return;
        }
        match key {
            "C" => *self = Self::default(),
            "<" => {
                if !self.fresh {
                    self.entry.pop();
                }
            }
            "+/-" => {
                let value = -self.value();
                self.show(value);
            }
            "%" => {
                let value = self.value() / 100.0;
                self.show(value);
                self.fresh = true;
            }
            "." => {
                if self.fresh {
                    self.entry.clear();
                    self.fresh = false;
                }
                if !self.entry.contains('.') {
                    if self.entry.is_empty() {
                        self.entry.push('0');
                    }
                    self.entry.push('.');
                }
            }
            "+" | "-" | "*" | "/" => {
                self.equals();
                self.pending = Some((self.value(), key.as_bytes()[0]));
                self.fresh = true;
            }
            "=" => {
                self.equals();
                self.pending = None;
                self.fresh = true;
            }
            digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                if self.fresh || self.entry == "0" {
                    self.entry.clear();
                    self.fresh = false;
                }
                if self.entry.len() < 16 {
                    self.entry.push_str(digit);
                }
            }
            _ => {}
        }
    }

    /// Applies the pending operator to the entry.
    fn equals(&mut self) {
        if let Some((left, operator)) = self.pending.take() {
            if self.fresh {
                // An operator pressed twice: the newer one counts.
                return;
            }
            let right = self.value();
            let result = match operator {
                b'+' => left + right,
                b'-' => left - right,
                b'*' => left * right,
                _ => left / right,
            };
            self.show(result);
        }
    }
}

fn frame(ui: &mut Ui<'_, '_>, calculator: &mut Calculator) {
    ui.background(colour::WINDOW);
    // Typed keys first.
    for key in core::mem::take(&mut ui.input.keys) {
        let name = match key {
            b'0'..=b'9' | b'.' | b'+' | b'-' | b'*' | b'/' | b'%' => {
                Some(core::str::from_utf8(core::slice::from_ref(&key)).unwrap_or(""))
            }
            b'\r' | b'=' => Some("="),
            0x08 | 0x7f => Some("<"),
            0x1b | b'c' | b'C' => Some("C"),
            _ => None,
        };
        if let Some(name) = name {
            calculator.press(name);
            ui.changed = true;
        }
    }
    // The display.
    let (w, _) = (ui.area.w, ui.area.h);
    let screen = Rect::new(12, 12, w - 24, 72);
    ui.surface.round_fill(screen, 12, colour::SURFACE);
    let shown = if calculator.error {
        "Error"
    } else if calculator.entry.is_empty() {
        "0"
    } else {
        calculator.entry.as_str()
    };
    let width = ui.measure(shown, Style::Title);
    ui.text_at(
        screen.x + screen.w - 16 - width,
        screen.y + 36,
        shown,
        Style::Title,
        colour::TEXT,
        screen,
    );
    if let Some((left, operator)) = calculator.pending {
        let mut above = String::new();
        let _ = write!(above, "{left} {}", char::from(operator));
        let width = ui.measure(&above, Style::Body);
        ui.text_at(
            screen.x + screen.w - 16 - width,
            screen.y + 10,
            &above,
            Style::Body,
            colour::MUTED,
            screen,
        );
    }
    // The keys.
    let (gap, top) = (8, screen.y + screen.h + 16);
    let size = (w - 24 - 3 * gap) / 4;
    for (row, keys) in KEYS.iter().enumerate() {
        for (col, &key) in keys.iter().enumerate() {
            let r = Rect::new(
                12 + col as i32 * (size + gap),
                top + row as i32 * (size - 6 + gap),
                size,
                size - 6,
            );
            let primary = matches!(key, "/" | "*" | "-" | "+" | "=");
            if ui.button_in(r, key, primary) {
                calculator.press(key);
            }
        }
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut calculator = Calculator::default();
    oceans_ui::run(&directory, "", WIDTH, HEIGHT, &mut calculator, frame)
}
