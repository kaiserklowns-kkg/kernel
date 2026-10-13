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

#[test]
fn scaled_surfaces_draw_logical_pixels_at_their_resolution() {
    let s2 = Scale::new(200).unwrap();
    let mut pixels = vec![0u32; 40 * 20];
    let mut s = Surface::scaled(&mut pixels, 40, 20, s2).unwrap();
    // Laid out in logical pixels: half the buffer's.
    assert_eq!((s.width, s.height), (20, 10));
    s.fill(Rect::new(1, 1, 2, 2), WHITE);
    assert_eq!(s.pixel(1, 1), Some(Rgb(0)));
    assert_eq!(s.pixel(2, 2), Some(WHITE));
    assert_eq!(s.pixel(5, 5), Some(WHITE));
    assert_eq!(s.pixel(6, 6), Some(Rgb(0)));
    assert_eq!(s.device(Rect::new(1, 1, 2, 2)), Rect::new(2, 2, 4, 4));

    // At 125 %, a window of 761 logical pixels is 951 and back.
    let s125 = Scale::new(125).unwrap();
    let mut pixels = vec![0u32; 951];
    let s = Surface::scaled(&mut pixels, 951, 1, s125).unwrap();
    assert_eq!(s.width, 761);
}

#[test]
fn text_keeps_its_logical_width_at_any_scale() {
    let mut typesetter = Typesetter::new().unwrap();
    let at_one = typesetter.measure("Oceans ทะเล", Style::Body);
    for percent in Scale::CHOICES {
        typesetter.set_scale(Scale::new(percent).unwrap());
        let width = typesetter.measure("Oceans ทะเล", Style::Body);
        assert!(
            (width - at_one).abs() <= 3,
            "{percent}: {width} vs {at_one}"
        );
    }
    // The glyphs themselves are larger: drawn at the scale.
    typesetter.set_scale(Scale::new(200).unwrap());
    let big = typesetter.glyph(Style::Body, 'O').height;
    typesetter.set_scale(Scale::ONE);
    let small = typesetter.glyph(Style::Body, 'O').height;
    assert!(big >= small * 2 - 1, "{big} {small}");

    // Drawn text ends where it was measured to.
    let s2 = Scale::new(150).unwrap();
    let mut pixels = vec![0u32; 300 * 60];
    let mut s = Surface::scaled(&mut pixels, 300, 60, s2).unwrap();
    let clip = Rect::new(0, 0, 200, 40);
    let end = s.text(&mut typesetter, (10, 5), "Oceans", Style::Body, WHITE, clip);
    assert_eq!(end - 10, typesetter.measure("Oceans", Style::Body));
    assert_eq!(typesetter.scale(), s2);
}

#[test]
fn paths_fill_and_cut_holes() {
    let mut pixels = vec![0u32; 30 * 30];
    let mut s = Surface::new(&mut pixels, 30, 30).unwrap();
    let mut ring = Path::new();
    ring.circle(15.0, 15.0, 12.0, false)
        .circle(15.0, 15.0, 6.0, true);
    s.fill_path(&ring, 0, 0, WHITE);
    assert_eq!(s.pixel(15, 15), Some(Rgb(0)));
    assert_eq!(s.pixel(15, 6), Some(WHITE));
    assert_eq!(s.pixel(0, 0), Some(Rgb(0)));
    // Off the surface: nothing, and no panic.
    s.fill_path(&ring, 100, -100, WHITE);
}

#[test]
fn drawing_keeps_to_the_clip() {
    let mut pixels = vec![0u32; 40 * 40];
    let mut s = Surface::scaled(&mut pixels, 40, 40, Scale::new(200).unwrap()).unwrap();
    s.clip_to(Some(Rect::new(5, 5, 10, 10)));
    assert_eq!(s.clip_area(), Some(Rect::new(5, 5, 10, 10)));
    s.fill(Rect::new(0, 0, 20, 20), WHITE);
    s.circle(2, 2, 3, WHITE);
    // Inside the clip (device 10..30), drawn; outside, not.
    assert_eq!(s.pixel(10, 10), Some(WHITE));
    assert_eq!(s.pixel(29, 29), Some(WHITE));
    assert_eq!(s.pixel(9, 10), Some(Rgb(0)));
    assert_eq!(s.pixel(30, 30), Some(Rgb(0)));
    assert_eq!(s.pixel(2, 2), Some(Rgb(0)));
    // Back to the whole surface.
    s.clip_to(None);
    s.fill(Rect::new(0, 0, 1, 1), WHITE);
    assert_eq!(s.pixel(0, 0), Some(WHITE));
}
