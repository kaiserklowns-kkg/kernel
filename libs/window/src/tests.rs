extern crate std;

use std::vec::Vec;

use super::proto::{Event, OpenRequest};
use super::*;

const AREA: Rect = Rect::new(292, 52, 972, 732);
const BUDGET: usize = 2 * 1280 * 800;
const APP: u64 = 7;
const OTHER: u64 = 8;

fn manager() -> Manager {
    Manager::new(AREA, BUDGET)
}

fn kinds(manager: &mut Manager, owner: u64) -> Vec<(u8, u32)> {
    manager
        .take_events(owner, 100)
        .iter()
        .map(|e| (e.kind, e.window))
        .collect()
}

#[test]
fn events_round_trip_and_unknown_kinds_are_refused() {
    let event = Event {
        window: 7,
        kind: kind::BUTTON,
        key: 0,
        button: 1,
        pressed: true,
        x: -3,
        y: 300,
    };
    assert_eq!(Event::decode(&event.encode()), Some(event));
    assert_eq!(Event::decode(&event.encode()[..11]), None);
    let mut bad = event.encode();
    bad[4] = 0;
    assert_eq!(Event::decode(&bad), None);
    bad[4] = 6;
    assert_eq!(Event::decode(&bad), None);
}

#[test]
fn open_requests_are_bounded() {
    let request = OpenRequest {
        bits: 1,
        width: 480,
        height: 240,
        title: "Notes",
    };
    let (data, len) = request.encode().unwrap();
    assert_eq!(OpenRequest::decode(&data[..len]), Some(request));
    for (width, height, title, bits) in [
        (proto::MAX_WIDTH + 1, 240, "x", 1),
        (480, proto::MIN_HEIGHT - 1, "x", 1),
        (480, 240, "line\nbreak", 1),
        (480, 240, "x", 0),
    ] {
        let (data, len) = OpenRequest {
            bits,
            width,
            height,
            title,
        }
        .encode()
        .unwrap();
        assert_eq!(OpenRequest::decode(&data[..len]), None);
    }
    assert_eq!(OpenRequest::decode(&data[..11]), None);
    let mut bad_utf8 = data;
    bad_utf8[12] = 0xff;
    assert_eq!(OpenRequest::decode(&bad_utf8[..len]), None);
    let long = OpenRequest {
        title: "a title much longer than forty-eight bytes allows",
        ..request
    };
    assert_eq!(long.encode(), None);
}

#[test]
fn a_new_window_is_placed_in_the_area_on_top_with_the_focus() {
    let mut m = manager();
    let first = m.open(APP, "Notes", "notes", 480, 240).unwrap();
    let frame = &m.frames()[0];
    // In the middle of the area (ADR-0076).
    let (w, h) = (480 + 2, 240 + TITLE_HEIGHT + 1);
    let (x, y) = (AREA.x + (AREA.w - w) / 2, AREA.y + (AREA.h - h) / 2);
    assert_eq!((frame.x, frame.y), (x, y));
    assert_eq!(
        frame.content(),
        Rect::new(x + 1, y + TITLE_HEIGHT, 480, 240)
    );
    assert_eq!(m.focus(), Focus::Window(first));
    assert_eq!(kinds(&mut m, APP), [(kind::FOCUS, first)]);
    assert_eq!(m.take_signals().into_iter().collect::<Vec<_>>(), [APP]);

    let second = m.open(OTHER, "Other", "", 200, 100).unwrap();
    // The next one, a step further.
    assert_eq!(m.frames()[1].x, AREA.x + (AREA.w - 202) / 2 + 32);
    assert_eq!(m.focus(), Focus::Window(second));
    // The first app hears it lost the focus.
    let lost = m.take_events(APP, 10);
    assert_eq!(lost.len(), 1);
    assert_eq!((lost[0].kind, lost[0].pressed), (kind::FOCUS, false));
}

#[test]
fn windows_too_big_for_the_area_are_kept_inside_it() {
    let mut m = Manager::new(Rect::new(0, 0, 500, 300), BUDGET);
    m.open(APP, "Big", "", 1000, 700).unwrap();
    let frame = &m.frames()[0];
    assert_eq!((frame.x, frame.y), (0, 0));
}

#[test]
fn limits_per_app_on_the_screen_and_in_pixels() {
    let mut m = manager();
    for _ in 0..proto::MAX_WINDOWS_PER_APP {
        m.open(APP, "Notes", "", 64, 32).unwrap();
    }
    assert_eq!(m.open(APP, "Notes", "", 64, 32), Err(Status::TooMany));
    assert!(m.open(OTHER, "Other", "", 64, 32).is_ok());

    let mut small = Manager::new(AREA, 100 * 100);
    assert!(small.open(APP, "A", "", 100, 64).is_ok());
    assert_eq!(small.open(OTHER, "B", "", 100, 64), Err(Status::TooMany));

    let mut many = manager();
    for owner in 0..MAX_WINDOWS as u64 {
        many.open(100 + owner, "A", "", 64, 32).unwrap();
    }
    assert_eq!(many.open(1, "A", "", 64, 32), Err(Status::TooMany));
}

#[test]
fn apps_reach_only_their_own_windows() {
    let mut m = manager();
    let mine = m.open(APP, "Notes", "", 100, 100).unwrap();
    assert_eq!(m.present(OTHER, mine), Err(Status::NotFound));
    assert_eq!(m.close(OTHER, mine), Err(Status::NotFound));
    assert_eq!(m.present(APP, mine), Ok(()));
    assert!(m.frames()[0].presented);
    assert!(m.take_events(OTHER, 10).is_empty());
}

#[test]
fn keys_go_to_the_focused_window_or_the_terminal() {
    let mut m = manager();
    assert_eq!(m.key(b'a'), KeyRoute::Terminal(b'a'));
    let id = m.open(APP, "Notes", "", 100, 100).unwrap();
    m.take_events(APP, 10);
    assert_eq!(m.key(b'h'), KeyRoute::Window(APP));
    let events = m.take_events(APP, 10);
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].kind, events[0].key, events[0].window),
        (kind::KEY, b'h', id)
    );
}

#[test]
fn ctrl_tab_goes_round_the_terminal_and_the_windows_in_order() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 100, 100).unwrap();
    let b = m.open(OTHER, "B", "", 100, 100).unwrap();
    assert_eq!(m.focus(), Focus::Window(b));
    assert_eq!(m.key(proto::KEY_NEXT_WINDOW), KeyRoute::Consumed);
    assert_eq!(m.focus(), Focus::Terminal);
    m.key(proto::KEY_NEXT_WINDOW);
    assert_eq!(m.focus(), Focus::Window(a));
    // Focused means on top.
    assert_eq!(m.frames().last().unwrap().id, a);
    m.key(proto::KEY_NEXT_WINDOW);
    assert_eq!(m.focus(), Focus::Window(b));
    // No app ever sees the key.
    assert!(
        m.take_events(APP, 100)
            .iter()
            .chain(m.take_events(OTHER, 100).iter())
            .all(|e| e.kind == kind::FOCUS)
    );
}

#[test]
fn closing_moves_the_focus_to_the_window_below_then_the_terminal() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 100, 100).unwrap();
    let b = m.open(OTHER, "B", "", 100, 100).unwrap();
    m.close(OTHER, b).unwrap();
    assert_eq!(m.focus(), Focus::Window(a));
    assert_eq!(m.close_owner(APP), [a]);
    assert_eq!(m.focus(), Focus::Terminal);
    assert!(m.frames().is_empty());
    assert!(m.take_events(APP, 10).is_empty());
}

#[test]
fn clicks_focus_raise_drag_and_close() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 200, 100).unwrap();
    let b = m.open(OTHER, "B", "", 200, 100).unwrap();
    m.take_events(APP, 100);
    m.take_events(OTHER, 100);
    let (fa, fb) = (m.frames()[0].clone(), m.frames()[1].clone());

    // A click outside every window is the desktop's.
    assert!(!m.button(1, true, 5, 5));

    // On A's visible content (left of B): A is raised and focused, and gets
    // the click at its own coordinates.
    let (x, y) = (fa.content().x + 3, fa.content().y + 90);
    assert!(!fb.outer().contains(x, y));
    assert!(m.button(1, true, x, y));
    assert_eq!(m.focus(), Focus::Window(a));
    assert_eq!(m.frames().last().unwrap().id, a);
    let events = m.take_events(APP, 100);
    let click = events.iter().find(|e| e.kind == kind::BUTTON).unwrap();
    assert_eq!((click.x, click.y, click.pressed), (3, 90, true));

    // Dragging B by its title bar (raised again first: A covers part of it).
    assert_eq!(m.frames()[0].id, b);
    m.set_focus(Focus::Window(b));
    let title = m.frames()[1].title_bar();
    assert!(m.button(1, true, title.x + 40, title.y + 10));
    assert!(m.pointer_moved(title.x + 140, title.y + 60));
    let moved = m.frames().iter().find(|f| f.id == b).unwrap();
    assert_eq!((moved.x, moved.y), (fb.x + 100, fb.y + 50));
    assert!(m.button(1, false, 0, 0));
    assert!(!m.pointer_moved(0, 0));

    // The close button asks the app; the window stays until it closes it.
    let close = m.frames().last().unwrap().close_button();
    assert!(m.button(1, true, close.x + 2, close.y + 2));
    let events = m.take_events(OTHER, 100);
    assert!(
        events
            .iter()
            .any(|e| e.kind == kind::CLOSE && e.window == b)
    );
    assert_eq!(m.frames().len(), 2);
}

#[test]
fn dragging_keeps_the_title_bar_on_the_area() {
    let mut m = manager();
    m.open(APP, "A", "", 200, 100).unwrap();
    let title = m.frames()[0].title_bar();
    m.button(1, true, title.x + 10, title.y + 5);
    m.pointer_moved(-5000, -5000);
    let frame = &m.frames()[0];
    assert!(frame.y >= AREA.y);
    assert!(frame.x + frame.outer().w >= AREA.x + 48);
}

#[test]
fn only_the_focused_window_sees_the_pointer_and_motion_coalesces() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 200, 100).unwrap();
    let content = m.frames()[0].content();
    m.take_events(APP, 100);
    m.pointer_moved(content.x + 1, content.y + 1);
    m.pointer_moved(content.x + 5, content.y + 6);
    let events = m.take_events(APP, 100);
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].kind, events[0].x, events[0].y),
        (kind::POINTER, 5, 6)
    );
    m.set_focus(Focus::Terminal);
    m.take_events(APP, 100);
    m.pointer_moved(content.x + 7, content.y + 7);
    assert!(m.take_events(APP, 100).is_empty());
    // Over the window but not focused: a click focuses it first.
    m.button(1, true, content.x + 7, content.y + 7);
    assert_eq!(m.focus(), Focus::Window(a));
}

#[test]
fn queues_are_bounded() {
    let mut m = manager();
    m.open(APP, "A", "", 200, 100).unwrap();
    for _ in 0..MAX_QUEUED * 2 {
        m.key(b'x');
    }
    assert_eq!(m.take_events(APP, 1000).len(), MAX_QUEUED);
}

/// The same bytes as go/oceans/window's `TestWireFormatMatchesRust`.
#[test]
fn wire_format_is_fixed() {
    let event = Event {
        window: 7,
        kind: kind::BUTTON,
        key: 0,
        button: 1,
        pressed: true,
        x: -3,
        y: 300,
    };
    assert_eq!(
        event.encode(),
        [7, 0, 0, 0, 3, 0, 1, 1, 0xfd, 0xff, 0x2c, 0x01]
    );
    let (data, len) = OpenRequest {
        bits: 1,
        width: 480,
        height: 240,
        title: "Notes",
    }
    .encode()
    .unwrap();
    assert_eq!(&data[..len], b"\x01\0\0\0\0\0\0\0\xe0\x01\xf0\0Notes");
}

#[test]
fn notification_texts_are_one_short_line() {
    assert_eq!(
        proto::notification_text(b" Saved 3 notes "),
        Some("Saved 3 notes")
    );
    for bad in [&b""[..], b"   ", b"two\nlines", b"bell\x07", &[0xff, 0xfe]] {
        assert_eq!(proto::notification_text(bad), None, "{bad:?}");
    }
    assert!(proto::notification_text(&[b'a'; proto::MAX_NOTIFICATION]).is_some());
    assert!(proto::notification_text(&[b'a'; proto::MAX_NOTIFICATION + 1]).is_none());
}

#[test]
fn minimized_windows_hide_lose_the_focus_and_come_back_on_top() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 200, 100).unwrap();
    let b = m.open(OTHER, "B", "", 200, 100).unwrap();
    let fb = m.frames()[1].clone();
    let _ = (kinds(&mut m, APP), kinds(&mut m, OTHER));
    // Its minimize button hides it; the focus goes to the window below.
    let button = fb.minimize_button();
    assert!(m.button(1, true, button.x + 2, button.y + 2));
    assert!(m.frames().iter().find(|f| f.id == b).unwrap().minimized);
    assert_eq!(m.focus(), Focus::Window(a));
    assert_eq!(kinds(&mut m, OTHER), [(kind::FOCUS, b)]);
    // A hidden window takes no clicks: they reach what is under it.
    let inside = fb.content();
    let fa = m.frames().iter().find(|f| f.id == a).unwrap().clone();
    if !fa.outer().contains(inside.x + 1, inside.y + 1) {
        assert!(!m.button(1, true, inside.x + 1, inside.y + 1));
    }
    // The last one shown minimized: the Terminal gets the keyboard.
    m.minimize(a);
    assert_eq!(m.focus(), Focus::Terminal);
    // Restored (from the taskbar): shown again, on top, with the focus.
    m.set_focus(Focus::Window(b));
    assert!(!m.frames().last().unwrap().minimized);
    assert_eq!(m.frames().last().unwrap().id, b);
    assert_eq!(m.focus(), Focus::Window(b));
    assert!(m.frames()[0].minimized);
}
