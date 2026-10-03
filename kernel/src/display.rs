//! The framebuffer console (ADR-0029): kernel log and console output drawn
//! as text on the boot framebuffer, so Oceans is usable on a screen and not
//! only over serial.
//!
//! - Glyphs come from Noto Sans Mono (16 px, regular, anti-aliased) via the
//!   `noto-sans-mono-bitmap` crate.
//! - Only 32-bit RGB framebuffers are used; anything else leaves the
//!   display off (serial still works). Diagnostics never depend on it
//!   (master spec §42).
//! - Understands what the shell sends: CR, LF, backspace, tab, and the
//!   escape sequences to clear the screen or the line and to place the
//!   cursor. Other sequences are consumed silently.
//! - Scrolling moves a quarter of the screen at a time, so long output
//!   costs a fraction of line-by-line scrolling on an uncached framebuffer.

use alloc::vec;
use alloc::vec::Vec;

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use spin::Mutex;

use crate::boot::BootInfo;
use crate::klog;
use crate::memory::paging;

const WEIGHT: FontWeight = FontWeight::Regular;
const HEIGHT: RasterHeight = RasterHeight::Size16;
const CELL_HEIGHT: usize = 16;
/// Ocean blue background, soft white text.
const BACKGROUND: (u32, u32, u32) = (0x0b, 0x1e, 0x33);
const FOREGROUND: (u32, u32, u32) = (0xd8, 0xe4, 0xee);
const MAX_PARAMS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// Seen ESC.
    Start,
    /// Inside `ESC [`.
    Csi,
}

struct Display {
    base: usize,
    pitch: usize,
    shifts: (u32, u32, u32),
    cell_width: usize,
    cols: usize,
    rows: usize,
    /// The characters on screen, for redrawing under the cursor.
    cells: Vec<u8>,
    col: usize,
    row: usize,
    escape: Escape,
    params: [u16; MAX_PARAMS],
    param_count: usize,
}

static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

impl Display {
    fn pixel(&self, (r, g, b): (u32, u32, u32)) -> u32 {
        (r << self.shifts.0) | (g << self.shifts.1) | (b << self.shifts.2)
    }

    fn put(&self, x: usize, y: usize, value: u32) {
        let at = self.base + y * self.pitch + x * 4;
        // SAFETY: `x < cols * cell_width <= width` and `y < rows * 16 <=
        // height`, so the 4 bytes lie inside the mapped framebuffer.
        unsafe { (at as *mut u32).write_volatile(value) }
    }

    /// Draws `byte` (or a blank) at cell (`col`, `row`), inverted for the
    /// cursor.
    fn draw(&self, col: usize, row: usize, byte: u8, inverted: bool) {
        let (fg, bg) = if inverted {
            (BACKGROUND, FOREGROUND)
        } else {
            (FOREGROUND, BACKGROUND)
        };
        let glyph = get_raster(char::from(byte), WEIGHT, HEIGHT)
            .or_else(|| get_raster('?', WEIGHT, HEIGHT));
        let blend = |a: u32, b: u32, i: u32| (a * (255 - i) + b * i) / 255;
        let (x0, y0) = (col * self.cell_width, row * CELL_HEIGHT);
        for y in 0..CELL_HEIGHT {
            for x in 0..self.cell_width {
                let intensity = glyph
                    .as_ref()
                    .filter(|_| byte != b' ')
                    .and_then(|g| g.raster().get(y)?.get(x).copied())
                    .map_or(0, u32::from);
                let color = (
                    blend(bg.0, fg.0, intensity),
                    blend(bg.1, fg.1, intensity),
                    blend(bg.2, fg.2, intensity),
                );
                self.put(x0 + x, y0 + y, self.pixel(color));
            }
        }
    }

    fn set_cell(&mut self, byte: u8) {
        self.cells[self.row * self.cols + self.col] = byte;
        self.draw(self.col, self.row, byte, false);
    }

    fn cursor(&self, on: bool) {
        let byte = self.cells[self.row * self.cols + self.col];
        self.draw(self.col, self.row, byte, on);
    }

    fn clear_rows(&mut self, rows: core::ops::Range<usize>) {
        let blank = self.pixel(BACKGROUND);
        for row in rows {
            self.cells[row * self.cols..(row + 1) * self.cols].fill(b' ');
            for y in row * CELL_HEIGHT..(row + 1) * CELL_HEIGHT {
                for x in 0..self.cols * self.cell_width {
                    self.put(x, y, blank);
                }
            }
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        // Jump-scroll by a quarter of the screen.
        let by = (self.rows / 4).max(1);
        let keep = self.rows - by;
        let row_bytes = CELL_HEIGHT * self.pitch;
        // SAFETY: both ranges lie inside the mapped framebuffer (rows *
        // CELL_HEIGHT lines of `pitch` bytes); `copy` handles the overlap.
        unsafe {
            core::ptr::copy(
                (self.base + by * row_bytes) as *const u8,
                self.base as *mut u8,
                keep * row_bytes,
            );
        }
        self.cells.copy_within(by * self.cols.., 0);
        self.clear_rows(keep..self.rows);
        self.row = keep;
    }

    fn csi(&mut self, final_byte: u8) {
        // Missing or zero parameters mean 1 (positions are 1-based).
        let param = |i: usize| match self.params.get(i) {
            Some(&value) if i < self.param_count && value > 0 => value,
            _ => 1,
        };
        match final_byte {
            b'J' => {
                if self.params[0] == 2 || self.param_count == 0 {
                    self.clear_rows(0..self.rows);
                    self.row = 0;
                    self.col = 0;
                }
            }
            b'H' | b'f' => {
                self.row = usize::from(param(0) - 1).min(self.rows - 1);
                self.col = usize::from(param(1) - 1).min(self.cols - 1);
            }
            b'K' => {
                for col in self.col..self.cols {
                    self.cells[self.row * self.cols + col] = b' ';
                    self.draw(col, self.row, b' ', false);
                }
            }
            _ => {} // colours and other controls are ignored
        }
    }

    fn write_byte(&mut self, byte: u8) {
        match (self.escape, byte) {
            (Escape::None, 0x1b) => self.escape = Escape::Start,
            (Escape::Start, b'[') => {
                self.escape = Escape::Csi;
                self.params = [0; MAX_PARAMS];
                self.param_count = 0;
            }
            (Escape::Start, _) => self.escape = Escape::None,
            (Escape::Csi, b'0'..=b'9') => {
                if self.param_count == 0 {
                    self.param_count = 1;
                }
                if let Some(param) = self.params.get_mut(self.param_count - 1) {
                    *param = param
                        .saturating_mul(10)
                        .saturating_add(u16::from(byte - b'0'));
                }
            }
            (Escape::Csi, b';') => self.param_count = (self.param_count.max(1) + 1).min(MAX_PARAMS),
            (Escape::Csi, 0x40..=0x7e) => {
                self.csi(byte);
                self.escape = Escape::None;
            }
            (Escape::Csi, _) => {} // intermediate bytes (`?`, `=`, ...)
            (Escape::None, b'\r') => self.col = 0,
            (Escape::None, b'\n') => self.newline(),
            (Escape::None, 0x08) => self.col = self.col.saturating_sub(1),
            (Escape::None, b'\t') => {
                let next = (self.col / 8 + 1) * 8;
                while self.col < next.min(self.cols - 1) {
                    self.set_cell(b' ');
                    self.col += 1;
                }
            }
            (Escape::None, 0x20..=0x7e) => {
                self.set_cell(byte);
                self.col += 1;
                if self.col == self.cols {
                    self.newline();
                }
            }
            (Escape::None, _) => {} // other control bytes
        }
    }
}

/// Finds the boot framebuffer and starts drawing on it.
pub fn init(boot: &BootInfo) {
    let Some(fb) = boot.framebuffer() else {
        klog::info!("no framebuffer; console on serial only");
        return;
    };
    if fb.bpp != 32 {
        klog::warn!("{}-bit framebuffer not supported; serial only", fb.bpp);
        return;
    }
    let size = fb.pitch * fb.height;
    let base = match paging::map_mmio(fb.physical, size) {
        Ok(base) => base as usize,
        Err(err) => {
            klog::warn!("cannot map the framebuffer: {err:?}");
            return;
        }
    };
    let cell_width = get_raster_width(WEIGHT, HEIGHT);
    let cols = (fb.width as usize) / cell_width;
    let rows = (fb.height as usize) / CELL_HEIGHT;
    if cols < 20 || rows < 5 {
        klog::warn!("{}x{} is too small for a console", fb.width, fb.height);
        return;
    }
    let mut display = Display {
        base,
        pitch: fb.pitch as usize,
        shifts: (
            u32::from(fb.red_shift),
            u32::from(fb.green_shift),
            u32::from(fb.blue_shift),
        ),
        cell_width,
        cols,
        rows,
        cells: vec![b' '; cols * rows],
        col: 0,
        row: 0,
        escape: Escape::None,
        params: [0; MAX_PARAMS],
        param_count: 0,
    };
    display.clear_rows(0..rows);
    *DISPLAY.lock() = Some(display);
    klog::info!(
        "{}x{} framebuffer, {cols}x{rows} text console",
        fb.width,
        fb.height
    );
}

/// Draws `bytes`. Called with the console lock held, so output from
/// different writers never interleaves within a call.
pub fn write(bytes: &[u8]) {
    let mut guard = DISPLAY.lock();
    if let Some(display) = guard.as_mut() {
        display.cursor(false);
        for &byte in bytes {
            display.write_byte(byte);
        }
        display.cursor(true);
    }
}

/// Like [`write`], but gives up instead of waiting (panic paths).
pub fn try_write(bytes: &[u8]) {
    if let Some(mut guard) = DISPLAY.try_lock()
        && let Some(display) = guard.as_mut()
    {
        for &byte in bytes {
            display.write_byte(byte);
        }
    }
}
