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
    bad[4] = 8;
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
    // Clear of the buttons at the left (ADR-0078).
    assert!(m.button(1, true, title.x + 100, title.y + 10));
    assert!(m.pointer_moved(title.x + 200, title.y + 60));
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

#[test]
fn an_app_copies_only_while_focused_and_once_per_input() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 100, 100).unwrap();
    // Nothing from the user yet.
    assert_eq!(m.copy(APP, b"hello"), Err(Status::NotAllowed));
    m.key(proto::CTRL_V);
    assert_eq!(m.clipboard_len(), 0);
    m.key(0x03);
    assert_eq!(m.copy(APP, b"hello"), Ok(5));
    assert_eq!(m.clipboard_len(), 5);
    // Once per key: the next copy waits for the user again.
    assert_eq!(m.copy(APP, b"again"), Err(Status::NotAllowed));
    // A click in the content allows it too.
    let content = m.frames()[0].content();
    m.button(1, true, content.x + 5, content.y + 5);
    assert_eq!(m.copy(APP, b"clicked"), Ok(7));
    // Another app, even with a key of its own before the focus moved on.
    let b = m.open(OTHER, "B", "", 100, 100).unwrap();
    m.key(b'x');
    m.set_focus(Focus::Window(a));
    assert_eq!(m.copy(OTHER, b"sneaky"), Err(Status::NotAllowed));
    // Focus moving away ends what the user allowed.
    m.key(b'y');
    m.set_focus(Focus::Window(b));
    m.set_focus(Focus::Window(a));
    assert_eq!(m.copy(APP, b"late"), Err(Status::NotAllowed));
    // Text is bounded and has no control characters but line breaks and tabs.
    m.key(b'z');
    assert_eq!(m.copy(APP, b""), Err(Status::BadRequest));
    assert_eq!(m.copy(APP, b"bell\x07"), Err(Status::BadRequest));
    assert_eq!(m.copy(APP, &[0xff]), Err(Status::BadRequest));
    let big = std::vec![b'a'; proto::MAX_CLIPBOARD + 1];
    assert_eq!(m.copy(APP, &big), Err(Status::BadRequest));
    assert_eq!(m.copy(APP, "two\r\nlines\tก".as_bytes()), Ok(14));
}

#[test]
fn pasting_is_pushed_once_to_the_focused_window() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 100, 100).unwrap();
    m.key(b'c');
    m.copy(APP, b"text").unwrap();
    m.take_events(APP, 100);
    // Nothing was pasted: nothing to take.
    assert_eq!(m.take_paste(APP), Err(Status::NotFound));
    // Ctrl+V and Ctrl+Shift+V paste; the app sees an event, not the key.
    for key in [proto::CTRL_V, proto::KEY_PASTE] {
        assert_eq!(m.key(key), KeyRoute::Window(APP));
        assert_eq!(kinds(&mut m, APP), [(kind::PASTE, a)]);
        assert_eq!(m.take_paste(APP), Ok("text"));
        assert_eq!(m.take_paste(APP), Err(Status::NotFound));
    }
    // Another app cannot take a paste meant for this one.
    let b = m.open(OTHER, "B", "", 100, 100).unwrap();
    m.set_focus(Focus::Window(a));
    m.key(proto::CTRL_V);
    assert_eq!(m.take_paste(OTHER), Err(Status::NotFound));
    // Nor this one once the focus moved on.
    m.set_focus(Focus::Window(b));
    assert_eq!(m.take_paste(APP), Err(Status::NotFound));
    // The clipboard outlives the app that copied.
    assert_eq!(m.close_owner(APP), [a]);
    assert_eq!(m.clipboard_len(), 4);
}

#[test]
fn the_terminal_pastes_one_line_with_ctrl_shift_v_only() {
    let mut m = manager();
    assert_eq!(m.terminal_paste(), None);
    assert_eq!(m.key(proto::KEY_PASTE), KeyRoute::TerminalPaste);
    // Ctrl+V is the shell's byte; Ctrl+C stays an interrupt.
    assert_eq!(m.key(proto::CTRL_V), KeyRoute::Terminal(proto::CTRL_V));
    assert_eq!(m.key(0x03), KeyRoute::Terminal(0x03));
    m.open(APP, "A", "", 100, 100).unwrap();
    m.key(b'c');
    m.copy(APP, b"echo hi\tthere\nrm -rf /\n").unwrap();
    assert_eq!(
        m.terminal_paste(),
        Some((std::string::String::from("echo hithere"), true))
    );
    m.key(b'c');
    m.copy(APP, b"one line\r\n\n").unwrap();
    assert_eq!(
        m.terminal_paste(),
        Some((std::string::String::from("one line"), false))
    );
}

fn resizes(manager: &mut Manager, owner: u64) -> Vec<(u32, i16, i16)> {
    manager
        .take_events(owner, 100)
        .iter()
        .filter(|e| e.kind == kind::RESIZE)
        .map(|e| (e.window, e.x, e.y))
        .collect()
}

#[test]
fn only_windows_their_apps_call_resizable_can_be_resized() {
    let mut m = manager();
    let id = m.open(APP, "A", "", 400, 300).unwrap();
    let o = m.frames()[0].outer();
    // Fixed: no edges, the zoom button does nothing.
    assert_eq!(m.frames()[0].edges_at(o.x + o.w + 2, o.y + 100), 0);
    assert!(!m.zoom(id));
    // Bounds on the smallest size: the protocol's, and the size now.
    assert_eq!(m.set_resizable(OTHER, id, 200, 100), Err(Status::NotFound));
    assert_eq!(m.set_resizable(APP, id, 63, 100), Err(Status::BadRequest));
    assert_eq!(m.set_resizable(APP, id, 401, 100), Err(Status::BadRequest));
    assert_eq!(m.set_resizable(APP, id, 200, 100), Ok(()));
    let f = &m.frames()[0];
    // Outside the right edge, on the bottom border, at a corner; not
    // inside, not on the title bar's top edge from inside.
    assert_eq!(f.edges_at(o.x + o.w + 2, o.y + 100), edge::RIGHT);
    assert_eq!(f.edges_at(o.x + 100, o.y + o.h - 1), edge::BOTTOM);
    assert_eq!(
        f.edges_at(o.x + o.w + 1, o.y + o.h + 1),
        edge::RIGHT | edge::BOTTOM
    );
    assert_eq!(f.edges_at(o.x - 3, o.y - 3), edge::LEFT | edge::TOP);
    assert_eq!(f.edges_at(o.x + 100, o.y + 100), 0);
    assert_eq!(f.edges_at(o.x + 100, o.y + 5), 0);
    assert_eq!(f.edges_at(o.x + o.w + GRIP, o.y + 100), 0);
}

#[test]
fn dragging_an_edge_or_a_corner_resizes_within_bounds() {
    let mut m = manager();
    let id = m.open(APP, "A", "", 400, 300).unwrap();
    m.set_resizable(APP, id, 200, 100).unwrap();
    m.take_events(APP, 100);
    let o = m.frames()[0].outer();
    // The bottom right corner, 100 right and 50 down.
    let (x, y) = (o.x + o.w + 1, o.y + o.h + 1);
    assert!(m.button(1, true, x, y));
    assert!(m.pointer_moved(x + 100, y + 50));
    let f = &m.frames()[0];
    assert_eq!((f.x, f.y, f.width, f.height), (o.x, o.y, 500, 350));
    // The app hears of it once, when the button comes up.
    assert!(resizes(&mut m, APP).is_empty());
    assert!(m.button(1, false, x + 100, y + 50));
    assert_eq!(resizes(&mut m, APP), [(id, 500, 350)]);
    // The left edge: the right edge stays; never under the smallest size.
    let o = m.frames()[0].outer();
    let right = o.x + o.w;
    m.button(1, true, o.x - 2, o.y + 100);
    m.pointer_moved(o.x + 1000, o.y + 100);
    m.button(1, false, o.x + 1000, o.y + 100);
    let f = &m.frames()[0];
    assert_eq!((f.width, f.outer().x + f.outer().w), (200, right));
    // Never larger than the area (972 wide: 970 of content).
    let o = f.outer();
    m.button(1, true, o.x + o.w, o.y + 100);
    m.pointer_moved(o.x + 5000, o.y + 100);
    m.button(1, false, o.x + 5000, o.y + 100);
    assert_eq!(m.frames()[0].width, 970);
    assert_eq!(resizes(&mut m, APP).len(), 2);
    // A press and release without a move: no event.
    let o = m.frames()[0].outer();
    m.button(1, true, o.x + 50, o.y + o.h);
    m.button(1, false, o.x + 50, o.y + o.h);
    assert!(resizes(&mut m, APP).is_empty());
}

#[test]
fn the_top_edge_keeps_the_title_bar_in_the_area() {
    let mut m = manager();
    let id = m.open(APP, "A", "", 400, 300).unwrap();
    m.set_resizable(APP, id, 200, 100).unwrap();
    let o = m.frames()[0].outer();
    let bottom = o.y + o.h;
    m.button(1, true, o.x + 100, o.y - 2);
    m.pointer_moved(o.x + 100, -500);
    m.button(1, false, o.x + 100, -500);
    let f = &m.frames()[0];
    assert_eq!((f.y, f.outer().y + f.outer().h), (AREA.y, bottom));
}

#[test]
fn zoom_and_a_double_click_maximize_and_restore() {
    let mut m = manager();
    let id = m.open(APP, "A", "", 400, 300).unwrap();
    m.set_resizable(APP, id, 200, 100).unwrap();
    m.take_events(APP, 100);
    let before = m.frames()[0].clone();
    // The zoom button: the whole area (972 x 732 with the frame).
    let zoom = before.zoom_button();
    assert!(m.button(1, true, zoom.x + 5, zoom.y + 5));
    let f = m.frames()[0].clone();
    assert_eq!((f.x, f.y, f.width, f.height), (AREA.x, AREA.y, 970, 703));
    assert_eq!(f.restore, Some((before.x, before.y, 400, 300)));
    assert_eq!(resizes(&mut m, APP), [(id, 970, 703)]);
    // A double click on the title bar puts it back.
    let title = f.title_bar();
    assert!(m.double_click(title.x + 200, title.y + 10));
    let f = &m.frames()[0];
    assert_eq!(
        (f.x, f.y, f.width, f.height),
        (before.x, before.y, 400, 300)
    );
    assert_eq!(f.restore, None);
    assert_eq!(resizes(&mut m, APP), [(id, 400, 300)]);
    // Not on the buttons, not inside, not on a fixed window.
    let f = m.frames()[0].clone();
    let close = f.close_button();
    assert!(!m.double_click(close.x + 5, close.y + 5));
    assert!(!m.double_click(f.content().x + 5, f.content().y + 5));
    let fixed = m.open(OTHER, "B", "", 100, 100).unwrap();
    let title = m.frames()[1].title_bar();
    assert!(!m.double_click(title.x + 80, title.y + 10));
    assert_eq!(m.frames()[1].id, fixed);
    // A double click also ends the drag its first press began.
    let title = m.frames()[0].title_bar();
    m.button(1, true, title.x + 200, title.y + 10);
    assert!(m.double_click(title.x + 200, title.y + 10));
    assert!(!m.pointer_moved(title.x + 300, title.y + 100));
}

#[test]
fn other_windows_pixels_bound_the_largest_size() {
    let budget = 970 * 703 + 500 * 400;
    let mut m = Manager::new(AREA, budget);
    m.open(OTHER, "B", "", 900, 400).unwrap();
    let id = m.open(APP, "A", "", 400, 300).unwrap();
    m.set_resizable(APP, id, 200, 100).unwrap();
    assert!(m.zoom(id));
    let f = m.frames().last().unwrap();
    assert_eq!(f.width, 970);
    assert!(
        (f.width * f.height) as usize + 900 * 400 <= budget,
        "{}",
        f.height
    );
    assert_eq!(m.size(APP, id), Ok((970, f.height as u16)));
    assert_eq!(m.size(OTHER, id), Err(Status::NotFound));
}

#[test]
fn opening_a_file_follows_the_users_input_like_copying() {
    let mut m = manager();
    let a = m.open(APP, "A", "", 100, 100).unwrap();
    assert_eq!(m.take_open(APP), Err(Status::NotAllowed));
    m.key(b'\r');
    assert_eq!(m.take_open(APP), Ok(()));
    assert_eq!(m.take_open(APP), Err(Status::NotAllowed));
    // A click allows one more; the other app none, focus moving ends it.
    let content = m.frames()[0].content();
    m.button(1, true, content.x + 5, content.y + 5);
    m.open(OTHER, "B", "", 100, 100).unwrap();
    m.set_focus(Focus::Window(a));
    assert_eq!(m.take_open(APP), Err(Status::NotAllowed));
    assert_eq!(m.take_open(OTHER), Err(Status::NotAllowed));
    // Copying and opening are allowed apart.
    m.key(b'x');
    m.copy(APP, b"text").unwrap();
    assert_eq!(m.take_open(APP), Ok(()));
}

#[test]
fn names_to_open_stay_in_home() {
    for good in ["notes.txt", "folder/picture.png", "ไทย.txt", "a b.md"] {
        assert_eq!(proto::open_name(good.as_bytes()), Some(good), "{good}");
    }
    for bad in [
        &b""[..],
        b"/etc/passwd",
        b"../keep/x",
        b"a/../b",
        b"a//b",
        b"./a",
        b"a/",
        b"line\nbreak",
        &[0xff],
    ] {
        assert_eq!(proto::open_name(bad), None, "{bad:?}");
    }
    assert!(proto::open_name(&[b'a'; proto::MAX_OPEN_NAME + 1]).is_none());
}

#[test]
fn volume_keys_go_to_the_desktop_whoever_has_the_focus() {
    let mut m = manager();
    assert_eq!(m.key(proto::KEY_MUTE), KeyRoute::Volume(VolumeKey::Mute));
    m.open(APP, "A", "", 100, 100).unwrap();
    m.take_events(APP, 100);
    assert_eq!(m.key(proto::KEY_VOLUME_UP), KeyRoute::Volume(VolumeKey::Up));
    assert_eq!(m.key(proto::KEY_VOLUME_DOWN), KeyRoute::Volume(VolumeKey::Down));
    // The app never sees them, nor does a press count as its input.
    assert!(m.take_events(APP, 100).is_empty());
    assert_eq!(m.copy(APP, b"x"), Err(Status::NotAllowed));
}

#[test]
fn volume_keys_step_by_five_and_unmute() {
    assert_eq!(VolumeKey::Up.apply((70, false)), (75, false));
    assert_eq!(VolumeKey::Up.apply((98, true)), (100, false));
    assert_eq!(VolumeKey::Down.apply((3, false)), (0, false));
    assert_eq!(VolumeKey::Down.apply((40, true)), (35, false));
    assert_eq!(VolumeKey::Mute.apply((40, false)), (40, true));
    assert_eq!(VolumeKey::Mute.apply((40, true)), (40, false));
    assert_eq!(VolumeKey::of(b'a'), None);
}

#[test]
fn media_keys_go_to_the_player_that_asked_or_was_looked_at_last() {
    let mut m = manager();
    // Nobody asked: they go nowhere.
    assert_eq!(m.key(proto::KEY_PLAY_PAUSE), KeyRoute::Consumed);
    let player = m.open(APP, "Music", "", 100, 100).unwrap();
    let other = m.open(OTHER, "Editor", "", 100, 100).unwrap();
    assert_eq!(m.want_media_keys(OTHER, player), Err(Status::NotFound));
    m.want_media_keys(APP, player).unwrap();
    m.take_events(APP, 100);
    // The editor has the focus; the player gets the key, as a key event.
    assert_eq!(m.focus(), Focus::Window(other));
    assert_eq!(m.key(proto::KEY_NEXT), KeyRoute::Window(APP));
    let events = m.take_events(APP, 100);
    assert_eq!((events[0].kind, events[0].key, events[0].window), (kind::KEY, proto::KEY_NEXT, player));
    // No input of the player's: no copy.
    assert_eq!(m.copy(APP, b"x"), Err(Status::NotAllowed));
    // A second player asks: it has them; looking at the first gives them back.
    m.want_media_keys(OTHER, other).unwrap();
    assert_eq!(m.media_owner(), Some(OTHER));
    m.set_focus(Focus::Window(player));
    assert_eq!(m.media_owner(), Some(APP));
    // A player gone: to the one before, then nowhere.
    m.close_owner(APP);
    assert_eq!(m.media_owner(), Some(OTHER));
    m.close(OTHER, other).unwrap();
    assert_eq!(m.key(proto::KEY_STOP), KeyRoute::Consumed);
}
