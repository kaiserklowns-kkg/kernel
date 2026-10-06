//! Drawing: a back buffer in RAM, and presenting only the pixels that
//! changed to the (uncached, slow) framebuffer. The shapes and interface
//! text are `oceans-draw`'s (ADR-0080), the same apps draw with; the
//! Terminal's monospaced bitmap font, the pointer and presenting are here.

use alloc::vec;
use alloc::vec::Vec;

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
pub use oceans_draw::{Rect, Rgb};
use oceans_draw::{Style, Surface, Typesetter};

/// Text styles: the interface's (proportional, with Thai: ADR-0077), and
/// the Terminal's monospaced grid.
#[derive(Clone, Copy)]
pub enum Font {
    Body,
    Strong,
    Title,
    /// The Terminal's: a fixed cell per character, Basic Latin.
    Mono,
}

impl Font {
    /// The bitmap font: the Terminal's, and the interface's fallback.
    fn spec(self) -> (FontWeight, RasterHeight, i32) {
        match self {
            Self::Body | Self::Mono => (FontWeight::Regular, RasterHeight::Size16, 16),
            Self::Strong => (FontWeight::Bold, RasterHeight::Size16, 16),
            Self::Title => (FontWeight::Bold, RasterHeight::Size20, 20),
        }
    }

    fn style(self) -> Option<Style> {
        match self {
            Self::Body => Some(Style::Body),
            Self::Strong => Some(Style::Strong),
            Self::Title => Some(Style::Title),
            Self::Mono => None,
        }
    }

    /// Width of one character cell of the bitmap font.
    pub fn advance(self) -> i32 {
        let (weight, height, _) = self.spec();
        get_raster_width(weight, height) as i32
    }

    /// A line's height.
    pub fn height(self) -> i32 {
        self.spec().2
    }
}

/// The back buffer, and what the screen shows now.
pub struct Canvas {
    pub width: i32,
    pub height: i32,
    pixels: Vec<u32>,
    front: Vec<u32>,
    screen: *mut u8,
    pitch: usize,
    shifts: (u32, u32, u32),
    /// The interface's fonts; without them, text falls back to the bitmap
    /// font.
    typesetter: Option<Typesetter>,
    /// The wallpaper, drawn once (empty until set).
    wallpaper: Vec<u32>,
}

impl Canvas {
    /// `screen` maps `pitch * height` bytes of a 32-bit framebuffer.
    pub fn new(screen: *mut u8, width: u32, height: u32, pitch: u32, shifts: (u8, u8, u8)) -> Self {
        let size = width as usize * height as usize;
        Self {
            width: width as i32,
            height: height as i32,
            pixels: vec![0; size],
            // Nothing is known to be on screen: everything is drawn once.
            front: vec![u32::MAX; size],
            screen,
            pitch: pitch as usize,
            shifts: (
                u32::from(shifts.0),
                u32::from(shifts.1),
                u32::from(shifts.2),
            ),
            typesetter: Typesetter::new(),
            wallpaper: Vec::new(),
        }
    }

    /// The back buffer, to draw on.
    fn surface(&mut self) -> Surface<'_> {
        let (w, h) = (self.width as usize, self.height as usize);
        Surface::new(&mut self.pixels, w, h).expect("the back buffer is the screen's size")
    }

    /// The width of `text` in `font`.
    pub fn measure(&mut self, text: &str, font: Font) -> i32 {
        match (font.style(), self.typesetter.as_mut()) {
            (Some(style), Some(typesetter)) => typesetter.measure(text, style),
            _ => font.advance() * text.chars().count() as i32,
        }
    }

    pub fn fill(&mut self, r: Rect, colour: Rgb) {
        self.surface().fill(r, colour);
    }

    pub fn round_fill(&mut self, r: Rect, radius: i32, colour: Rgb) {
        self.surface().round_fill(r, radius, colour);
    }

    pub fn tint(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
        self.surface().tint(r, radius, colour, alpha);
    }

    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, colour: Rgb) {
        self.surface().circle(cx, cy, r, colour);
    }

    pub fn shadow(&mut self, r: Rect, spread: i32, strength: u32) {
        self.surface().shadow(r, spread, strength);
    }

    /// Darkens everything (behind a modal dialog).
    pub fn dim(&mut self, shade: Rgb, alpha: u32) {
        self.surface().dim(shade, alpha);
    }

    /// Copies an app's pixels (`width` per row, `0x00RRGGBB`) into `to`,
    /// clipped to the screen. `pixels` is shared memory the app may be
    /// writing: a frame may tear, nothing worse.
    pub fn blit(&mut self, to: Rect, pixels: *const u32, width: usize) {
        let x0 = to.x.max(0);
        let y0 = to.y.max(0);
        let x1 = (to.x + to.w).min(self.width);
        let y1 = (to.y + to.h).min(self.height);
        for y in y0..y1 {
            let source = (y - to.y) as usize * width + (x0 - to.x) as usize;
            let row = (y * self.width) as usize;
            for (i, pixel) in self.pixels[row + x0 as usize..row + x1.max(x0) as usize]
                .iter_mut()
                .enumerate()
            {
                // SAFETY: `to` is no larger than the window's `width ×
                // height` pixels, all mapped; volatile, as another process
                // writes them.
                *pixel = unsafe { pixels.add(source + i).read_volatile() } & 0x00ff_ffff;
            }
        }
    }

    /// Keeps `pixels` (the whole screen) as the wallpaper: drawn once,
    /// copied on every frame.
    pub fn set_wallpaper(&mut self, pixels: Vec<u32>) {
        if pixels.len() == self.pixels.len() {
            self.wallpaper = pixels;
        }
    }

    /// The wallpaper, over everything (the frame's first step).
    pub fn draw_wallpaper(&mut self) {
        if self.wallpaper.len() == self.pixels.len() {
            self.pixels.copy_from_slice(&self.wallpaper);
        }
    }

    /// Text from (`x`, `y`) (top left), clipped to `clip`; returns the x
    /// after it.
    pub fn text(&mut self, x: i32, y: i32, text: &str, font: Font, colour: Rgb, clip: Rect) -> i32 {
        match (font.style(), self.typesetter.take()) {
            (Some(style), Some(mut typesetter)) => {
                let end = self
                    .surface()
                    .text(&mut typesetter, (x, y), text, style, colour, clip);
                self.typesetter = Some(typesetter);
                end
            }
            (_, typesetter) => {
                self.typesetter = typesetter;
                self.bitmap_text(x, y, text, font, colour, clip)
            }
        }
    }

    /// The bitmap font: a fixed cell per character; characters outside
    /// Basic Latin show as `?`.
    fn bitmap_text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        font: Font,
        colour: Rgb,
        clip: Rect,
    ) -> i32 {
        let (weight, height, _) = font.spec();
        let advance = font.advance();
        let mut pen = x;
        for c in text.chars() {
            if pen >= clip.x + clip.w {
                break;
            }
            let glyph = get_raster(c, weight, height).or_else(|| get_raster('?', weight, height));
            if let Some(glyph) = glyph.filter(|_| c != ' ') {
                for (gy, row) in glyph.raster().iter().enumerate() {
                    for (gx, &alpha) in row.iter().enumerate() {
                        let (px, py) = (pen + gx as i32, y + gy as i32);
                        if alpha == 0
                            || !clip.contains(px, py)
                            || px >= self.width
                            || py >= self.height
                        {
                            continue;
                        }
                        let at = (py * self.width + px) as usize;
                        self.pixels[at] = colour.over(Rgb(self.pixels[at]), u32::from(alpha)).0;
                    }
                }
            }
            pen += advance;
        }
        pen
    }

    /// The pointer: an arrow with its tip at (`x`, `y`).
    pub fn cursor(&mut self, x: i32, y: i32, fill: Rgb, edge: Rgb) {
        const ARROW: [&[u8]; 16] = [
            b"X",
            b"XX",
            b"X.X",
            b"X..X",
            b"X...X",
            b"X....X",
            b"X.....X",
            b"X......X",
            b"X.......X",
            b"X........X",
            b"X.....XXXX",
            b"X..X..X",
            b"X.X X..X",
            b"XX  X..X",
            b"X    X..X",
            b"      XX",
        ];
        for (dy, row) in ARROW.iter().enumerate() {
            for (dx, &cell) in row.iter().enumerate() {
                let colour = match cell {
                    b'X' => edge,
                    b'.' => fill,
                    _ => continue,
                };
                self.fill(Rect::new(x + dx as i32, y + dy as i32, 1, 1), colour);
            }
        }
    }

    /// Writes the pixels that differ from what the screen shows; returns
    /// how many.
    pub fn present(&mut self) -> usize {
        let mut written = 0;
        let (r, g, b) = self.shifts;
        for y in 0..self.height as usize {
            let row = y * self.width as usize;
            for x in 0..self.width as usize {
                let value = self.pixels[row + x];
                if self.front[row + x] == value {
                    continue;
                }
                self.front[row + x] = value;
                let (cr, cg, cb) = ((value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff);
                let native = (cr << r) | (cg << g) | (cb << b);
                // SAFETY: (x, y) lies inside the framebuffer (`width` x
                // `height` pixels of 4 bytes, `pitch` bytes a line), mapped
                // writable for the life of the service.
                unsafe {
                    self.screen
                        .add(y * self.pitch + x * 4)
                        .cast::<u32>()
                        .write_volatile(native);
                }
                written += 1;
            }
        }
        written
    }
}
