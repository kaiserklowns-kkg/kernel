extern crate std;

use std::vec;

use super::*;

const WHITE: Rgb = Rgb(0xff_ff_ff);
const BLUE: Rgb = Rgb(0x2f_7c_f6);

#[test]
fn surfaces_check_their_size_and_clip_everything() {
    let mut pixels = vec![0u32; 20 * 10];
    assert!(Surface::new(&mut pixels, 21, 10).is_none());
    let mut s = Surface::new(&mut pixels, 20, 10).unwrap();
    // Partly and wholly outside: no panic, only the inside is drawn.
    s.fill(Rect::new(-5, -5, 10, 10), WHITE);
    s.fill(Rect::new(100, 100, 10, 10), WHITE);
    s.circle(19, 9, 6, WHITE);
    s.round_fill(Rect::new(15, 5, 50, 50), 8, WHITE);
    assert_eq!(s.pixel(0, 0), Some(WHITE));
    assert_eq!(s.pixel(5, 5), Some(Rgb(0)));
    assert_eq!(s.pixel(20, 0), None);
}

#[test]
fn rounded_rectangles_leave_their_corners() {
    let mut pixels = vec![0u32; 40 * 40];
    let mut s = Surface::new(&mut pixels, 40, 40).unwrap();
    s.round_fill(Rect::new(0, 0, 40, 40), 10, WHITE);
    // The corner pixel stays; the middle and the edges' middles are filled.
    assert_eq!(s.pixel(0, 0), Some(Rgb(0)));
    assert_eq!(s.pixel(39, 39), Some(Rgb(0)));
    assert_eq!(s.pixel(20, 20), Some(WHITE));
    assert_eq!(s.pixel(0, 20), Some(WHITE));
    assert_eq!(s.pixel(20, 0), Some(WHITE));
}

#[test]
fn tints_blend_over_what_is_there() {
    let mut pixels = vec![0u32; 4];
    let mut s = Surface::new(&mut pixels, 2, 2).unwrap();
    s.tint(Rect::new(0, 0, 2, 2), 0, WHITE, 255);
    assert_eq!(s.pixel(1, 1), Some(WHITE));
    s.tint(Rect::new(0, 0, 1, 1), 0, Rgb(0), 128);
    let Rgb(half) = s.pixel(0, 0).unwrap();
    assert!((0x7e..=0x80).contains(&(half & 0xff)), "{half:06x}");
    assert_eq!(BLUE.over(WHITE, 0), WHITE);
    assert_eq!(BLUE.over(WHITE, 255), BLUE);
}

#[test]
fn circles_are_round_and_soft_edged() {
    let mut pixels = vec![0u32; 21 * 21];
    let mut s = Surface::new(&mut pixels, 21, 21).unwrap();
    s.circle(10, 10, 6, WHITE);
    assert_eq!(s.pixel(10, 10), Some(WHITE));
    assert_eq!(s.pixel(10, 1), Some(Rgb(0)));
    // Symmetric.
    assert_eq!(s.pixel(4, 10), s.pixel(16, 10));
    assert_eq!(s.pixel(10, 4), s.pixel(10, 16));
}

#[test]
fn text_is_measured_drawn_and_thai_has_glyphs() {
    let mut typesetter = Typesetter::new().unwrap();
    let width = typesetter.measure("Oceans", Style::Body);
    assert!((30..80).contains(&width), "{width}");
    assert!(typesetter.measure("Oceans", Style::Title) > width);
    // Thai letters are drawn from the Thai font, not as `?`; a combining
    // vowel takes no room.
    let question = typesetter.glyph(Style::Body, '?').coverage.clone();
    assert_ne!(typesetter.glyph(Style::Body, 'ส').coverage, question);
    assert_eq!(typesetter.glyph(Style::Body, '\u{0e31}').advance, 0);
    // A character no font has falls back to `?`.
    assert_eq!(typesetter.glyph(Style::Body, '\u{4e2d}').coverage, question);

    let mut pixels = vec![0u32; 100 * 30];
    let mut s = Surface::new(&mut pixels, 100, 30).unwrap();
    let clip = Rect::new(0, 0, 100, 30);
    let end = s.text(&mut typesetter, (4, 4), "Hi", Style::Strong, WHITE, clip);
    assert!(end > 4);
    assert!(s.pixels().iter().any(|&p| p != 0));
    // Clipped away: nothing drawn.
    let mut pixels = vec![0u32; 100 * 30];
    let mut s = Surface::new(&mut pixels, 100, 30).unwrap();
    s.text(
        &mut typesetter,
        (4, 4),
        "Hi",
        Style::Strong,
        WHITE,
        Rect::new(0, 0, 0, 0),
    );
    assert!(s.pixels().iter().all(|&p| p == 0));
}
