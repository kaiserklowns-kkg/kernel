//! Interface text (ADR-0077): Noto Sans, proportional and anti-aliased,
//! with Noto Sans Thai for Thai. Each character is rasterized once per
//! style and kept.
//! (The display's Terminal keeps a monospaced bitmap font of its own.)

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _, point};

static LATIN_REGULAR: &[u8] = include_bytes!("../fonts/NotoSans-Regular.ttf");
static LATIN_BOLD: &[u8] = include_bytes!("../fonts/NotoSans-Bold.ttf");
static THAI_REGULAR: &[u8] = include_bytes!("../fonts/NotoSansThai-Regular.ttf");
static THAI_BOLD: &[u8] = include_bytes!("../fonts/NotoSansThai-Bold.ttf");

/// An interface text style: weight and size.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Style {
    Body,
    Strong,
    Title,
}

impl Style {
    fn bold(self) -> bool {
        !matches!(self, Self::Body)
    }

    /// The scale: the font's height in pixels.
    fn px(self) -> f32 {
        match self {
            Self::Body | Self::Strong => 16.0,
            Self::Title => 21.0,
        }
    }
}

/// A rasterized character.
pub struct Glyph {
    /// From the pen to the coverage's left edge.
    pub left: i32,
    /// From the line's top to the coverage's top.
    pub top: i32,
    pub width: usize,
    pub height: usize,
    /// How far the pen moves (0 for a combining mark).
    pub advance: i32,
    /// `width × height` coverage, 0..=255.
    pub coverage: Vec<u8>,
}

/// Whether Noto Sans Thai draws `c`.
fn thai(c: char) -> bool {
    ('\u{0e00}'..='\u{0e7f}').contains(&c)
}

pub struct Typesetter {
    faces: [FontRef<'static>; 4],
    glyphs: BTreeMap<(Style, char), Glyph>,
}

impl Typesetter {
    /// `None` if a bundled font does not parse (it always does).
    pub fn new() -> Option<Self> {
        Some(Self {
            faces: [
                FontRef::try_from_slice(LATIN_REGULAR).ok()?,
                FontRef::try_from_slice(LATIN_BOLD).ok()?,
                FontRef::try_from_slice(THAI_REGULAR).ok()?,
                FontRef::try_from_slice(THAI_BOLD).ok()?,
            ],
            glyphs: BTreeMap::new(),
        })
    }

    fn face(&self, style: Style, c: char) -> &FontRef<'static> {
        let index = usize::from(thai(c)) * 2 + usize::from(style.bold());
        &self.faces[index]
    }

    /// The character `c` in `style`, rasterized the first time it is asked
    /// for. Characters no font has show as `?`.
    pub fn glyph(&mut self, style: Style, c: char) -> &Glyph {
        if !self.glyphs.contains_key(&(style, c)) {
            let glyph = self.rasterize(style, c);
            self.glyphs.insert((style, c), glyph);
        }
        &self.glyphs[&(style, c)]
    }

    fn rasterize(&self, style: Style, c: char) -> Glyph {
        let mut face = self.face(style, c);
        let mut id = face.glyph_id(c);
        if id.0 == 0 && c != ' ' {
            face = self.face(style, '?');
            id = face.glyph_id('?');
        }
        // Noto Sans Thai draws smaller than Noto Sans at the same size:
        // Thai is scaled up to match, on the Latin font's baseline.
        let px = if thai(c) {
            style.px() * 1.15
        } else {
            style.px()
        };
        let scale = PxScale::from(px);
        let scaled = face.as_scaled(scale);
        let latin = &self.faces[usize::from(style.bold())];
        let ascent = latin.as_scaled(PxScale::from(style.px())).ascent();
        // Rounded (no `f32::round` without std).
        let advance = (scaled.h_advance(id) + 0.5) as i32;
        let Some(outline) =
            face.outline_glyph(id.with_scale_and_position(scale, point(0.0, ascent)))
        else {
            // A space: nothing to draw.
            return Glyph {
                left: 0,
                top: 0,
                width: 0,
                height: 0,
                advance,
                coverage: Vec::new(),
            };
        };
        let bounds = outline.px_bounds();
        let (width, height) = (bounds.width() as usize, bounds.height() as usize);
        let mut coverage = alloc::vec![0u8; width * height];
        outline.draw(|x, y, amount| {
            let (x, y) = (x as usize, y as usize);
            if x < width && y < height {
                coverage[y * width + x] = (amount.clamp(0.0, 1.0) * 255.0) as u8;
            }
        });
        Glyph {
            left: bounds.min.x as i32,
            top: bounds.min.y as i32,
            width,
            height,
            advance,
            coverage,
        }
    }

    /// The width of `text` in `style`.
    pub fn measure(&mut self, text: &str, style: Style) -> i32 {
        text.chars().map(|c| self.glyph(style, c).advance).sum()
    }
}
