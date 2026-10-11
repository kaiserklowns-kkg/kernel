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
    ("Orange", Rgb(0xf5_8f_3b)),
    ("Green", Rgb(0x2f_a8_4f)),
    ("Graphite", Rgb(0x7d_7d_85)),
];

/// The colours of one appearance with one accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub appearance: Appearance,
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
        let accent = ACCENTS.get(accent).map_or(ACCENTS[0].1, |&(_, rgb)| rgb);
        let white = Rgb(0xff_ff_ff);
        match appearance {
            Appearance::Light => {
                let surface = white;
                // The blue the desktop always had keeps its exact shades.
                let blue = accent == ACCENTS[0].1;
                Self {
                    appearance,
                    window: Rgb(0xf6_f6_f8),
                    sidebar: Rgb(0xe9_e9_ee),
                    surface,
                    text: Rgb(0x1d_1d_1f),
                    muted: Rgb(0x6e_6e_73),
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

/// This process's theme, from now on.
pub fn set(appearance: Appearance, accent: u8) {
    APPEARANCE.store(appearance.byte(), Ordering::Relaxed);
    ACCENT.store(accent, Ordering::Relaxed);
}

/// The theme this process draws with: light and blue until [`set`].
pub fn current() -> Theme {
    Theme::new(
        Appearance::from_byte(APPEARANCE.load(Ordering::Relaxed)),
        usize::from(ACCENT.load(Ordering::Relaxed)),
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
        set(Appearance::Light, 0);
    }
}
