//! Drawing: a back buffer in RAM, shapes and text, and presenting only the
//! pixels that changed to the (uncached, slow) framebuffer.

use alloc::vec;
use alloc::vec::Vec;

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};

use crate::text::{Style, Typesetter};

/// A colour, 0xRRGGBB.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u32);

impl Rgb {
    fn channels(self) -> (u32, u32, u32) {
        ((self.0 >> 16) & 0xff, (self.0 >> 8) & 0xff, self.0 & 0xff)
    }

    /// `self` over `under` with `alpha` (0..=255).
    pub fn over(self, under: Rgb, alpha: u32) -> Rgb {
        let (r1, g1, b1) = self.channels();
        let (r0, g0, b0) = under.channels();
        let mix = |a: u32, b: u32| (b * (255 - alpha) + a * alpha) / 255;
        Rgb((mix(r1, r0) << 16) | (mix(g1, g0) << 8) | mix(b1, b0))
    }
}

pub use oceans_window::Rect;

/// The integer square root of a small non-negative number.
fn isqrt(n: i32) -> i32 {
    let mut root = 0;
    while (root + 1) * (root + 1) <= n {
        root += 1;
    }
    root
}

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

    /// The width of `text` in `font`.
    pub fn measure(&mut self, text: &str, font: Font) -> i32 {
        match (font.style(), self.typesetter.as_mut()) {
            (Some(style), Some(typesetter)) => typesetter.measure(text, style),
            _ => font.advance() * text.chars().count() as i32,
        }
    }

    fn clip(&self, r: Rect) -> Option<(i32, i32, i32, i32)> {
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = (r.x + r.w).min(self.width);
        let y1 = (r.y + r.h).min(self.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    pub fn fill(&mut self, r: Rect, color: Rgb) {
        let Some((x0, y0, x1, y1)) = self.clip(r) else {
            return;
        };
        for y in y0..y1 {
            let row = (y * self.width) as usize;
            self.pixels[row + x0 as usize..row + x1 as usize].fill(color.0);
        }
    }

    /// Copies an app's pixels (`width` per row, `0x00RRGGBB`) into `to`,
    /// clipped to the screen. `pixels` is shared memory the app may be
    /// writing: a frame may tear, nothing worse.
    pub fn blit(&mut self, to: Rect, pixels: *const u32, width: usize) {
        let Some((x0, y0, x1, y1)) = self.clip(to) else {
            return;
        };
        for y in y0..y1 {
            let source = (y - to.y) as usize * width + (x0 - to.x) as usize;
            let row = (y * self.width) as usize;
            for (i, pixel) in self.pixels[row + x0 as usize..row + x1 as usize]
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

    /// A rectangle with rounded corners of `radius` pixels (ADR-0076): the
    /// corners' pixels outside the circle are left as they are, and the
    /// edge pixel of each corner row is blended for a smoother curve.
    pub fn round_fill(&mut self, r: Rect, radius: i32, color: Rgb) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            // The row's distance above (or below) the corners' centres.
            let above = if dy < radius {
                radius - dy
            } else if dy >= r.h - radius {
                dy - (r.h - radius) + 1
            } else {
                0
            };
            let inset = if above == 0 {
                0
            } else {
                radius - isqrt(radius * radius - (above - 1) * (above - 1))
            };
            let y = r.y + dy;
            self.fill(Rect::new(r.x + inset, y, r.w - 2 * inset, 1), color);
            if inset > 0 {
                // Soften the step at each end.
                for x in [r.x + inset - 1, r.x + r.w - inset] {
                    if x >= 0 && x < self.width && y >= 0 && y < self.height {
                        let at = (y * self.width + x) as usize;
                        self.pixels[at] = color.over(Rgb(self.pixels[at]), 110).0;
                    }
                }
            }
        }
    }

    /// `color` laid over `r` with `alpha` (0..=255): a translucent surface
    /// (ADR-0078), with rounded corners of `radius`.
    pub fn tint(&mut self, r: Rect, radius: i32, color: Rgb, alpha: u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            let above = if dy < radius {
                radius - dy
            } else if dy >= r.h - radius {
                dy - (r.h - radius) + 1
            } else {
                0
            };
            let inset = if above == 0 {
                0
            } else {
                radius - isqrt(radius * radius - (above - 1) * (above - 1))
            };
            let row = Rect::new(r.x + inset, r.y + dy, r.w - 2 * inset, 1);
            let Some((x0, y0, x1, _)) = self.clip(row) else {
                continue;
            };
            let at = (y0 * self.width) as usize;
            for pixel in &mut self.pixels[at + x0 as usize..at + x1 as usize] {
                *pixel = color.over(Rgb(*pixel), alpha).0;
            }
        }
    }

    /// A filled circle of radius `r` around (`cx`, `cy`), its edge
    /// anti-aliased.
    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, color: Rgb) {
        // In quarter pixels, to soften the edge.
        let r4 = r * 4;
        for y in cy - r - 1..=cy + r + 1 {
            for x in cx - r - 1..=cx + r + 1 {
                if x < 0 || y < 0 || x >= self.width || y >= self.height {
                    continue;
                }
                let mut inside = 0;
                for sy in 0..2 {
                    for sx in 0..2 {
                        let dx = (x - cx) * 4 + sx * 2 - 1;
                        let dy = (y - cy) * 4 + sy * 2 - 1;
                        if dx * dx + dy * dy <= r4 * r4 {
                            inside += 1;
                        }
                    }
                }
                if inside > 0 {
                    let at = (y * self.width + x) as usize;
                    self.pixels[at] = color.over(Rgb(self.pixels[at]), inside * 255 / 4).0;
                }
            }
        }
    }

    /// A soft shadow under `r`: darker near it, fading over `spread`
    /// pixels, a little lower than the surface (ADR-0078).
    pub fn shadow(&mut self, r: Rect, spread: i32, strength: u32) {
        for step in (1..=spread).rev() {
            let alpha = strength / spread as u32;
            self.tint(
                Rect::new(
                    r.x - step,
                    r.y - step + spread / 2,
                    r.w + 2 * step,
                    r.h + 2 * step,
                ),
                step + 6,
                Rgb(0),
                alpha,
            );
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

    /// Darkens everything (behind a modal dialog).
    pub fn dim(&mut self, shade: Rgb, alpha: u32) {
        for pixel in &mut self.pixels {
            *pixel = shade.over(Rgb(*pixel), alpha).0;
        }
    }

    /// Text from (`x`, `y`) (top left), clipped to `clip`; returns the x
    /// after it.
    pub fn text(&mut self, x: i32, y: i32, text: &str, font: Font, color: Rgb, clip: Rect) -> i32 {
        match font.style() {
            Some(style) if self.typesetter.is_some() => {
                self.typeset(x, y, text, style, color, clip)
            }
            _ => self.bitmap_text(x, y, text, font, color, clip),
        }
    }

    /// Interface text (ADR-0077): each character's coverage, blended.
    fn typeset(&mut self, x: i32, y: i32, text: &str, style: Style, color: Rgb, clip: Rect) -> i32 {
        let Some(mut typesetter) = self.typesetter.take() else {
            return x;
        };
        let mut pen = x;
        for c in text.chars() {
            if pen >= clip.x + clip.w {
                break;
            }
            let glyph = typesetter.glyph(style, c);
            for gy in 0..glyph.height {
                for gx in 0..glyph.width {
                    let alpha = glyph.coverage[gy * glyph.width + gx];
                    let (px, py) = (pen + glyph.left + gx as i32, y + glyph.top + gy as i32);
                    if alpha == 0
                        || !clip.contains(px, py)
                        || px < 0
                        || py < 0
                        || px >= self.width
                        || py >= self.height
                    {
                        continue;
                    }
                    let at = (py * self.width + px) as usize;
                    self.pixels[at] = color.over(Rgb(self.pixels[at]), u32::from(alpha)).0;
                }
            }
            pen += glyph.advance;
        }
        self.typesetter = Some(typesetter);
        pen
    }

    /// The bitmap font: a fixed cell per character; characters outside
    /// Basic Latin show as `?`.
    fn bitmap_text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        font: Font,
        color: Rgb,
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
                        self.pixels[at] = color.over(Rgb(self.pixels[at]), u32::from(alpha)).0;
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
                let color = match cell {
                    b'X' => edge,
                    b'.' => fill,
                    _ => continue,
                };
                self.fill(Rect::new(x + dx as i32, y + dy as i32, 1, 1), color);
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
                let (cr, cg, cb) = Rgb(value).channels();
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
