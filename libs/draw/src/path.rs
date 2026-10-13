//! Filled outlines (ADR-0136): lines, curves and arcs in logical pixels,
//! filled anti-aliased at the surface's scale by the rasterizer `ab_glyph`
//! fills glyphs with. The system's icons ([`crate::icon`]) are drawn so,
//! sharp at any scale.
//!
//! Coverage adds up by direction: a shape drawn inside another the other
//! way round is a hole in it (a ring, a crescent); the same way round they
//! merge.

use alloc::vec::Vec;

use ab_glyph_rasterizer::{Point, Rasterizer, point};

use crate::{Rgb, Surface};

#[derive(Clone, Copy, Debug)]
enum Segment {
    Line(Point, Point),
    Quad(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

/// An outline to fill: closed shapes, in logical pixels from where it is
/// drawn.
#[derive(Clone, Debug, Default)]
pub struct Path {
    segments: Vec<Segment>,
    start: Option<Point>,
    at: Point,
}

/// How far a cubic's control points lie along the tangents for a quarter
/// circle: 4/3·tan(π/8).
const KAPPA: f32 = 0.552_284_8;

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a shape at (`x`, `y`), closing the one before.
    pub fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.close();
        self.start = Some(point(x, y));
        self.at = point(x, y);
        self
    }

    pub fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        let to = point(x, y);
        self.begin();
        self.segments.push(Segment::Line(self.at, to));
        self.at = to;
        self
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) -> &mut Self {
        let to = point(x, y);
        self.begin();
        self.segments
            .push(Segment::Quad(self.at, point(cx, cy), to));
        self.at = to;
        self
    }

    pub fn cubic_to(&mut self, c1: (f32, f32), c2: (f32, f32), x: f32, y: f32) -> &mut Self {
        let to = point(x, y);
        self.begin();
        self.segments.push(Segment::Cubic(
            self.at,
            point(c1.0, c1.1),
            point(c2.0, c2.1),
            to,
        ));
        self.at = to;
        self
    }

    /// An arc round (`cx`, `cy`) of radius `r`, from angle `from` to `to`
    /// (radians, clockwise on the screen from the right), joined to where
    /// the outline is by a line.
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, from: f32, to: f32) -> &mut Self {
        let at = |a: f32| (cx + r * libm::cosf(a), cy + r * libm::sinf(a));
        let (x0, y0) = at(from);
        if self.start.is_none() {
            self.move_to(x0, y0);
        } else {
            self.line_to(x0, y0);
        }
        // In parts of at most a quarter turn, each a cubic.
        let sweep = to - from;
        let parts =
            (libm::fabsf(sweep) / (core::f32::consts::FRAC_PI_2) - 1e-4).max(0.0) as usize + 1;
        let step = sweep / parts as f32;
        let k = 4.0 / 3.0 * libm::tanf(step / 4.0);
        let mut a = from;
        for _ in 0..parts {
            let b = a + step;
            let (ax, ay) = at(a);
            let (bx, by) = at(b);
            let c1 = (ax - k * r * libm::sinf(a), ay + k * r * libm::cosf(a));
            let c2 = (bx + k * r * libm::sinf(b), by - k * r * libm::cosf(b));
            self.cubic_to(c1, c2, bx, by);
            a = b;
        }
        self
    }

    /// Ends the shape, back to where it started.
    pub fn close(&mut self) -> &mut Self {
        if let Some(start) = self.start.take()
            && (start.x != self.at.x || start.y != self.at.y)
        {
            self.segments.push(Segment::Line(self.at, start));
            self.at = start;
        }
        self
    }

    /// A circle, clockwise (`hole`: the other way, to cut one out).
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, hole: bool) -> &mut Self {
        let k = KAPPA * r;
        let s = if hole { -1.0 } else { 1.0 };
        self.move_to(cx + r, cy);
        self.cubic_to((cx + r, cy + s * k), (cx + k, cy + s * r), cx, cy + s * r);
        self.cubic_to((cx - k, cy + s * r), (cx - r, cy + s * k), cx - r, cy);
        self.cubic_to((cx - r, cy - s * k), (cx - k, cy - s * r), cx, cy - s * r);
        self.cubic_to((cx + k, cy - s * r), (cx + r, cy - s * k), cx + r, cy);
        self.close()
    }

    /// A rectangle with corners of radius `r`, clockwise.
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) -> &mut Self {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let k = KAPPA * r;
        let (x1, y1) = (x + w, y + h);
        self.move_to(x + r, y);
        self.line_to(x1 - r, y);
        self.cubic_to((x1 - r + k, y), (x1, y + r - k), x1, y + r);
        self.line_to(x1, y1 - r);
        self.cubic_to((x1, y1 - r + k), (x1 - r + k, y1), x1 - r, y1);
        self.line_to(x + r, y1);
        self.cubic_to((x + r - k, y1), (x, y1 - r + k), x, y1 - r);
        self.line_to(x, y + r);
        self.cubic_to((x, y + r - k), (x + r - k, y), x + r, y);
        self.close()
    }

    /// The line from (`x0`, `y0`) to (`x1`, `y1`), `width` thick, with
    /// round ends: a stroke.
    pub fn stroke(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32) -> &mut Self {
        let half = width / 2.0;
        let angle = libm::atan2f(y1 - y0, x1 - x0);
        let quarter = core::f32::consts::FRAC_PI_2;
        self.close();
        self.arc(x1, y1, half, angle - quarter, angle + quarter);
        self.arc(x0, y0, half, angle + quarter, angle + 3.0 * quarter);
        self.close()
    }

    /// `other`'s shapes added the other way round: where they lie on this
    /// path's, they cut holes in them.
    pub fn append_reversed(&mut self, other: &Path) -> &mut Self {
        let mut other = other.clone();
        other.close();
        self.close();
        for segment in other.segments.iter().rev() {
            self.segments.push(match *segment {
                Segment::Line(a, b) => Segment::Line(b, a),
                Segment::Quad(a, b, c) => Segment::Quad(c, b, a),
                Segment::Cubic(a, b, c, d) => Segment::Cubic(d, c, b, a),
            });
        }
        self
    }

    fn begin(&mut self) {
        if self.start.is_none() {
            self.start = Some(self.at);
        }
    }

    fn points(&self) -> impl Iterator<Item = Point> + '_ {
        self.segments.iter().flat_map(|s| {
            let (a, b, c, d) = match *s {
                Segment::Line(a, b) => (a, b, b, b),
                Segment::Quad(a, b, c) => (a, b, c, c),
                Segment::Cubic(a, b, c, d) => (a, b, c, d),
            };
            [a, b, c, d]
        })
    }
}

impl Surface<'_> {
    /// Fills `path` drawn from (`x`, `y`), anti-aliased at the surface's
    /// scale; what lies outside the surface is left out.
    pub fn fill_path(&mut self, path: &Path, x: i32, y: i32, colour: Rgb) {
        self.fill_path_alpha(path, x, y, colour, 255);
    }

    /// As [`Surface::fill_path`], `alpha` (0..=255) over what is there.
    pub fn fill_path_alpha(&mut self, path: &Path, x: i32, y: i32, colour: Rgb, alpha: u32) {
        let mut shape = path.clone();
        shape.close();
        if shape.segments.is_empty() {
            return;
        }
        let s = f32::from(self.scale().percent()) / 100.0;
        let (ox, oy) = (x as f32, y as f32);
        let to_device = |p: Point| point((p.x + ox) * s, (p.y + oy) * s);
        // The pixels it may touch, clipped to the surface.
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in shape.points().map(to_device) {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
        let (dw, dh) = self.device_size();
        let bx0 = (libm::floorf(x0) as i32).max(0);
        let by0 = (libm::floorf(y0) as i32).max(0);
        let bx1 = (libm::ceilf(x1) as i32 + 1).min(dw);
        let by1 = (libm::ceilf(y1) as i32 + 1).min(dh);
        if bx0 >= bx1 || by0 >= by1 {
            return;
        }
        let (w, h) = ((bx1 - bx0) as usize, (by1 - by0) as usize);
        let mut raster = Rasterizer::new(w, h);
        let local = |p: Point| {
            let p = to_device(p);
            point(p.x - bx0 as f32, p.y - by0 as f32)
        };
        for segment in &shape.segments {
            match *segment {
                Segment::Line(a, b) => raster.draw_line(local(a), local(b)),
                Segment::Quad(a, b, c) => raster.draw_quad(local(a), local(b), local(c)),
                Segment::Cubic(a, b, c, d) => {
                    raster.draw_cubic(local(a), local(b), local(c), local(d))
                }
            }
        }
        let alpha = alpha.min(255) as f32;
        raster.for_each_pixel_2d(|px, py, coverage| {
            let a = (coverage.clamp(0.0, 1.0) * alpha + 0.5) as u32;
            if a > 0 {
                self.blend(bx0 + px as i32, by0 + py as i32, colour, a);
            }
        });
    }
}
