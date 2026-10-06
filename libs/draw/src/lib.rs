//! Drawing for Oceans (ADR-0080): the shapes and text the desktop and apps
//! draw with, on any buffer of `0x00RRGGBB` pixels. Host-tested.
//!
//! - [`Surface`]: a pixel buffer borrowed for drawing: fills, rounded
//!   rectangles, translucent tints, anti-aliased circles, soft shadows.
//! - [`Typesetter`]: interface text (ADR-0077), Noto Sans with Noto Sans
//!   Thai, rasterized with `ab_glyph` once per character and style.
//!
//! The display service draws the desktop with it, and apps draw their
//! windows with it (through `oceans-ui`), so both look the same.

#![no_std]

extern crate alloc;

mod text;

pub use oceans_window::Rect;
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

/// A pixel buffer borrowed for drawing: `width × height` pixels, row by
/// row, `0x00RRGGBB`. Everything is clipped to it.
pub struct Surface<'a> {
    pub width: i32,
    pub height: i32,
    pixels: &'a mut [u32],
}

impl<'a> Surface<'a> {
    /// `None` if `pixels` is not `width × height` pixels.
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Option<Self> {
        (pixels.len() == width * height).then_some(Self {
            width: width as i32,
            height: height as i32,
            pixels,
        })
    }

    pub fn pixel(&self, x: i32, y: i32) -> Option<Rgb> {
        (x >= 0 && y >= 0 && x < self.width && y < self.height)
            .then(|| Rgb(self.pixels[(y * self.width + x) as usize]))
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        self.pixels
    }

    fn clip(&self, r: Rect) -> Option<(i32, i32, i32, i32)> {
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = (r.x + r.w).min(self.width);
        let y1 = (r.y + r.h).min(self.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    fn blend(&mut self, x: i32, y: i32, colour: Rgb, alpha: u32) {
        if x >= 0 && y >= 0 && x < self.width && y < self.height {
            let at = (y * self.width + x) as usize;
            self.pixels[at] = colour.over(Rgb(self.pixels[at]), alpha).0;
        }
    }

    pub fn fill(&mut self, r: Rect, colour: Rgb) {
        let Some((x0, y0, x1, y1)) = self.clip(r) else {
            return;
        };
        for y in y0..y1 {
            let row = (y * self.width) as usize;
            self.pixels[row + x0 as usize..row + x1 as usize].fill(colour.0);
        }
    }

    /// A rectangle with rounded corners of `radius` pixels: the corners'
    /// pixels outside the circle are left as they are, and the edge pixel
    /// of each corner row is blended for a smoother curve.
    pub fn round_fill(&mut self, r: Rect, radius: i32, colour: Rgb) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            let inset = corner_inset(dy, r.h, radius);
            let y = r.y + dy;
            self.fill(Rect::new(r.x + inset, y, r.w - 2 * inset, 1), colour);
            if inset > 0 {
                self.blend(r.x + inset - 1, y, colour, 110);
                self.blend(r.x + r.w - inset, y, colour, 110);
            }
        }
    }

    /// `colour` laid over `r` with `alpha` (0..=255), with rounded corners
    /// of `radius`: a translucent surface.
    pub fn tint(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        for dy in 0..r.h {
            let inset = corner_inset(dy, r.h, radius);
            let row = Rect::new(r.x + inset, r.y + dy, r.w - 2 * inset, 1);
            let Some((x0, y0, x1, _)) = self.clip(row) else {
                continue;
            };
            let at = (y0 * self.width) as usize;
            for pixel in &mut self.pixels[at + x0 as usize..at + x1 as usize] {
                *pixel = colour.over(Rgb(*pixel), alpha).0;
            }
        }
    }

    /// A one-pixel outline with rounded corners.
    pub fn outline(&mut self, r: Rect, radius: i32, colour: Rgb, alpha: u32) {
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

    /// A filled circle of radius `r` around (`cx`, `cy`), its edge
    /// anti-aliased (four samples a pixel).
    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, colour: Rgb) {
        let r4 = r * 4;
        for y in cy - r - 1..=cy + r + 1 {
            for x in cx - r - 1..=cx + r + 1 {
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
                    self.blend(x, y, colour, inside * 255 / 4);
                }
            }
        }
    }

    /// A soft shadow under `r`: darker near it, fading over `spread`
    /// pixels, a little lower than the surface.
    pub fn shadow(&mut self, r: Rect, spread: i32, strength: u32) {
        let spread = spread.max(1);
        for step in (1..=spread).rev() {
            self.tint(
                Rect::new(
                    r.x - step,
                    r.y - step + spread / 2,
                    r.w + 2 * step,
                    r.h + 2 * step,
                ),
                step + 6,
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
        let (x, y) = at;
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
                    if alpha != 0 && clip.contains(px, py) {
                        self.blend(px, py, colour, u32::from(alpha));
                    }
                }
            }
            pen += glyph.advance;
        }
        pen
    }
}

#[cfg(test)]
mod tests;
