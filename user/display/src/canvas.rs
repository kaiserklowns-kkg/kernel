//! Drawing: a back buffer in RAM, and presenting only the pixels that
//! changed to the (uncached, slow) framebuffer. The shapes and interface
//! text are `oceans-draw`'s (ADR-0080), the same apps draw with; the
//! Terminal's monospaced bitmap font, the pointer and presenting are here.
//!
//! **Scale** (ADR-0136): the desktop is laid out in logical pixels,
//! `width × height` of them; the back buffer is the screen's own pixels,
//! and everything is drawn into it at the scale, sharp. Apps' windows come
//! at the scale already and are copied pixel for pixel.

use alloc::vec;
use alloc::vec::Vec;

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
pub use oceans_draw::{Rect, Rgb, Scale};
use oceans_draw::{Style, Surface, Typesetter};

/// Text styles: the interface's (proportional, with Thai: ADR-0077), and
/// the Terminal's monospaced grid.
#[derive(Clone, Copy)]
pub enum Font {
    Body,
    Strong,
    Title,
    /// The time, large (ADR-0119).
    Display,
    /// The Terminal's: a fixed cell per character, Basic Latin.
    Mono,
}

impl Font {
    /// The bitmap font: the Terminal's, and the interface's fallback (for
    /// the large time too: its largest).
    fn spec(self) -> (FontWeight, RasterHeight, i32) {
        match self {
            Self::Body | Self::Mono => (FontWeight::Regular, RasterHeight::Size16, 16),
            Self::Strong => (FontWeight::Bold, RasterHeight::Size16, 16),
            Self::Title => (FontWeight::Bold, RasterHeight::Size20, 20),
            Self::Display => (FontWeight::Regular, RasterHeight::Size20, 56),
        }
    }

    fn style(self) -> Option<Style> {
        match self {
            Self::Body => Some(Style::Body),
            Self::Strong => Some(Style::Strong),
            Self::Title => Some(Style::Title),
            Self::Display => Some(Style::Display),
            Self::Mono => None,
        }
    }

    /// Width of one character cell of the bitmap font (logical pixels).
    pub fn advance(self) -> i32 {
        let (weight, height, _) = self.spec();
        get_raster_width(weight, height) as i32
    }

    /// A line's height (logical pixels).
    pub fn height(self) -> i32 {
        self.spec().2
    }
}

/// The bitmap font's size for `px` of the screen's pixels: the largest
/// that is not larger (the Terminal's cells stay their logical size).
fn raster_for(px: i32) -> RasterHeight {
    match px {
        32.. => RasterHeight::Size32,
        24..=31 => RasterHeight::Size24,
        20..=23 => RasterHeight::Size20,
        _ => RasterHeight::Size16,
    }
}

/// The back buffer, and what the screen shows now.
/// How many places frosted glass is kept for (ADR-0151): Control Center
/// and the gallery beside it, and a little more.
const FROSTED_KEPT: usize = 4;

pub struct Canvas {
    /// The screen in logical pixels (ADR-0136): what the desktop lays out.
    pub width: i32,
    pub height: i32,
    /// The screen in its own pixels.
    pub device_width: i32,
    pub device_height: i32,
    scale: Scale,
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
    /// The logo at the size last drawn: its side and `0xAARRGGBB` pixels.
    logo: Option<(i32, Vec<u32>)>,
    /// Frosted glass blurred lately (ADR-0151): where (device pixels),
    /// a hash of what was under it, and the blurred pixels. The same
    /// background again is not blurred again.
    frosted: Vec<(Rect, u64, Vec<u32>)>,
}

impl Canvas {
    /// `screen` maps `pitch * height` bytes of a 32-bit framebuffer.
    pub fn new(screen: *mut u8, width: u32, height: u32, pitch: u32, shifts: (u8, u8, u8)) -> Self {
        let size = width as usize * height as usize;
        Self {
            width: width as i32,
            height: height as i32,
            device_width: width as i32,
            device_height: height as i32,
            scale: Scale::ONE,
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
            logo: None,
            frosted: Vec::new(),
        }
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// Draws at `scale` from now on: the logical size follows.
    pub fn set_scale(&mut self, scale: Scale) {
        self.scale = scale;
        self.width = scale.logical_fit(self.device_width);
        self.height = scale.logical_fit(self.device_height);
        if let Some(typesetter) = self.typesetter.as_mut() {
            typesetter.set_scale(scale);
        }
    }

    /// The back buffer, to draw on.
    fn surface(&mut self) -> Surface<'_> {
        let (w, h) = (self.device_width as usize, self.device_height as usize);
        Surface::scaled(&mut self.pixels, w, h, self.scale)
            .expect("the back buffer is the screen's size")
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

    /// Draws with the shared drawing (`oceans_draw::icon`'s icons).
    pub fn with_surface(&mut self, draw: impl FnOnce(&mut Surface<'_>)) {
        draw(&mut self.surface());
    }

    /// Frosted glass (ADR-0150), as macOS's and Windows' panels are: what
    /// is under the rounded `r` blurred (`blur` logical pixels), then
    /// `tint` laid over it with `alpha`. The rest of the screen is left.
    /// What is under it is hashed first: the same again (the pointer
    /// moving over the panel, a slider dragged) takes the blur kept from
    /// last time (ADR-0151).
    pub fn frost(&mut self, r: Rect, radius: i32, blur: i32, tint: Rgb, alpha: u32) {
        let d = self.scale.rect(r);
        let x0 = d.x.max(0);
        let y0 = d.y.max(0);
        let x1 = (d.x + d.w).min(self.device_width);
        let y1 = (d.y + d.h).min(self.device_height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let stride = self.device_width as usize;
        let under = Rect::new(x0, y0, w as i32, h as i32);
        // FNV-1a over what is under it, with how far it blurs.
        let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ blur as u64;
        for y in 0..h {
            let row = (y0 as usize + y) * stride + x0 as usize;
            for &p in &self.pixels[row..row + w] {
                hash = (hash ^ u64::from(p)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        let kept = self
            .frosted
            .iter()
            .position(|(at, h, _)| *at == under && *h == hash);
        let blurred = match kept {
            Some(i) => core::mem::take(&mut self.frosted[i].2),
            None => self.blurred(under, blur),
        };
        // Back, inside the rounded shape only.
        let corner = self.scale.len(radius).min(d.w / 2).min(d.h / 2);
        for y in 0..h {
            let dy = y0 + y as i32 - d.y;
            let inset = rounded_inset(dy, d.h, corner);
            let row = (y0 as usize + y) * stride;
            for x in 0..w {
                let dx = x0 + x as i32 - d.x;
                if dx < inset || dx >= d.w - inset {
                    continue;
                }
                self.pixels[row + x0 as usize + x] = blurred[y * w + x];
            }
        }
        // Kept for next time: one a place, a few places.
        self.frosted.retain(|(at, _, _)| *at != under);
        if self.frosted.len() >= FROSTED_KEPT {
            self.frosted.remove(0);
        }
        self.frosted.push((under, hash, blurred));
        self.surface().tint(r, radius, tint, alpha);
    }

    /// The back buffer's pixels in `under` (device pixels, on the screen)
    /// blurred: three box blurs across and down, close to a Gaussian.
    fn blurred(&self, under: Rect, blur: i32) -> Vec<u32> {
        let (w, h) = (under.w as usize, under.h as usize);
        let stride = self.device_width as usize;
        let mut channels = [vec![0u32; w * h], vec![0u32; w * h], vec![0u32; w * h]];
        for y in 0..h {
            let row = (under.y as usize + y) * stride + under.x as usize;
            for x in 0..w {
                let p = self.pixels[row + x];
                channels[0][y * w + x] = (p >> 16) & 0xff;
                channels[1][y * w + x] = (p >> 8) & 0xff;
                channels[2][y * w + x] = p & 0xff;
            }
        }
        let reach = self.scale.len(blur.max(1)) as usize / 2 + 1;
        let mut scratch = vec![0u32; w.max(h)];
        for channel in &mut channels {
            for _ in 0..3 {
                box_blur(channel, w, h, 1, w, reach, &mut scratch);
                box_blur(channel, h, w, w, 1, reach, &mut scratch);
            }
        }
        (0..w * h)
            .map(|at| channels[0][at] << 16 | channels[1][at] << 8 | channels[2][at])
            .collect()
    }

    /// Darkens everything (behind a modal dialog).
    pub fn dim(&mut self, shade: Rgb, alpha: u32) {
        self.surface().dim(shade, alpha);
    }

    /// An app's window content `to` (logical): its pixels (`width ×
    /// height`, `0x00RRGGBB`, the screen's, at the scale) copied one for
    /// one, clipped to the screen; what they do not cover (a resize the
    /// app has not caught up with) in `rest`. `pixels` is shared memory
    /// the app may be writing: a frame may tear, nothing worse.
    pub fn blit_window(
        &mut self,
        to: Rect,
        pixels: *const u32,
        width: usize,
        height: usize,
        rest: Rgb,
    ) {
        let to = self.scale.rect(to);
        let w = to.w.min(width as i32);
        let h = to.h.min(height as i32);
        let x0 = to.x.max(0);
        let y0 = to.y.max(0);
        let x1 = (to.x + w).min(self.device_width);
        let y1 = (to.y + h).min(self.device_height);
        for y in y0..y1 {
            let source = (y - to.y) as usize * width + (x0 - to.x) as usize;
            let row = (y * self.device_width) as usize;
            for (i, pixel) in self.pixels[row + x0 as usize..row + x1.max(x0) as usize]
                .iter_mut()
                .enumerate()
            {
                // SAFETY: (x, y) is inside the window's `width × height`
                // pixels, all mapped; volatile, as another process writes
                // them.
                *pixel = unsafe { pixels.add(source + i).read_volatile() } & 0x00ff_ffff;
            }
        }
        self.fill_device(Rect::new(to.x + w, to.y, to.w - w, to.h), rest);
        self.fill_device(Rect::new(to.x, to.y + h, w, to.h - h), rest);
    }

    /// Fills `r` of the screen's own pixels.
    fn fill_device(&mut self, r: Rect, colour: Rgb) {
        let x0 = r.x.max(0);
        let x1 = (r.x + r.w).min(self.device_width);
        if x0 >= x1 {
            return;
        }
        for y in r.y.max(0)..(r.y + r.h).min(self.device_height) {
            let row = (y * self.device_width) as usize;
            self.pixels[row + x0 as usize..row + x1 as usize].fill(colour.0);
        }
    }

    /// The Oceans logo (ADR-0117), `size × size` logical pixels from (`x`,
    /// `y`), blended over what is there: made at the size the scale needs,
    /// once for each size (ADR-0136), from its 80-pixel copy (RGBA, made
    /// from `oceans-logo.png`): as large as it is drawn at 200 %, and small
    /// enough to keep in the program rather than decode the photograph's
    /// 512 pixels into memory the desktop would keep.
    pub fn logo(&mut self, x: i32, y: i32, size: i32) {
        const SIDE: u32 = 80;
        const PICTURE: &[u8] = include_bytes!("../assets/oceans-logo-80.rgba");
        let to = self.scale.rect(Rect::new(x, y, size, size));
        if to.w <= 0 {
            return;
        }
        if self.logo.as_ref().is_none_or(|(side, _)| *side != to.w) {
            let image = oceans_image::Image {
                width: SIDE,
                height: SIDE,
                pixels: PICTURE
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
                    .collect(),
            };
            let side = to.w as u32;
            self.logo = Some((to.w, oceans_image::scale(&image, side, side)));
        }
        let Some((side, pixels)) = &self.logo else {
            return;
        };
        for (row, line) in pixels.chunks(*side as usize).enumerate() {
            let dy = to.y + row as i32;
            if !(0..self.device_height).contains(&dy) {
                continue;
            }
            for (col, &argb) in line.iter().enumerate() {
                let dx = to.x + col as i32;
                let alpha = argb >> 24;
                if alpha == 0 || !(0..self.device_width).contains(&dx) {
                    continue;
                }
                let at = (dy * self.device_width + dx) as usize;
                self.pixels[at] = Rgb(argb & 0x00ff_ffff).over(Rgb(self.pixels[at]), alpha).0;
            }
        }
    }

    /// Keeps `pixels` (the whole screen, its own pixels) as the
    /// wallpaper: drawn once, copied on every frame.
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

    /// The bitmap font: a fixed cell per character (logical), each drawn
    /// at the size nearest the scale; characters outside Basic Latin show
    /// as `?`.
    fn bitmap_text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        font: Font,
        colour: Rgb,
        clip: Rect,
    ) -> i32 {
        let (weight, logical, _) = font.spec();
        let height = raster_for(self.scale.len(logical.val() as i32));
        let advance = font.advance();
        let clip = self.scale.rect(clip);
        let top = self.scale.device(y);
        let mut pen = x;
        for c in text.chars() {
            let left = self.scale.device(pen);
            if left >= clip.x + clip.w {
                break;
            }
            let glyph = get_raster(c, weight, height).or_else(|| get_raster('?', weight, height));
            if let Some(glyph) = glyph.filter(|_| c != ' ') {
                for (gy, row) in glyph.raster().iter().enumerate() {
                    for (gx, &alpha) in row.iter().enumerate() {
                        let (px, py) = (left + gx as i32, top + gy as i32);
                        if alpha == 0
                            || !clip.contains(px, py)
                            || px >= self.device_width
                            || py >= self.device_height
                        {
                            continue;
                        }
                        let at = (py * self.device_width + px) as usize;
                        self.pixels[at] = colour.over(Rgb(self.pixels[at]), u32::from(alpha)).0;
                    }
                }
            }
            pen += advance;
        }
        pen
    }

    /// The pointer: an arrow with its tip at (`x`, `y`) of the screen's
    /// pixels (the pointer moves by them), as large as the scale makes it.
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
        let s = self.scale;
        for (dy, row) in ARROW.iter().enumerate() {
            for (dx, &cell) in row.iter().enumerate() {
                let colour = match cell {
                    b'X' => edge,
                    b'.' => fill,
                    _ => continue,
                };
                let cell = s.rect(Rect::new(dx as i32, dy as i32, 1, 1));
                for py in y + cell.y..y + cell.y + cell.h {
                    for px in x + cell.x..x + cell.x + cell.w {
                        if (0..self.device_width).contains(&px)
                            && (0..self.device_height).contains(&py)
                        {
                            self.pixels[(py * self.device_width + px) as usize] = colour.0;
                        }
                    }
                }
            }
        }
    }

    /// Writes the pixels that differ from what the screen shows; returns
    /// how many.
    pub fn present(&mut self) -> usize {
        let mut written = 0;
        let (r, g, b) = self.shifts;
        let (width, height) = (self.device_width as usize, self.device_height as usize);
        for y in 0..height {
            let row = y * width;
            for x in 0..width {
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

/// One box blur pass over `data` (a channel, `lines` lines of `len`
/// values, `step` apart within a line and `next` from one line to the
/// next): each value the mean of those within `reach` of it.
fn box_blur(
    data: &mut [u32],
    len: usize,
    lines: usize,
    step: usize,
    next: usize,
    reach: usize,
    scratch: &mut [u32],
) {
    let window = (2 * reach + 1) as u32;
    for line in 0..lines {
        let base = line * next;
        let at = |i: usize| base + i.min(len - 1) * step;
        // The window round the first value, its edges repeated.
        let mut sum = data[at(0)] * (reach as u32 + 1);
        for i in 1..=reach {
            sum += data[at(i)];
        }
        for i in 0..len {
            scratch[i] = sum / window;
            let leaving = data[at(i.saturating_sub(reach))];
            let coming = data[at(i + reach + 1)];
            sum = sum + coming - leaving;
        }
        for (i, &value) in scratch[..len].iter().enumerate() {
            data[base + i * step] = value;
        }
    }
}

/// How far row `dy` of a `height`-row shape with corners of `radius` is
/// inset at each end (as `oceans_draw` rounds its rectangles).
fn rounded_inset(dy: i32, height: i32, radius: i32) -> i32 {
    let above = if dy < radius {
        radius - dy
    } else if dy >= height - radius {
        dy - (height - radius) + 1
    } else {
        0
    };
    if above == 0 {
        return 0;
    }
    let n = radius * radius - (above - 1) * (above - 1);
    let mut root = 0;
    while (root + 1) * (root + 1) <= n {
        root += 1;
    }
    radius - root
}
