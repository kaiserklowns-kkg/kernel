//! The design values (ADR-0113): every colour the desktop and apps draw
//! their surfaces, text and controls with, for the light and the dark
//! appearance, and the accent the user chose.
//!
//! Each process keeps the theme it draws with ([`set`], [`current`]): the
//! desktop sets its own from Core, and tells every window; `oceans-ui`
//! sets an app's when its window hears of it.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::Rgb;

/// Light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

impl Appearance {
    pub const fn from_byte(byte: u8) -> Self {
        if byte == 1 { Self::Dark } else { Self::Light }
    }

    pub const fn byte(self) -> u8 {
        match self {
            Self::Light => 0,
            Self::Dark => 1,
        }
    }
}

/// The accents to choose from (ADR-0113): blue first, the default.
pub const ACCENTS: [(&str, Rgb); 6] = [
    ("Blue", Rgb(0x2f_7c_f6)),
    ("Purple", Rgb(0x8e_5c_f7)),
    ("Pink", Rgb(0xe8_4a_7f)),
    ("Orange", Rgb(0xe0_6c_0a)),
    ("Green", Rgb(0x2f_a8_4f)),
    ("Graphite", Rgb(0x7d_7d_85)),
];

/// The accents with more contrast (ADR-0155): each dark enough that white
/// on it reaches 4.5:1 (WCAG AA), in the order of [`ACCENTS`].
pub const CONTRAST_ACCENTS: [Rgb; 6] = [
    Rgb(0x1a_5f_d0),
    Rgb(0x6e_3c_d6),
    Rgb(0xc0_2c_5e),
    Rgb(0xa8_52_0a),
    Rgb(0x1d_76_35),
    Rgb(0x5e_5e_66),
];

/// What the user chose for legibility (ADR-0155), beside light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Increase contrast: stronger text, lines and borders, deeper
    /// accents.
    pub contrast: bool,
    /// Reduce transparency: the menu bar and Wave solid, the wallpaper no
    /// longer through them.
    pub solid: bool,
}

impl Options {
    pub const fn from_byte(byte: u8) -> Self {
        Self {
            contrast: byte & 1 != 0,
            solid: byte & 2 != 0,
        }
    }

    pub const fn byte(self) -> u8 {
        (self.contrast as u8) | ((self.solid as u8) << 1)
    }
}

/// The colours of one appearance with one accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub appearance: Appearance,
    /// The legibility options these colours were made with.
    pub options: Options,
    /// A window's background.
    pub window: Rgb,
    /// Sidebars and toolbars.
    pub sidebar: Rgb,
    /// Fields, pages, cards: what holds content.
    pub surface: Rgb,
    pub text: Rgb,
    /// Secondary text.
    pub muted: Rgb,
    pub accent: Rgb,
    /// The accent pressed or pointed at.
    pub accent_pressed: Rgb,
    /// Text and marks on the accent.
    pub on_accent: Rgb,
    pub button: Rgb,
    pub button_hover: Rgb,
    /// A selected row: the accent, faint.
    pub selected: Rgb,
    /// Separators and field borders.
    pub line: Rgb,
    pub danger: Rgb,
    pub success: Rgb,
    /// The desktop's own: the apps panel, dialogs, title bars, window
    /// borders, the window buttons when off.
    pub panel: Rgb,
    pub dialog: Rgb,
    pub title_focused: Rgb,
    pub title_idle: Rgb,
    pub border: Rgb,
    pub button_off: Rgb,
}

impl Theme {
    pub fn new(appearance: Appearance, accent: usize) -> Self {
        Self::with(appearance, accent, Options::default())
    }

    /// The colours of `appearance` with accent number `accent`, made more
    /// legible as `options` asks (ADR-0155).
    pub fn with(appearance: Appearance, accent: usize, options: Options) -> Self {
        let mut theme = Self::base(appearance, accent);
        theme.options = options;
        if options.contrast {
            theme.raise_contrast(accent);
        }
        theme
    }

    /// Increase contrast (ADR-0155): text and secondary text toward full
    /// black or white, lines and borders that stand out (3:1 against the
    /// window), controls a step further from the window, and accents dark
    /// enough for white text on them (4.5:1).
    fn raise_contrast(&mut self, accent: usize) {
        let deep = CONTRAST_ACCENTS
            .get(accent)
            .copied()
            .unwrap_or(CONTRAST_ACCENTS[0]);
        self.accent = deep;
        self.accent_pressed = Rgb(0).over(deep, 40);
        match self.appearance {
            Appearance::Light => {
                self.text = Rgb(0x00_00_00);
                self.muted = Rgb(0x48_48_4d);
                self.line = Rgb(0x8a_8a_90);
                self.border = Rgb(0x6e_6e_73);
                self.button = Rgb(0xd1_d1_d6);
                self.button_hover = Rgb(0xbe_be_c4);
                self.title_focused = Rgb(0xd1_d1_d6);
                self.selected = deep.over(self.surface, 40);
                self.button_off = Rgb(0xa8_a8_ae);
                self.danger = Rgb(0xc4_1e_14);
                self.success = Rgb(0x1d_76_35);
            }
            Appearance::Dark => {
                self.text = Rgb(0xff_ff_ff);
                self.muted = Rgb(0xc7_c7_cc);
                self.line = Rgb(0x7c_7c_82);
                self.border = Rgb(0x8e_8e_93);
                self.button = Rgb(0x48_48_4a);
                self.button_hover = Rgb(0x58_58_5c);
                self.title_focused = Rgb(0x48_48_4a);
                self.selected = deep.over(self.surface, 150);
                self.button_off = Rgb(0x7c_7c_82);
            }
        }
    }

    fn base(appearance: Appearance, accent: usize) -> Self {
        let accent = ACCENTS.get(accent).map_or(ACCENTS[0].1, |&(_, rgb)| rgb);
        let white = Rgb(0xff_ff_ff);
        match appearance {
            Appearance::Light => {
                let surface = white;
                // The blue the desktop always had keeps its exact shades.
                let blue = accent == ACCENTS[0].1;
                Self {
                    appearance,
                    options: Options::default(),
                    window: Rgb(0xf6_f6_f8),
                    sidebar: Rgb(0xe9_e9_ee),
                    surface,
                    text: Rgb(0x1d_1d_1f),
                    muted: Rgb(0x66_66_6b),
                    accent,
                    accent_pressed: if blue {
                        Rgb(0x52_93_f8)
                    } else {
                        white.over(accent, 40)
                    },
                    on_accent: white,
                    button: Rgb(0xe3_e3_e8),
                    button_hover: Rgb(0xd5_d5_db),
                    selected: if blue {
                        Rgb(0xd4_e3_fc)
                    } else {
                        accent.over(surface, 50)
                    },
                    line: Rgb(0xd8_d8_de),
                    danger: Rgb(0xff_3b_30),
                    success: Rgb(0x34_c7_59),
                    panel: Rgb(0xf2_f2_f7),
                    dialog: Rgb(0xf9_f9_fb),
                    title_focused: Rgb(0xe3_e3_e8),
                    title_idle: Rgb(0xf6_f6_f8),
                    border: Rgb(0xb4_b4_bc),
                    button_off: Rgb(0xd1_d1_d6),
                }
            }
            Appearance::Dark => {
                let surface = Rgb(0x2c_2c_2e);
                Self {
                    appearance,
                    options: Options::default(),
                    window: Rgb(0x1e_1e_20),
                    sidebar: Rgb(0x26_26_29),
                    surface,
                    text: Rgb(0xf2_f2_f7),
                    muted: Rgb(0x98_98_9f),
                    accent,
                    accent_pressed: white.over(accent, 50),
                    on_accent: white,
                    button: Rgb(0x3a_3a_3c),
                    button_hover: Rgb(0x48_48_4a),
                    selected: accent.over(surface, 90),
                    line: Rgb(0x3d_3d_40),
                    danger: Rgb(0xff_45_3a),
                    success: Rgb(0x32_d7_4b),
                    panel: Rgb(0x26_26_29),
                    dialog: Rgb(0x2c_2c_2e),
                    title_focused: Rgb(0x3a_3a_3c),
                    title_idle: Rgb(0x2a_2a_2c),
                    border: Rgb(0x50_50_55),
                    button_off: Rgb(0x55_55_5a),
                }
            }
        }
    }
}

static APPEARANCE: AtomicU8 = AtomicU8::new(0);
static ACCENT: AtomicU8 = AtomicU8::new(0);
static OPTIONS: AtomicU8 = AtomicU8::new(0);

/// This process's theme, from now on.
pub fn set(appearance: Appearance, accent: u8) {
    APPEARANCE.store(appearance.byte(), Ordering::Relaxed);
    ACCENT.store(accent, Ordering::Relaxed);
}

/// This process's legibility options (ADR-0155), from now on.
pub fn set_options(options: Options) {
    OPTIONS.store(options.byte(), Ordering::Relaxed);
}

/// The theme this process draws with: light and blue, without options,
/// until [`set`] and [`set_options`].
pub fn current() -> Theme {
    Theme::with(
        Appearance::from_byte(APPEARANCE.load(Ordering::Relaxed)),
        usize::from(ACCENT.load(Ordering::Relaxed)),
        Options::from_byte(OPTIONS.load(Ordering::Relaxed)),
    )
}

/// The appearance and accent set, as bytes (for the window protocol and
/// Core).
pub fn bytes() -> (u8, u8) {
    (
        APPEARANCE.load(Ordering::Relaxed),
        ACCENT.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_blue_keeps_the_desktops_shades() {
        let t = Theme::new(Appearance::Light, 0);
        assert_eq!(t.accent, Rgb(0x2f_7c_f6));
        assert_eq!(t.selected, Rgb(0xd4_e3_fc));
        assert_eq!(t.window, Rgb(0xf6_f6_f8));
        assert_eq!(t.panel, Rgb(0xf2_f2_f7));
    }

    #[test]
    fn dark_is_dark_and_accents_apply() {
        let t = Theme::new(Appearance::Dark, 3);
        assert_eq!(t.accent, ACCENTS[3].1);
        // Light text on dark surfaces.
        let lum = |c: Rgb| (c.0 >> 16 & 0xff) + (c.0 >> 8 & 0xff) + (c.0 & 0xff);
        assert!(lum(t.text) > 600 && lum(t.window) < 150 && lum(t.surface) < 150);
        // An accent out of range is blue.
        assert_eq!(Theme::new(Appearance::Light, 99).accent, ACCENTS[0].1);
        assert_eq!(Appearance::from_byte(1), Appearance::Dark);
        assert_eq!(Appearance::from_byte(7), Appearance::Light);
    }

    #[test]
    fn the_process_theme_follows_set() {
        assert_eq!(current().appearance, Appearance::Light);
        set(Appearance::Dark, 2);
        assert_eq!(
            (current().appearance, current().accent),
            (Appearance::Dark, ACCENTS[2].1)
        );
        assert_eq!(bytes(), (1, 2));
        set_options(Options {
            contrast: true,
            solid: true,
        });
        assert_eq!(current().accent, CONTRAST_ACCENTS[2]);
        assert!(current().options.solid);
        set_options(Options::default());
        set(Appearance::Light, 0);
    }

    #[test]
    fn options_round_trip_as_a_byte() {
        for byte in 0..4 {
            assert_eq!(Options::from_byte(byte).byte(), byte);
        }
        // Bits a later Oceans may add are ignored here.
        assert_eq!(Options::from_byte(0xfc), Options::default());
    }

    #[test]
    fn without_options_the_colours_are_unchanged() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            for accent in 0..ACCENTS.len() {
                let mut plain = Theme::with(appearance, accent, Options::default());
                assert_eq!(plain, Theme::new(appearance, accent));
                // Reduce transparency is the desktop's to apply; the
                // colours stay.
                let solid = Theme::with(
                    appearance,
                    accent,
                    Options {
                        contrast: false,
                        solid: true,
                    },
                );
                plain.options.solid = true;
                assert_eq!(solid, plain);
            }
        }
    }

    extern crate std;

    /// WCAG 2.x relative luminance.
    fn luminance(c: Rgb) -> f64 {
        let linear = |v: u32| {
            let s = f64::from(v) / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (c.0 >> 16 & 0xff, c.0 >> 8 & 0xff, c.0 & 0xff);
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    /// WCAG 2.x contrast ratio, 1 to 21.
    fn ratio(a: Rgb, b: Rgb) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// ADR-0155: every pair of text and what it is drawn on, in every
    /// appearance, accent and option, reaches WCAG AA. A value changed
    /// below it fails here.
    #[test]
    fn text_is_legible_everywhere() {
        let mut failures = std::vec::Vec::new();
        for appearance in [Appearance::Light, Appearance::Dark] {
            for byte in 0..4 {
                let options = Options::from_byte(byte);
                for accent in 0..ACCENTS.len() {
                    let t = Theme::with(appearance, accent, options);
                    let mut need = |what: &str, fg: Rgb, bg: Rgb, min: f64| {
                        let got = ratio(fg, bg);
                        if got < min {
                            failures.push(std::format!(
                                "{appearance:?} {options:?} accent {accent}: {what} \
                                 {fg:?} on {bg:?} is {got:.2}, needs {min}"
                            ));
                        }
                    };
                    // Body text: 4.5:1 on everything that holds it.
                    for (name, bg) in [
                        ("window", t.window),
                        ("sidebar", t.sidebar),
                        ("surface", t.surface),
                        ("button", t.button),
                        ("button_hover", t.button_hover),
                        ("selected", t.selected),
                        ("panel", t.panel),
                        ("dialog", t.dialog),
                        ("title_focused", t.title_focused),
                        ("title_idle", t.title_idle),
                    ] {
                        need(&std::format!("text on {name}"), t.text, bg, 4.5);
                    }
                    // Secondary text: 4.5:1 where it is used.
                    for (name, bg) in [
                        ("window", t.window),
                        ("sidebar", t.sidebar),
                        ("surface", t.surface),
                        ("panel", t.panel),
                        ("dialog", t.dialog),
                    ] {
                        need(&std::format!("muted on {name}"), t.muted, bg, 4.5);
                    }
                    // Labels on the accent are bold: 3:1 (WCAG's large
                    // text); 4.5:1 with more contrast.
                    let on_accent = if options.contrast { 4.5 } else { 3.0 };
                    need("on_accent on accent", t.on_accent, t.accent, on_accent);
                    // With more contrast, lines and borders are seen
                    // (WCAG 1.4.11: 3:1 for parts of controls).
                    if options.contrast {
                        for (name, bg) in [("window", t.window), ("surface", t.surface)] {
                            need(&std::format!("border on {name}"), t.border, bg, 3.0);
                            need(&std::format!("line on {name}"), t.line, bg, 3.0);
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
