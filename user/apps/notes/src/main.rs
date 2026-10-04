//! Notes: the example windowed Oceans app (ADR-0059).
//!
//! It opens a window through `use windows` and draws into it; what is typed
//! while the window has the focus appears there, and each line ended with
//! Enter is appended to `notes.txt` in its storage (`use storage`). The
//! close button ends it.

#![no_std]
#![no_main]

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use oceans_display_proto::{Event, Window, events, kind};
use oceans_fs_proto::{Kind, Node, flags};
use oceans_rt::{Directory, Start};

oceans_rt::entry!(main);

const EXIT_BAD_START: i64 = 2;
const EXIT_NO_WINDOW: i64 = 3;

const WIDTH: u16 = 480;
const HEIGHT: u16 = 240;
/// The notification bit for window events.
const EVENTS: u64 = 1;

const PAPER: u32 = 0xf4_ef_e1;
const INK: u32 = 0x22_2a_38;
const FAINT: u32 = 0x8a_80_6c;
const CARET: u32 = 0x3b_9e_ff;
const LINE_HEIGHT: usize = 20;
const MARGIN: usize = 12;
/// Characters kept on the line being typed, and lines shown above it.
const MAX_LINE: usize = 52;
const KEPT_LINES: usize = 8;

struct Notes {
    window: Window,
    lines: [([u8; MAX_LINE], usize); KEPT_LINES],
    kept: usize,
    line: [u8; MAX_LINE],
    len: usize,
    focused: bool,
    storage: Option<Node>,
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(windows), Ok(notification)) = (
        directory.find("use", "windows"),
        oceans_rt::notification_create(),
    ) else {
        return EXIT_NO_WINDOW;
    };
    let Ok(window) = Window::open(windows, notification, EVENTS, WIDTH, HEIGHT, "notes.txt") else {
        return EXIT_NO_WINDOW;
    };
    let mut notes = Notes {
        window,
        lines: [([0; MAX_LINE], 0); KEPT_LINES],
        kept: 0,
        line: [0; MAX_LINE],
        len: 0,
        focused: true,
        storage: directory.find("use", "storage").map(Node),
    };
    notes.draw();
    let mut batch = [Event::default(); 20];
    loop {
        if oceans_rt::notification_wait(notification).is_err() {
            return EXIT_BAD_START;
        }
        loop {
            let count = match events(windows, &mut batch) {
                Ok(0) => break,
                Ok(count) => count,
                Err(_) => return EXIT_NO_WINDOW,
            };
            for event in &batch[..count] {
                match event.kind {
                    kind::KEY => notes.key(event.key),
                    kind::FOCUS => notes.focused = event.pressed,
                    kind::CLOSE => {
                        let _ = notes.window.close();
                        return 0;
                    }
                    _ => continue,
                }
            }
            notes.draw();
        }
    }
}

impl Notes {
    fn key(&mut self, key: u8) {
        match key {
            b'\r' | b'\n' => self.enter(),
            // Backspace (DEL) and Ctrl+H.
            0x7f | 0x08 => self.len = self.len.saturating_sub(1),
            b' '..=b'~' if self.len < MAX_LINE => {
                self.line[self.len] = key;
                self.len += 1;
            }
            _ => {}
        }
    }

    /// The line is kept: shown above, and appended to `notes.txt`.
    fn enter(&mut self) {
        if self.kept == KEPT_LINES {
            self.lines.rotate_left(1);
            self.kept -= 1;
        }
        self.lines[self.kept] = (self.line, self.len);
        self.kept += 1;
        if let Some(storage) = &self.storage {
            let _ = append(storage, &self.line[..self.len]);
        }
        self.len = 0;
    }

    fn draw(&mut self) {
        let width = self.window.width;
        let pixels = self.window.pixels();
        pixels.fill(PAPER);
        let mut y = MARGIN;
        for &(line, len) in &self.lines[..self.kept] {
            text(pixels, width, MARGIN, y, &line[..len], FAINT);
            y += LINE_HEIGHT;
        }
        let end = text(pixels, width, MARGIN, y, &self.line[..self.len], INK);
        if self.focused {
            for row in y..y + 16 {
                for x in end..end + 2 {
                    if let Some(pixel) = pixels.get_mut(row * width + x) {
                        *pixel = CARET;
                    }
                }
            }
        }
        let _ = self.window.present();
    }
}

/// Draws ASCII `bytes` at `x`, `y`; returns where the next character goes.
fn text(pixels: &mut [u32], width: usize, x: usize, y: usize, bytes: &[u8], color: u32) -> usize {
    let advance = get_raster_width(FontWeight::Regular, RasterHeight::Size16);
    let mut pen = x;
    for &byte in bytes {
        if let Some(glyph) = get_raster(char::from(byte), FontWeight::Regular, RasterHeight::Size16)
        {
            for (gy, row) in glyph.raster().iter().enumerate() {
                for (gx, &alpha) in row.iter().enumerate() {
                    let (px, py) = (pen + gx, y + gy);
                    if alpha < 128 || px >= width {
                        continue;
                    }
                    if let Some(pixel) = pixels.get_mut(py * width + px) {
                        *pixel = color;
                    }
                }
            }
        }
        pen += advance;
    }
    pen
}

/// Appends `line` and a newline to `notes.txt`.
fn append(storage: &Node, line: &[u8]) -> Result<(), oceans_fs_proto::FsError> {
    let (file, kind) = storage.open("notes.txt", flags::CREATE_FILE | flags::WRITE)?;
    let result = (|| {
        if kind != Kind::File {
            return Ok(());
        }
        let end = file.stat()?.size;
        file.write_all(end, line)?;
        file.write_all(end + line.len() as u64, b"\n")?;
        file.sync()
    })();
    file.close();
    result
}
