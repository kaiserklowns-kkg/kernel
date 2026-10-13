//! The system apps' icons (ADR-0139), as macOS and Windows draw theirs:
//! a picture of what the app does on a rounded tile that catches the
//! light, from outlines on a 64-unit grid, sharp at any size and scale.
//!
//! Only the apps that come with the system have one, and only by their
//! verified id ([`for_app`]): another app named "Settings" cannot take the
//! system's gear. Other apps keep the tile with their initial the desktop
//! draws.

use core::f32::consts::PI;

use crate::{Path, Rect, Rgb, Surface};

/// The units of the grid an icon is drawn on.
const UNITS: f32 = 64.0;

/// A grid of [`UNITS`] laid on the square centred in `r`.
#[derive(Clone, Copy)]
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
            u: side as f32 / UNITS,
        }
    }

    fn p(&self, v: f32) -> f32 {
        v * self.u
    }

    fn fill(&self, s: &mut Surface<'_>, path: &Path, colour: Rgb) {
        s.fill_path(path, self.x, self.y, colour);
    }

    fn gradient(&self, s: &mut Surface<'_>, path: &Path, top: Rgb, bottom: Rgb) {
        s.fill_path_gradient(path, self.x, self.y, top, bottom);
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
        let mut p = Path::new();
        p.round_rect(self.p(x), self.p(y), self.p(w), self.p(h), self.p(r));
        p
    }

    fn circle(&self, cx: f32, cy: f32, r: f32) -> Path {
        let mut p = Path::new();
        p.circle(self.p(cx), self.p(cy), self.p(r), false);
        p
    }

    fn stroke(&self, points: &[(f32, f32)], width: f32) -> Path {
        let mut p = Path::new();
        for pair in points.windows(2) {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            p.stroke(
                self.p(x0),
                self.p(y0),
                self.p(x1),
                self.p(y1),
                self.p(width),
            );
        }
        p
    }

    fn polygon(&self, points: &[(f32, f32)]) -> Path {
        let mut p = Path::new();
        if let Some(&(x, y)) = points.first() {
            p.move_to(self.p(x), self.p(y));
            for &(x, y) in &points[1..] {
                p.line_to(self.p(x), self.p(y));
            }
            p.close();
        }
        p
    }

    /// The tile every icon stands on: a rounded square with the light on
    /// its top, and a faint edge.
    fn tile(&self, s: &mut Surface<'_>, top: Rgb, bottom: Rgb) {
        let edge = self.rect(4.0, 4.0, 56.0, 56.0, 13.0);
        self.fill(s, &edge, bottom.over(Rgb(0), 200));
        let face = self.rect(4.6, 4.6, 54.8, 54.8, 12.4);
        self.gradient(s, &face, top, bottom);
    }
}

/// A gear of `teeth` round (`cx`, `cy`): teeth out to `outer`, the rim at
/// `inner`, a hole of `hole` in the middle (in grid units).
pub(crate) fn gear(
    cx: f32,
    cy: f32,
    outer: f32,
    inner: f32,
    hole: f32,
    teeth: usize,
    u: f32,
) -> Path {
    let mut p = Path::new();
    let step = 2.0 * PI / teeth as f32;
    let at = |r: f32, a: f32| ((cx + r * libm::cosf(a)) * u, (cy + r * libm::sinf(a)) * u);
    for i in 0..teeth {
        let a = i as f32 * step - PI / 2.0;
        // The tooth's flanks taper a little, as a gear's do.
        let points = [
            at(inner, a - step * 0.30),
            at(outer, a - step * 0.20),
            at(outer, a + step * 0.20),
            at(inner, a + step * 0.30),
        ];
        for (j, &(x, y)) in points.iter().enumerate() {
            if i == 0 && j == 0 {
                p.move_to(x, y);
            } else {
                p.line_to(x, y);
            }
        }
        // The rim to the next tooth.
        let (x, y) = at(inner, a + step * 0.70);
        p.line_to(x, y);
    }
    p.close();
    p.circle(cx * u, cy * u, hole * u, true);
    p
}

/// The system app an id names, if it is one with an icon of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    Terminal,
    Settings,
    Files,
    Calculator,
    TextEditor,
    ImageViewer,
    Music,
    ActivityMonitor,
}

/// The ids of the apps that come with the system (ADR-0080).
const SYSTEM: [(&str, App); 7] = [
    ("app.oceans.settings", App::Settings),
    ("app.oceans.files", App::Files),
    ("app.oceans.calculator", App::Calculator),
    ("app.oceans.editor", App::TextEditor),
    ("app.oceans.viewer", App::ImageViewer),
    ("app.oceans.music", App::Music),
    ("app.oceans.activity", App::ActivityMonitor),
];

/// The system app with the verified id `id`, if any.
pub fn for_app(id: &str) -> Option<App> {
    SYSTEM
        .iter()
        .find(|(known, _)| *known == id)
        .map(|&(_, app)| app)
}

const WHITE: Rgb = Rgb(0xff_ff_ff);

/// Draws `app`'s icon in `r` (its shorter side square).
pub fn draw(s: &mut Surface<'_>, r: Rect, app: App) {
    let g = Grid::new(r);
    match app {
        App::Terminal => {
            g.tile(s, Rgb(0x48_48_4a), Rgb(0x1c_1c_1e));
            // The prompt: a chevron and the cursor's bar, in green.
            let green = Rgb(0x32_d7_4b);
            g.fill(
                s,
                &g.stroke(&[(18.0, 22.0), (28.0, 31.0), (18.0, 40.0)], 4.6),
                green,
            );
            g.fill(s, &g.rect(32.0, 38.0, 15.0, 4.6, 2.3), green);
        }
        App::Settings => {
            g.tile(s, Rgb(0x9a_9a_a0), Rgb(0x5a_5a_60));
            // A gear in a gear, as macOS's System Settings has it.
            let outer = gear(32.0, 32.0, 23.0, 18.5, 0.0, 12, g.u);
            g.gradient(s, &outer, Rgb(0xf2_f2_f7), Rgb(0xc7_c7_cc));
            g.fill(s, &g.circle(32.0, 32.0, 14.5), Rgb(0x6e_6e_73));
            let inner = gear(32.0, 32.0, 12.5, 9.5, 4.5, 8, g.u);
            g.gradient(s, &inner, Rgb(0xe5_e5_ea), Rgb(0xae_ae_b2));
        }
        App::Files => {
            g.tile(s, Rgb(0x6a_c8_ff), Rgb(0x1a_7c_f0));
            // A folder: its tab, its back, and the front catching the light.
            g.fill(s, &g.rect(13.0, 18.0, 16.0, 9.0, 3.0), Rgb(0xd6_ec_ff));
            g.fill(s, &g.rect(13.0, 22.0, 38.0, 26.0, 4.0), Rgb(0xd6_ec_ff));
            g.gradient(
                s,
                &g.rect(13.0, 27.0, 38.0, 21.0, 4.0),
                WHITE,
                Rgb(0xe4_f2_ff),
            );
        }
        App::Calculator => {
            g.tile(s, Rgb(0x58_58_5e), Rgb(0x2c_2c_2e));
            // Its keys: grey numbers, the operators down the right in
            // orange, as the calculator people know.
            for row in 0..4 {
                for col in 0..4 {
                    let (x, y) = (13.0 + col as f32 * 10.0, 15.0 + row as f32 * 10.0);
                    let colour = match (row, col) {
                        (_, 3) => Rgb(0xff_9f_0a),
                        (0, _) => Rgb(0xa5_a5_aa),
                        _ => Rgb(0x6e_6e_73),
                    };
                    g.fill(s, &g.circle(x + 4.0, y + 4.0, 4.0), colour);
                }
            }
        }
        App::TextEditor => {
            g.tile(s, Rgb(0xff_ff_ff), Rgb(0xe0_e0_e6));
            // A page of lines and a pencil across it.
            let line = Rgb(0xb0_b0_b8);
            for i in 0..5 {
                let w = if i == 4 { 18.0 } else { 30.0 };
                g.fill(s, &g.rect(14.0, 16.0 + i as f32 * 7.0, w, 3.0, 1.5), line);
            }
            let pencil = g.stroke(&[(29.0, 49.0), (49.0, 29.0)], 7.0);
            g.fill(s, &pencil, Rgb(0xff_b3_00));
            g.fill(
                s,
                &g.stroke(&[(46.0, 32.0), (49.0, 29.0)], 7.0),
                Rgb(0xff_6b_6b),
            );
            g.fill(
                s,
                &g.polygon(&[(24.0, 54.0), (26.0, 45.0), (33.0, 52.0)]),
                Rgb(0x3a_3a_3c),
            );
        }
        App::ImageViewer => {
            g.tile(s, Rgb(0x64_d2_ff), Rgb(0x0a_84_ff));
            // A photograph: its white border, a sky, the sun, two hills.
            g.fill(s, &g.rect(11.0, 15.0, 42.0, 34.0, 4.0), WHITE);
            g.gradient(
                s,
                &g.rect(14.0, 18.0, 36.0, 28.0, 2.0),
                Rgb(0xbf_e6_ff),
                Rgb(0x8e_cf_ff),
            );
            g.fill(s, &g.circle(40.0, 26.0, 4.0), Rgb(0xff_cc_00));
            g.fill(
                s,
                &g.polygon(&[(14.0, 46.0), (26.0, 30.0), (38.0, 46.0)]),
                Rgb(0x34_c7_59),
            );
            g.fill(
                s,
                &g.polygon(&[(28.0, 46.0), (38.0, 35.0), (50.0, 46.0)]),
                Rgb(0x24_8a_3d),
            );
        }
        App::Music => {
            g.tile(s, Rgb(0xff_6b_8a), Rgb(0xfa_23_3b));
            // Two notes beamed together.
            g.fill(
                s,
                &g.polygon(&[(25.0, 20.0), (47.0, 15.0), (47.0, 21.0), (25.0, 26.0)]),
                WHITE,
            );
            g.fill(s, &g.rect(25.0, 22.0, 3.6, 22.0, 1.0), WHITE);
            g.fill(s, &g.rect(43.4, 17.0, 3.6, 22.0, 1.0), WHITE);
            g.fill(s, &g.circle(21.5, 44.0, 6.0), WHITE);
            g.fill(s, &g.circle(40.0, 39.5, 6.0), WHITE);
        }
        App::ActivityMonitor => {
            g.tile(s, Rgb(0x48_48_4a), Rgb(0x1c_1c_1e));
            // A graph of the machine's load: its grid and a green line.
            let grid = Rgb(0x48_48_4a);
            for i in 0..3 {
                g.fill(
                    s,
                    &g.rect(12.0, 22.0 + i as f32 * 10.0, 40.0, 1.2, 0.6),
                    grid,
                );
            }
            let line = [
                (12.0, 42.0),
                (19.0, 36.0),
                (25.0, 40.0),
                (31.0, 24.0),
                (38.0, 33.0),
                (44.0, 28.0),
                (52.0, 30.0),
            ];
            g.fill(s, &g.stroke(&line, 3.4), Rgb(0x30_d1_58));
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use super::*;
    use crate::Scale;

    #[test]
    fn only_the_systems_verified_ids_have_icons() {
        assert_eq!(for_app("app.oceans.settings"), Some(App::Settings));
        assert_eq!(for_app("app.oceans.music"), Some(App::Music));
        assert_eq!(for_app("app.example.settings"), None);
        assert_eq!(for_app("Settings"), None);
        assert_eq!(for_app(""), None);
    }

    #[test]
    fn every_icon_fills_its_tile_at_any_scale() {
        let apps = [
            App::Terminal,
            App::Settings,
            App::Files,
            App::Calculator,
            App::TextEditor,
            App::ImageViewer,
            App::Music,
            App::ActivityMonitor,
        ];
        for percent in [100u16, 200] {
            let side = 48 * usize::from(percent) / 100;
            for app in apps {
                let mut pixels = vec![0u32; side * side];
                let scale = Scale::new(percent).unwrap();
                let mut s = Surface::scaled(&mut pixels, side, side, scale).unwrap();
                draw(&mut s, Rect::new(0, 0, 48, 48), app);
                // The tile covers most of the square; its corners are clear.
                let lit = s.pixels().iter().filter(|&&p| p != 0).count();
                assert!(lit * 100 > side * side * 70, "{app:?} at {percent}: {lit}");
                assert_eq!(s.pixel(0, 0), Some(Rgb(0)), "{app:?}");
                // More than one colour: a picture, not a plain tile.
                let middle = s.pixel(side as i32 / 2, side as i32 / 2);
                let edge = s.pixel(side as i32 / 2, side as i32 / 12);
                assert_ne!(middle, edge, "{app:?} at {percent}");
            }
        }
    }
}
