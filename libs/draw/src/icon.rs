//! The system's icons (ADR-0136): drawn from outlines on a 24-unit grid,
//! filled anti-aliased at the surface's scale, so they are as sharp in the
//! menu bar of a 4K panel as at 100 %. Each is drawn centred in a box, as
//! large as the box's shorter side; `ink` is its colour, `faint` the
//! colour of what is off (a signal's unlit bars).
//!
//! They follow the shapes people know from other systems, so they read at
//! a glance: Wi-Fi's fan, Ethernet's ports, a speaker with waves, a bell,
//! two switches for Control Center, a person.

use core::f32::consts::PI;

use crate::{Path, Rect, Rgb, Surface};

/// A 24-unit grid laid on the square centred in `r`.
struct Grid {
    x: i32,
    y: i32,
    u: f32,
}

impl Grid {
    fn new(r: Rect) -> Self {
        let side = r.w.min(r.h);
        Self {
            x: r.x + (r.w - side) / 2,
            y: r.y + (r.h - side) / 2,
            u: side as f32 / 24.0,
        }
    }

    fn fill(&self, s: &mut Surface<'_>, path: &Path, ink: Rgb) {
        s.fill_path(path, self.x, self.y, ink);
    }

    /// A ring's part round (`cx`, `cy`) between radii `inner` and `outer`,
    /// from angle `from` to `to` (clockwise, radians), with round ends.
    fn band(&self, p: &mut Path, (cx, cy): (f32, f32), inner: f32, outer: f32, from: f32, to: f32) {
        let u = self.u;
        let (cx, cy) = (cx * u, cy * u);
        let (inner, outer) = (inner * u, outer * u);
        let mid = (inner + outer) / 2.0;
        let half = (outer - inner) / 2.0;
        let end = |a: f32| (cx + mid * libm::cosf(a), cy + mid * libm::sinf(a));
        p.move_to(cx + outer * libm::cosf(from), cy + outer * libm::sinf(from));
        p.arc(cx, cy, outer, from, to);
        let (ex, ey) = end(to);
        p.arc(ex, ey, half, to, to + PI);
        p.arc(cx, cy, inner, to, from);
        let (sx, sy) = end(from);
        p.arc(sx, sy, half, from + PI, from + 2.0 * PI);
        p.close();
    }

    fn circle(&self, p: &mut Path, cx: f32, cy: f32, r: f32, hole: bool) {
        p.circle(cx * self.u, cy * self.u, r * self.u, hole);
    }

    fn round_rect(&self, p: &mut Path, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let u = self.u;
        p.round_rect(x * u, y * u, w * u, h * u, r * u);
    }

    fn stroke(&self, p: &mut Path, (x0, y0): (f32, f32), (x1, y1): (f32, f32), width: f32) {
        let u = self.u;
        p.stroke(x0 * u, y0 * u, x1 * u, y1 * u, width * u);
    }
}

/// Wi-Fi's three bands over the dot, upwards from (12, 20).
const WIFI_BANDS: [(f32, f32); 3] = [(5.0, 7.6), (9.8, 12.4), (14.6, 17.2)];
const WIFI_CENTRE: (f32, f32) = (12.0, 20.0);
/// The fan's half-angle either side of straight up.
const WIFI_SPREAD: f32 = 0.74;

/// Wi-Fi: the dot and three bands, the first `bars` (0..=3) in `ink`, the
/// rest in `faint`.
pub fn wifi(s: &mut Surface<'_>, r: Rect, bars: u8, ink: Rgb, faint: Rgb) {
    let g = Grid::new(r);
    let up = -PI / 2.0;
    let mut dot = Path::new();
    g.circle(&mut dot, WIFI_CENTRE.0, WIFI_CENTRE.1, 2.2, false);
    g.fill(s, &dot, ink);
    for (i, &(inner, outer)) in WIFI_BANDS.iter().enumerate() {
        let mut band = Path::new();
        g.band(
            &mut band,
            WIFI_CENTRE,
            inner,
            outer,
            up - WIFI_SPREAD,
            up + WIFI_SPREAD,
        );
        g.fill(s, &band, if (i as u8) < bars { ink } else { faint });
    }
}

/// Wi-Fi on but not joined: the fan in `faint`, struck through in `ink`.
pub fn wifi_off(s: &mut Surface<'_>, r: Rect, ink: Rgb, faint: Rgb) {
    wifi(s, r, 0, faint, faint);
    let g = Grid::new(r);
    let mut slash = Path::new();
    g.stroke(&mut slash, (4.0, 3.5), (20.0, 21.0), 2.0);
    g.fill(s, &slash, ink);
}

/// The bars a signal of `rssi` dBm lights (ADR-0130): -60 and above all
/// three, -72 and above two, else one.
pub fn wifi_bars(rssi: i8) -> u8 {
    match rssi {
        -60.. => 3,
        -72..=-61 => 2,
        _ => 1,
    }
}

/// Ethernet: a network port between two chevrons, `<···>`, as other
/// systems show a wired connection; struck through when not connected.
pub fn ethernet(s: &mut Surface<'_>, r: Rect, connected: bool, ink: Rgb) {
    let g = Grid::new(r);
    let mut p = Path::new();
    g.stroke(&mut p, (6.0, 7.0), (1.5, 12.0), 2.0);
    g.stroke(&mut p, (1.5, 12.0), (6.0, 17.0), 2.0);
    g.stroke(&mut p, (18.0, 7.0), (22.5, 12.0), 2.0);
    g.stroke(&mut p, (22.5, 12.0), (18.0, 17.0), 2.0);
    for x in [8.0, 12.0, 16.0] {
        g.circle(&mut p, x, 12.0, 1.6, false);
    }
    g.fill(s, &p, ink);
    if !connected {
        let mut slash = Path::new();
        g.stroke(&mut slash, (4.0, 3.5), (20.0, 20.5), 2.0);
        g.fill(s, &slash, ink);
    }
}

/// A speaker: its body and cone, then up to three waves for `level` (0 to
/// 100), or a cross when `muted` or silent.
pub fn speaker(s: &mut Surface<'_>, r: Rect, level: u8, muted: bool, ink: Rgb) {
    let g = Grid::new(r);
    let u = g.u;
    let mut p = Path::new();
    // The body and the cone, one outline.
    g.round_rect(&mut p, 1.0, 8.5, 5.0, 7.0, 1.2);
    p.move_to(4.5 * u, 8.5 * u)
        .line_to(9.0 * u, 4.2 * u)
        .quad_to(10.6 * u, 3.2 * u, 10.6 * u, 5.0 * u)
        .line_to(10.6 * u, 19.0 * u)
        .quad_to(10.6 * u, 20.8 * u, 9.0 * u, 19.8 * u)
        .line_to(4.5 * u, 15.5 * u)
        .close();
    if muted || level == 0 {
        g.stroke(&mut p, (14.5, 9.0), (20.5, 15.0), 2.0);
        g.stroke(&mut p, (20.5, 9.0), (14.5, 15.0), 2.0);
        g.fill(s, &p, ink);
        return;
    }
    let waves = match level {
        1..=33 => 1,
        34..=66 => 2,
        _ => 3,
    };
    for &(inner, outer) in [(4.4, 6.2), (7.8, 9.6), (11.2, 13.0)].iter().take(waves) {
        g.band(&mut p, (10.0, 12.0), inner, outer, -0.8, 0.8);
    }
    g.fill(s, &p, ink);
}

/// The notifications: a bell, its clapper below.
pub fn bell(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let u = g.u;
    let mut p = Path::new();
    p.move_to(12.0 * u, 3.0 * u)
        .cubic_to((16.6 * u, 3.0 * u), (18.4 * u, 6.6 * u), 18.4 * u, 10.2 * u)
        .line_to(18.4 * u, 14.0 * u)
        .quad_to(18.6 * u, 15.8 * u, 20.6 * u, 17.0 * u)
        .quad_to(21.4 * u, 17.6 * u, 21.0 * u, 18.2 * u)
        .line_to(3.0 * u, 18.2 * u)
        .quad_to(2.6 * u, 17.6 * u, 3.4 * u, 17.0 * u)
        .quad_to(5.4 * u, 15.8 * u, 5.6 * u, 14.0 * u)
        .line_to(5.6 * u, 10.2 * u)
        .cubic_to((5.6 * u, 6.6 * u), (7.4 * u, 3.0 * u), 12.0 * u, 3.0 * u)
        .close();
    g.circle(&mut p, 12.0, 2.6, 1.4, false);
    // The clapper: a half circle under the rim.
    p.move_to(9.4 * u, 19.6 * u)
        .line_to(14.6 * u, 19.6 * u)
        .arc(12.0 * u, 19.6 * u, 2.6 * u, 0.0, PI)
        .close();
    g.fill(s, &p, ink);
}

/// Control Center: two switches, one on each side.
pub fn controls(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let mut p = Path::new();
    for (y, knob) in [(4.0, 16.5), (13.0, 7.5)] {
        // A pill, hollow, and its knob.
        g.round_rect(&mut p, 2.0, y, 20.0, 8.0, 4.0);
        let u = g.u;
        let mut hole = Path::new();
        hole.round_rect(3.8 * u, (y + 1.8) * u, 16.4 * u, 4.4 * u, 2.2 * u);
        reverse_into(&mut p, &hole);
        g.circle(&mut p, knob, y + 4.0, 2.6, false);
    }
    g.fill(s, &p, ink);
}

/// The user: a head and shoulders in a circle.
pub fn user(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let u = g.u;
    let mut p = Path::new();
    g.circle(&mut p, 12.0, 12.0, 10.5, false);
    g.circle(&mut p, 12.0, 12.0, 8.8, true);
    g.circle(&mut p, 12.0, 9.6, 3.6, false);
    // The shoulders, cut by the ring's inside.
    p.move_to(5.6 * u, 18.4 * u)
        .cubic_to((6.6 * u, 15.4 * u), (9.0 * u, 14.4 * u), 12.0 * u, 14.4 * u)
        .cubic_to(
            (15.0 * u, 14.4 * u),
            (17.4 * u, 15.4 * u),
            18.4 * u,
            18.4 * u,
        )
        .cubic_to(
            (16.8 * u, 20.0 * u),
            (14.6 * u, 20.8 * u),
            12.0 * u,
            20.8 * u,
        )
        .cubic_to((9.4 * u, 20.8 * u), (7.2 * u, 20.0 * u), 5.6 * u, 18.4 * u)
        .close();
    g.fill(s, &p, ink);
}

/// `hole`'s shapes added to `p` the other way round: cut out of what they
/// lie on.
fn reverse_into(p: &mut Path, hole: &Path) {
    p.append_reversed(hole);
}

/// Dark (ADR-0139): a crescent moon, the part of one circle another
/// leaves.
pub fn moon(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let u = g.u;
    // The moon round (12, 12), the shadow's circle up and to its right.
    let (ax, ay, ra) = (12.0f32, 12.5f32, 8.5f32);
    let (bx, by, rb) = (16.5f32, 8.0f32, 7.5f32);
    let (dx, dy) = (bx - ax, by - ay);
    let d = libm::sqrtf(dx * dx + dy * dy);
    let toward = libm::atan2f(dy, dx);
    // Where the circles cross, seen from each centre.
    let half_a = libm::acosf(((ra * ra - rb * rb + d * d) / (2.0 * d * ra)).clamp(-1.0, 1.0));
    let half_b = libm::acosf(((rb * rb - ra * ra + d * d) / (2.0 * d * rb)).clamp(-1.0, 1.0));
    let away = toward + PI;
    let mut p = Path::new();
    // Round the moon, the long way, from one crossing to the other ...
    p.arc(
        ax * u,
        ay * u,
        ra * u,
        toward + half_a,
        toward + 2.0 * PI - half_a,
    );
    // ... and back along the shadow's edge, inside it.
    p.arc(bx * u, by * u, rb * u, away + half_b, away - half_b);
    p.close();
    g.fill(s, &p, ink);
}

/// Lock (ADR-0139): a padlock, its shackle over its body, a keyhole cut
/// out.
pub fn lock(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let mut p = Path::new();
    g.band(&mut p, (12.0, 10.5), 3.0, 5.2, PI, 2.0 * PI);
    g.round_rect(&mut p, 6.8, 10.0, 2.2, 3.0, 0.4);
    g.round_rect(&mut p, 15.0, 10.0, 2.2, 3.0, 0.4);
    g.round_rect(&mut p, 4.5, 11.5, 15.0, 10.5, 2.6);
    let mut hole = Path::new();
    let u = g.u;
    hole.circle(12.0 * u, 15.6 * u, 1.7 * u, false);
    hole.round_rect(11.2 * u, 16.0 * u, 1.6 * u, 3.4 * u, 0.6 * u);
    reverse_into(&mut p, &hole);
    g.fill(s, &p, ink);
}

/// Settings (ADR-0139): a gear of eight teeth, its middle cut out.
pub fn gear(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let p = crate::apps::gear(12.0, 12.0, 10.0, 7.6, 3.2, 8, g.u);
    g.fill(s, &p, ink);
}

/// Play: a triangle pointing right, its corners softened.
pub fn play(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let u = g.u;
    let mut p = Path::new();
    p.move_to(7.0 * u, 5.5 * u)
        .quad_to(7.0 * u, 3.8 * u, 8.6 * u, 4.7 * u)
        .line_to(19.0 * u, 10.8 * u)
        .quad_to(20.4 * u, 12.0 * u, 19.0 * u, 13.2 * u)
        .line_to(8.6 * u, 19.3 * u)
        .quad_to(7.0 * u, 20.2 * u, 7.0 * u, 18.5 * u)
        .close();
    g.fill(s, &p, ink);
}

/// Pause: two bars.
pub fn pause(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let mut p = Path::new();
    g.round_rect(&mut p, 6.0, 4.5, 4.4, 15.0, 1.4);
    g.round_rect(&mut p, 13.6, 4.5, 4.4, 15.0, 1.4);
    g.fill(s, &p, ink);
}

/// Next (`forward`) or Previous: two triangles and a bar, as players
/// show them.
pub fn skip(s: &mut Surface<'_>, r: Rect, forward: bool, ink: Rgb) {
    let g = Grid::new(r);
    let u = g.u;
    // Drawn pointing right; Previous is its mirror image.
    let x = |v: f32| if forward { v * u } else { (24.0 - v) * u };
    let mut p = Path::new();
    for start in [3.0f32, 10.5] {
        p.move_to(x(start), 6.0 * u)
            .line_to(x(start + 8.5), 12.0 * u)
            .line_to(x(start), 18.0 * u)
            .close();
    }
    let (left, right) = if forward { (19.0, 21.4) } else { (2.6, 5.0) };
    g.round_rect(&mut p, left, 6.0, right - left, 12.0, 0.8);
    g.fill(s, &p, ink);
}

/// Back: a chevron pointing left.
pub fn back(s: &mut Surface<'_>, r: Rect, ink: Rgb) {
    let g = Grid::new(r);
    let mut p = Path::new();
    g.stroke(&mut p, (15.0, 5.0), (8.0, 12.0), 2.6);
    g.stroke(&mut p, (8.0, 12.0), (15.0, 19.0), 2.6);
    g.fill(s, &p, ink);
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use super::*;
    use crate::Scale;

    const WHITE: Rgb = Rgb(0xff_ff_ff);
    const GREY: Rgb = Rgb(0x80_80_80);

    fn drawn(scale: u16, draw: impl Fn(&mut Surface<'_>)) -> (usize, usize) {
        let size = 24 * usize::from(scale) / 100 + 4;
        let mut pixels = vec![0u32; size * size];
        let mut s = Surface::scaled(&mut pixels, size, size, Scale::new(scale).unwrap()).unwrap();
        draw(&mut s);
        let lit = s.pixels().iter().filter(|&&p| p != 0).count();
        let full = s.pixels().iter().filter(|&&p| p & 0xff >= 0xe0).count();
        (lit, full)
    }

    #[test]
    fn icons_draw_and_grow_with_the_scale() {
        type Draw = fn(&mut Surface<'_>, Rect);
        let icons: [(&str, Draw); 8] = [
            ("wifi", |s, r| wifi(s, r, 3, WHITE, GREY)),
            ("wifi off", |s, r| wifi_off(s, r, WHITE, GREY)),
            ("ethernet", |s, r| ethernet(s, r, true, WHITE)),
            ("speaker", |s, r| speaker(s, r, 80, false, WHITE)),
            ("muted", |s, r| speaker(s, r, 80, true, WHITE)),
            ("bell", |s, r| bell(s, r, WHITE)),
            ("controls", |s, r| controls(s, r, WHITE)),
            ("user", |s, r| user(s, r, WHITE)),
        ];
        for (name, draw) in icons {
            let at = |s: &mut Surface<'_>| draw(s, Rect::new(1, 1, 24, 24));
            let (lit1, full1) = drawn(100, at);
            let (lit2, full2) = drawn(200, at);
            assert!(full1 > 20, "{name}: {full1} solid pixels at 100 %");
            // Four times the pixels at twice the scale, near enough.
            assert!(
                lit2 > lit1 * 3 && lit2 < lit1 * 5,
                "{name}: {lit1} → {lit2}"
            );
            assert!(full2 > full1 * 3, "{name}");
        }
    }

    #[test]
    fn wifi_lights_its_bars() {
        let lit = |bars| drawn(100, |s| wifi(s, Rect::new(1, 1, 24, 24), bars, WHITE, GREY)).1;
        assert!(lit(0) < lit(1) && lit(1) < lit(2) && lit(2) < lit(3));
        assert_eq!(wifi_bars(-40), 3);
        assert_eq!(wifi_bars(-70), 2);
        assert_eq!(wifi_bars(-90), 1);
    }

    #[test]
    fn holes_are_cut() {
        // The user's ring is hollow: its middle, below the head, is clear.
        let mut pixels = vec![0u32; 26 * 26];
        let mut s = Surface::new(&mut pixels, 26, 26).unwrap();
        user(&mut s, Rect::new(1, 1, 24, 24), WHITE);
        assert_eq!(s.pixel(1 + 5, 1 + 12), Some(Rgb(0)));
        assert_eq!(s.pixel(1 + 12, 1 + 9), Some(WHITE));
    }
}
#[cfg(test)]
mod more_tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn control_and_media_icons_draw() {
        type Draw = fn(&mut Surface<'_>, Rect);
        let all: [(&str, Draw); 7] = [
            ("moon", |s, r| moon(s, r, Rgb(0xffffff))),
            ("lock", |s, r| lock(s, r, Rgb(0xffffff))),
            ("gear", |s, r| gear(s, r, Rgb(0xffffff))),
            ("play", |s, r| play(s, r, Rgb(0xffffff))),
            ("pause", |s, r| pause(s, r, Rgb(0xffffff))),
            ("next", |s, r| skip(s, r, true, Rgb(0xffffff))),
            ("back", |s, r| back(s, r, Rgb(0xffffff))),
        ];
        for (name, draw) in all {
            let mut px = vec![0u32; 26 * 26];
            let mut s = Surface::new(&mut px, 26, 26).unwrap();
            draw(&mut s, Rect::new(1, 1, 24, 24));
            let lit = s.pixels().iter().filter(|&&p| p & 0xff >= 0xe0).count();
            assert!(lit > 25, "{name}: {lit}");
        }
    }
}
