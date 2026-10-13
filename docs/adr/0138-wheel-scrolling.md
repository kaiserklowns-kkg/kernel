# ADR-0138: Scrolling with the wheel

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0042 (pointer input), ADR-0059 (windows), ADR-0080
  (the app toolkit), ADR-0132 (the PS/2 wheel), ADR-0137 (adaptive
  layout)
- Part of Phase 10 (the desktop experience).

## Context

The input service has decoded wheels since ADR-0042 (USB) and ADR-0132
(PS/2 touchpads and mice), as `Kind::Wheel`, but the desktop dropped
them.

Nothing scrolled. Settings' longer pages, Files' folders of hundreds of
entries and Activity Monitor's processes were cut off at the bottom of
their windows. Text Editor and the Terminal could only be moved with the
keyboard. ADR-0137 left scrolling as its own milestone.

## Decision

### The wheel goes to the window under the pointer

- **The event.** A new window event, `kind::WHEEL` (10), carries:
  - the detents down the page (`key`, an `i8`, positive towards the end,
    as a wheel turned towards the user scrolls);
  - the detents to the right (`button`);
  - where the pointer is in the window (`x`, `y`).

  `Event::wheel` reads them.
- **Who gets it.** `Manager::wheel` sends it to the window under the
  pointer, whether it has the focus or not, as other systems scroll what
  is pointed at.
  - Nothing is sent while a window is dragged or resized, or when the
    pointer is over a title bar.
  - Turns that come before the app takes them add up into one event.
- **What blocks it.** The desktop sends no wheel to windows while
  something lies over them: the lock screen, a dialog, Launchpad, Control
  Center, the notifications or the user's menu.
- **The Terminal.** Over the Terminal, the wheel shows earlier rows of
  the console, three a detent. A key typed there brings the cursor's rows
  back.

### Clipping in `oceans-draw`

`Surface::clip_to(area)` keeps everything drawn inside `area` (logical;
`None`: the whole surface):

- fills, rounded shapes and tints;
- circles, paths and text.

Whatever scrolls out of a view is not drawn.

### `Scroll` in the toolkit

A view that scrolls around any widgets, kept by the app as a `Scroll`
(the offset, and how tall its content was):

- **`Ui::begin_scroll(view, &scroll)`.** It clips drawing to `view` and
  lays widgets out from the view's top less the offset, as usual.
  - Clicks and hovering reach only what shows.
  - Room is kept for the bar only when the content is taller than the
    view, so a page that fits is laid out as before.
- **`Ui::end_scroll(view, &mut scroll)`.** It measures the content.
  - The wheel turned over the view scrolls it three lines a detent.
  - The widget with the keyboard (Tab, ADR-0106) is scrolled into sight.
  - The offset stays within the content.
  - A rounded bar at the right shows where the view is.
- **`Input::wheel`.** It has the detents of the frame's events, and
  `pointer` is where they turned.

### The apps

- **Settings.** Every page scrolls in its window. Apps' page is as long
  as the longer of its two columns. A new page starts at the top.
- **Files.** The list column scrolls, from the top of each folder.
- **Text Editor.** The wheel moves the page three lines a detent, and a
  long line sideways. The page follows the cursor only when the cursor
  moved, so the wheel can look elsewhere.
- **Image Viewer.** The wheel pans a picture larger than its view.
- **Activity Monitor.** The wheel moves the first process the table
  shows.

## Consequences

- **What scrolls.** A wheel or a touchpad's scrolling (in its mouse mode)
  scrolls what is pointed at in every system app. A window behind
  another can be scrolled without raising it.
- **New apps.** A page longer than its window takes `begin_scroll` and
  `end_scroll` around its widgets.
- **The smoke tests.** QEMU's monitor turns a USB mouse's wheel
  (`mouse_move 0 0 1`), and a smoke step already sees the turn reach the
  input service (ADR-0042).
  - **What is host-tested.** What the desktop does with it: the window
    under the pointer, focused or not; turns adding up; title bars and the
    desktop excluded; the event's wire format. Clipping is host-tested
    too.
  - **What the probes check.** That pages which fit are laid out as
    before.
- **Not yet.** Smooth (pixel) scrolling from touchpads' own protocols
  (ADR-0132 left them for later); the Wi-Fi list and the notifications
  scroll when they need to.

## Alternatives considered

- **Scrolling only lists.** Settings' pages, the long ones, are not
  lists. Clipping at the surface lets any run of widgets scroll with the
  same two calls.
- **The wheel to the focused window.** Other systems scroll what the
  pointer is over. With focus it would take a click first, which also
  moves things in some apps.
- **Retained scroll widgets that keep their content.** The toolkit is
  immediate-mode: a view laid out again each frame from its offset is
  simpler, and the app keeps the one number.

## Checklist (master spec §48)

- **Purpose.** Long content reachable with the wheel, in every system
  app and the Terminal.
- **Architecture.** Wheel routing in the window manager, clipping in
  `oceans-draw`, `Scroll` in the toolkit.
- **API.**
  - `kind::WHEEL`, `Event::wheel`, `Manager::wheel`;
  - `Surface::clip_to`, `Surface::clip_area`;
  - `Scroll`, `Ui::begin_scroll`, `Ui::end_scroll`, `Input::wheel`.
- **Dependencies.** None new.
- **Security implications.**
  - An app gets the wheel only over its own window's content, as it gets
    clicks.
  - Detents are bounded to an `i8` and positions to the window.
- **Testing strategy.**
  - Host tests for routing and adding up, the event's encoding, and
    clipping at 200 %.
  - The smoke tests' pixel probes for unchanged layouts.
- **Failure behaviour.**
  - Wheel events an app does not take are dropped with its queue's other
    events past its limit.
  - A view whose content shrank is scrolled back within it.
