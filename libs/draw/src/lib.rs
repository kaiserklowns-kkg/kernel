//! Drawing for Oceans (ADR-0080): the shapes and text the desktop and apps
//! draw with, on any buffer of `0x00RRGGBB` pixels. Host-tested.
//!
//! - [`Surface`]: a pixel buffer borrowed for drawing: fills, rounded
//!   rectangles, translucent tints, anti-aliased circles, soft shadows,
//!   and filled outlines ([`Path`]: the system's icons, [`icon`]).
//! - [`Typesetter`]: interface text (ADR-0077), Noto Sans with Noto Sans
//!   Thai, rasterized with `ab_glyph` once per character, style and scale.
//!
//! **Scale** (ADR-0136): a surface is given in logical pixels, the units
//! everything is laid out in, and draws them at its [`Scale`]: shapes,
//! curves and text are drawn at the buffer's own resolution, so a 125 %
//! interface is larger and still sharp. At 100 % nothing changes.
//!
//! The display service draws the desktop with it, and apps draw their
//! windows with it (through `oceans-ui`), so both look the same.

#![no_std]

extern crate alloc;

pub mod icon;
pub mod layout;
mod path;
mod text;
pub mod theme;

pub use oceans_window::{Rect, Scale};
pub use path::Path;
pub use text::{Glyph, Style, Typesetter};

/// A colour, 0xRRGGBB.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u32);

impl Rgb {
    fn channels(self) -> (u32, u32, u32) {
        ((self.0 >> 16) & 0xff, (self.0 >> 8) & 0xff, self.0 & 0xff)
    }

    /// `self` over `under` with `alpha` (0..=255).
    pub fn over(self, under: Rgb, alpha: u32) -> Rgb {
        let alpha = alpha.min(255);
        let (r1, g1, b1) = self.channels();
        let (r0, g0, b0) = under.channels();
        let mix = |a: u32, b: u32| (b * (255 - alpha) + a * alpha) / 255;
        Rgb((mix(r1, r0) << 16) | (mix(g1, g0) << 8) | mix(b1, b0))
    }
}

/// The integer square root of a small non-negative number.
fn isqrt(n: i32) -> i32 {
    let mut root = 0;
    while (root + 1) * (root + 1) <= n {
        root += 1;
    }
    root
}

/// How far row `dy` of a `height`-row shape with corners of `radius` is
/// inset at each end.
fn corner_inset(dy: i32, height: i32, radius: i32) -> i32 {
    let above = if dy < radius {
        radius - dy
    } else if dy >= height - radius {
        dy - (height - radius) + 1
    } else {
        0
    };
    if above == 0 {
        0
    } else {
        radius - isqrt(radius * radius - (above - 1) * (above - 1))
    }
}

/// `r` grown by `by` pixels on every side (shrunk if negative).
fn grow(r: Rect, by: i32) -> Rect {
    Rect::new(r.x - by, r.y - by, r.w + 2 * by, r.h + 2 * by)
}

/// A pixel buffer borrowed for drawing: `width × height` pixels at its
/// scale, row by row, `0x00RRGGBB`. Everything is clipped to it.
pub struct Surface<'a> {
    /// Its size in logical pixels (ADR-0136), what is laid out on it.
    pub width: i32,
    pub height: i32,
    /// Its size in its own pixels.
    device_width: i32,
    device_height: i32,
    scale: Scale,
    /// What drawing is kept to (ADR-0138): the whole buffer, or a part of
    /// it ([`Surface::clip_to`]), in device pixels: `x0, y0, x1, y1`.
    bounds: (i32, i32, i32, i32),
    /// That part, as asked for (logical).
    clip_area: Option<Rect>,
    pixels: &'a mut [u32],
}

impl<'a> Surface<'a> {
    /// `None` if `pixels` is not `width × height` pixels. At 100 %.
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Option<Self> {
        Self::scaled(pixels, width, height, Scale::ONE)
    }

    /// `width × height` device pixels drawn at `scale`: its logical size
    /// is theirs at that scale.
    pub fn scaled(
        pixels: &'a mut [u32],
        width: usize,
        height: usize,
        scale: Scale,
    ) -> Option<Self> {
        let (w, h) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
        (pixels.len() == width * height).then_some(Self {
            width: scale.logical_len(w),
            height: scale.logical_len(h),
            device_width: w,
            device_height: h,
            scale,
            bounds: (0, 0, w, h),
            clip_area: None,
            pixels,
        })
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// Its size in its own pixels.
    pub fn device_size(&self) -> (i32, i32) {
        (self.device_width, self.device_height)
    }

    /// Where logical `r` lies in the buffer's pixels: for drawing pixels
    /// of one's own (a photograph) at the buffer's resolution.
    pub fn device(&self, r: Rect) -> Rect {
        self.scale.rect(r)
    }

    /// The pixel at (`x`, `y`) of the buffer (device pixels).
    pub fn pixel(&self, x: i32, y: i32) -> Option<Rgb> {
        (x >= 0 && y >= 0 && x < self.device_width && y < self.device_height)
            .then(|| Rgb(self.pixels[(y * self.device_width + x) as usize]))
    }

    /// The buffer's pixels, `device_size` of them.
    pub fn pixels(&mut self) -> &mut [u32] {
        self.pixels
    }

    /// From now on, drawing only inside `area` (logical; `None`: anywhere
    /// on the surface): what scrolls out of a view is not drawn
    /// (ADR-0138).
    pub fn clip_to(&mut self, area: Option<Rect>) {
        self.clip_area = area;
        self.bounds = match area.map(|r| self.scale.rect(r)) {
            Some(r) => (
                r.x.max(0),
                r.y.max(0),
                (r.x + r.w).min(self.device_width),
                (r.y + r.h).min(self.device_height),
            ),
            None => (0, 0, self.device_width, self.device_height),
        };
    }

    /// The area drawing is kept to, if any ([`Surface::clip_to`]).
    pub fn clip_area(&self) -> Option<Rect> {
        self.clip_area
    }

    fn clip(&self, r: Rect) -> Option<(i32, i32, i32, i32)> {
        let (bx0, by0, bx1, by1) = self.bounds;
        let x0 = r.x.max(bx0);
        let y0 = r.y.max(by0);
        let x1 = (r.x + r.w).min(bx1);
        let y1 = (r.y + r.h).min(by1);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    fn blend(&mut self, x: i32, y: i32, colour: Rgb, alpha: u32) {
        let (x0, y0, x1, y1) = self.bounds;
        if x >= x0 && y >= y0 && x < x1 && y < y1 {
            let at = (y * self.device_width + x) as usize;
            self.pixels[at] = colour.over(Rgb(self.pixels[at]), alpha).0;
        }
    }

    pub fn fill(&mut self, r: Rect, colour: Rgb) {
        self.fill_px(self.scale.rect(r), colour);
    }

    fn fill_px(&mut self, r: Rect, colour: Rgb) {
        let Some((x0, y0, x1, y1)) = self.clip(r) else {
            return;
        };
        for y in y0..y1 {
            let row = (y * self.device_width) as usize;
            self.pixels[row + x0 as usize..row + x1 as usize].fill(colour.0);
        }
    }

    /// A rectangle with rounded corners of `radius` pixels: the corners'
    /// pixels outside the circle are left as they are, and the edge pixel
    /// of each corner row is blended for a smoother curve.
    pub fn round_fill(&mut self, r: Rect, radius: i32, colour: Rgb) {
        self.round_fill_px(self.scale.rect(r), self.scale.len(radius), colour);
    }

    fn round_fill_px(&mut self, r: Rect, radius: i32, colour: Rgb) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            let inset = corner_inset(dy, r.h, radius);
            let y = r.y + dy;
            self.fill_px(Rect::new(r.x + inset, y, r.w - 2 * inset, 1), colour);
            if inset > 0 {
                self.blend(r.x + inset - 1, y, colour, 110);
                self.blend(r.x + r.w - inset, y, colour, 110);
            }
        }
    }

    /// `colour` laid over `r` with `alpha` (0..=255), with rounded corners
    /// of `radius`: a translucent surface.
    pub fn tint(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
        self.tint_px(self.scale.rect(r), self.scale.len(radius), colour, alpha);
    }

    fn tint_px(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            let inset = corner_inset(dy, r.h, radius);
            let row = Rect::new(r.x + inset, r.y + dy, r.w - 2 * inset, 1);
            let Some((x0, y0, x1, _)) = self.clip(row) else {
                continue;
            };
            let at = (y0 * self.device_width) as usize;
            for pixel in &mut self.pixels[at + x0 as usize..at + x1 as usize] {
                *pixel = colour.over(Rgb(*pixel), alpha).0;
            }
        }
    }

    /// A one-pixel outline with rounded corners (as thick as one logical
    /// pixel is).
    pub fn outline(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
        let (r, radius) = (self.scale.rect(r), self.scale.len(radius));
        for i in 0..self.scale.len(1) {
            self.outline_px(grow(r, -i), (radius - i).max(0), colour, alpha);
        }
    }

    fn outline_px(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            let inset = corner_inset(dy, r.h, radius);
            let y = r.y + dy;
            if dy == 0 || dy == r.h - 1 {
                for x in r.x + inset..r.x + r.w - inset {
                    self.blend(x, y, colour, alpha);
                }
            } else {
                self.blend(r.x + inset, y, colour, alpha);
                self.blend(r.x + r.w - 1 - inset, y, colour, alpha);
            }
        }
    }

    /// A filled circle of radius `r` around the middle of pixel (`cx`,
    /// `cy`), its edge anti-aliased (sixteen samples a pixel).
    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, colour: Rgb) {
        let s = self.scale;
        // In eighths of a device pixel: the middle of the logical pixel,
        // and the radius at the scale.
        let cx8 = (s.device(cx) + s.device(cx + 1)) * 4;
        let cy8 = (s.device(cy) + s.device(cy + 1)) * 4;
        let r8 = (i64::from(r) * i64::from(s.percent()) * 8 / 100) as i32;
        let reach = r8 / 8 + 2;
        let (px, py) = (cx8 / 8, cy8 / 8);
        let r2 = i64::from(r8) * i64::from(r8);
        for y in py - reach..=py + reach {
            for x in px - reach..=px + reach {
                let mut inside = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let dx = i64::from(x * 8 + sx * 2 + 1 - cx8);
                        let dy = i64::from(y * 8 + sy * 2 + 1 - cy8);
                        if dx * dx + dy * dy <= r2 {
                            inside += 1;
                        }
                    }
                }
                if inside > 0 {
                    self.blend(x, y, colour, inside * 255 / 16);
                }
            }
        }
    }

    /// A soft shadow under `r`: darker near it, fading over `spread`
    /// pixels, a little lower than the surface.
    pub fn shadow(&mut self, r: Rect, spread: i32, strength: u32) {
        let r = self.scale.rect(r);
        let spread = self.scale.len(spread).max(1);
        let corner = self.scale.len(6);
        for step in (1..=spread).rev() {
            self.tint_px(
                Rect::new(
                    r.x - step,
                    r.y - step + spread / 2,
                    r.w + 2 * step,
                    r.h + 2 * step,
                ),
                step + corner,
                Rgb(0),
                strength / spread as u32,
            );
        }
    }

    /// Darkens everything (behind a modal dialog).
    pub fn dim(&mut self, shade: Rgb, alpha: u32) {
        for pixel in self.pixels.iter_mut() {
            *pixel = shade.over(Rgb(*pixel), alpha).0;
        }
    }

    /// Interface text from `at` (the line's top left), clipped to `clip`;
    /// returns the x after it.
    pub fn text(
        &mut self,
        typesetter: &mut Typesetter,
        at: (i32, i32),
        text: &str,
        style: Style,
        colour: Rgb,
        clip: Rect,
    ) -> i32 {
        typesetter.set_scale(self.scale);
        let clip = self.scale.rect(clip);
        let start = self.scale.device(at.0);
        let y = self.scale.device(at.1);
        let mut pen = start;
        for c in text.chars() {
            if pen >= clip.x + clip.w {
                break;
            }
            let glyph = typesetter.glyph(style, c);
            for gy in 0..glyph.height {
                for gx in 0..glyph.width {
                    let alpha = glyph.coverage[gy * glyph.width + gx];
                    let (px, py) = (pen + glyph.left + gx as i32, y + glyph.top + gy as i32);
                    if alpha != 0 && clip.contains(px, py) {
                        self.blend(px, py, colour, u32::from(alpha));
                    }
                }
            }
            pen += glyph.advance;
        }
        at.0 + self.scale.logical_len(pen - start)
    }
}

#[cfg(test)]
mod tests;
