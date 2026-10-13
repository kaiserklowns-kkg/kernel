//! The interface's scale (ADR-0136): how many of the screen's pixels one
//! pixel of the interface takes, in hundredths.
//!
//! Everything the desktop and apps lay out is in **logical pixels**: a
//! window 760 wide, a 28-pixel menu bar, 16-pixel text. Drawing turns them
//! into the screen's (device) pixels at the scale the user chose, or the
//! one the screen's size suggests, so the interface is the same physical
//! size on a dense laptop panel as on a desktop monitor, and sharp on both:
//! shapes and text are drawn at the screen's resolution, not enlarged.
//!
//! Rectangles are turned edge by edge, so ones that touch in logical
//! pixels touch on the screen too, with no gap or overlap at 125 %.

use crate::Rect;

/// A scale, in percent: one of [`Scale::CHOICES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scale(u16);

/// The smallest screen the desktop is laid out for, in logical pixels:
/// a scale that would leave less is not offered.
pub const MIN_LOGICAL: (i32, i32) = (1024, 720);

/// What a screen of this many pixels per inch shows at 100 %: the density
/// the interface was drawn for (a desktop monitor seen from arm's length).
const REFERENCE_PPI: u32 = 112;

impl Default for Scale {
    fn default() -> Self {
        Self::ONE
    }
}

impl Scale {
    pub const ONE: Self = Self(100);
    /// The scales offered, in percent.
    pub const CHOICES: [u16; 7] = [100, 125, 150, 175, 200, 250, 300];

    /// `None` unless `percent` is one of [`Scale::CHOICES`].
    pub fn new(percent: u16) -> Option<Self> {
        Self::CHOICES.contains(&percent).then_some(Self(percent))
    }

    pub fn percent(self) -> u16 {
        self.0
    }

    fn p(self) -> i64 {
        i64::from(self.0)
    }

    /// The device pixel where logical position `v` falls (rounded, so
    /// positions and edges map the same way).
    pub fn device(self, v: i32) -> i32 {
        (i64::from(v) * self.p() + 50).div_euclid(100) as i32
    }

    /// A length in device pixels, wherever it lies: a window's pixels, a
    /// corner's radius, a line's thickness (at least one for one).
    pub fn len(self, v: i32) -> i32 {
        let d = self.device(v);
        if v > 0 { d.max(1) } else { d }
    }

    /// The logical pixel that device pixel `d` lies in.
    pub fn logical(self, d: i32) -> i32 {
        let mut l = (i64::from(d) * 100).div_euclid(self.p()) as i32;
        while self.device(l + 1) <= d {
            l += 1;
        }
        while self.device(l) > d {
            l -= 1;
        }
        l
    }

    /// The logical length `d` device pixels make, rounded: the inverse of
    /// [`Scale::len`] (a window's logical size from its pixels').
    pub fn logical_len(self, d: i32) -> i32 {
        (i64::from(d) * 100 + self.p() / 2).div_euclid(self.p()) as i32
    }

    /// The logical length that fits in `d` device pixels (a screen's).
    pub fn logical_fit(self, d: i32) -> i32 {
        (i64::from(d) * 100).div_euclid(self.p()) as i32
    }

    /// `r` on the screen: its edges turned, so rectangles that touch still
    /// touch.
    pub fn rect(self, r: Rect) -> Rect {
        let (x0, y0) = (self.device(r.x), self.device(r.y));
        let (x1, y1) = (self.device(r.x + r.w), self.device(r.y + r.h));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Whether a `width × height` screen keeps at least [`MIN_LOGICAL`] at
    /// this scale (100 % always does: nothing is smaller than the screen).
    pub fn fits(self, width: i32, height: i32) -> bool {
        self == Self::ONE
            || (self.logical_fit(width) >= MIN_LOGICAL.0
                && self.logical_fit(height) >= MIN_LOGICAL.1)
    }

    /// The scales a `width × height` screen can take, smallest first.
    pub fn choices(width: i32, height: i32) -> impl Iterator<Item = Scale> {
        Self::CHOICES
            .into_iter()
            .map(Self)
            .filter(move |s| s.fits(width, height))
    }

    /// The scale for a `width × height` screen `mm` millimetres wide (from
    /// its EDID, if it said): its density over [`REFERENCE_PPI`], to the
    /// nearest choice that fits. Without a believable size, from the
    /// resolution alone, as a laptop of it would want.
    pub fn auto(width: i32, height: i32, mm: Option<(u16, u16)>) -> Scale {
        let wanted = match mm {
            // Below 15 cm or over 1.5 m it is an aspect ratio, a projector
            // or wrong.
            Some((mm_w, _)) if (150..=1500).contains(&mm_w) && width > 0 => {
                let ppi = width as u32 * 254 / (u32::from(mm_w) * 10);
                ppi * 100 / REFERENCE_PPI
            }
            _ => match height {
                2000.. => 200,
                1400.. => 150,
                1050.. => 125,
                _ => 100,
            },
        };
        let mut best = Self::ONE;
        for choice in Self::choices(width, height) {
            // The nearest: a choice is taken once `wanted` is past the
            // middle between it and the one before.
            if u32::from(choice.0) <= wanted + 12 {
                best = choice;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_is_identity() {
        let s = Scale::ONE;
        for v in [-7, 0, 1, 28, 1279] {
            assert_eq!(s.device(v), v);
            assert_eq!(s.logical(v), v);
            assert_eq!(s.len(v), v);
            assert_eq!(s.logical_len(v), v);
        }
        let r = Rect::new(3, 4, 50, 60);
        assert_eq!(s.rect(r), r);
    }

    #[test]
    fn touching_rects_still_touch() {
        for percent in Scale::CHOICES {
            let s = Scale::new(percent).unwrap();
            for x in -20..200 {
                let a = s.rect(Rect::new(x, 0, 1, 1));
                let b = s.rect(Rect::new(x + 1, 0, 1, 1));
                assert_eq!(a.x + a.w, b.x, "{percent} at {x}");
                assert!(a.w >= 1);
            }
        }
    }

    #[test]
    fn logical_inverts_device() {
        for percent in Scale::CHOICES {
            let s = Scale::new(percent).unwrap();
            for d in -50..500 {
                let l = s.logical(d);
                assert!(s.device(l) <= d && d < s.device(l + 1), "{percent} {d}");
            }
            // A window's size survives the trip to its pixels and back.
            for w in 1..2000 {
                assert_eq!(s.logical_len(s.len(w)), w, "{percent} {w}");
            }
        }
    }

    #[test]
    fn only_choices() {
        assert_eq!(Scale::new(0), None);
        assert_eq!(Scale::new(110), None);
        assert_eq!(Scale::new(125).map(Scale::percent), Some(125));
    }

    #[test]
    fn choices_keep_room() {
        let list = |w, h| {
            Scale::choices(w, h)
                .map(Scale::percent)
                .collect::<alloc::vec::Vec<_>>()
        };
        assert_eq!(list(1280, 800), [100]);
        assert_eq!(list(1920, 1080), [100, 125, 150]);
        assert_eq!(list(2560, 1440), [100, 125, 150, 175, 200]);
        assert_eq!(list(3840, 2160), [100, 125, 150, 175, 200, 250, 300]);
        // A small screen still has 100 %.
        assert_eq!(list(800, 600), [100]);
    }

    #[test]
    fn auto_from_density() {
        let auto = |w, h, mm| Scale::auto(w, h, mm).percent();
        // A 15.6" 1080p laptop (344 mm, 141 ppi).
        assert_eq!(auto(1920, 1080, Some((344, 194))), 125);
        // A 14" 1080p laptop (309 mm, 158 ppi).
        assert_eq!(auto(1920, 1080, Some((309, 174))), 150);
        // A 24" 1080p monitor (531 mm, 92 ppi).
        assert_eq!(auto(1920, 1080, Some((531, 299))), 100);
        // A 27" 1440p monitor (597 mm, 109 ppi).
        assert_eq!(auto(2560, 1440, Some((597, 336))), 100);
        // A 15.6" 4K laptop (344 mm, 283 ppi): as much as offered.
        assert_eq!(auto(3840, 2160, Some((344, 194))), 250);
        // QEMU says 100 dpi.
        assert_eq!(auto(1280, 800, Some((325, 203))), 100);
        // An aspect ratio or nothing: by resolution.
        assert_eq!(auto(1920, 1080, Some((16, 9))), 125);
        assert_eq!(auto(1920, 1080, None), 125);
        assert_eq!(auto(3840, 2160, None), 200);
        assert_eq!(auto(1366, 768, None), 100);
        // Never more than fits.
        assert_eq!(auto(1920, 1080, Some((200, 112))), 150);
    }
}
