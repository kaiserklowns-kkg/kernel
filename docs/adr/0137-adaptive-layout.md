# ADR-0137: Adaptive layout

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0080 (the app toolkit), ADR-0097 (resizing windows),
  ADR-0117 (Launchpad), ADR-0136 (the interface's scale)
- Part of Phase 10 (the desktop experience).

## Context

ADR-0136 made the interface the same physical size on any screen.

What was laid out on it still assumed one window size and one screen:

- **Fixed positions.** Toolbars placed their buttons at fixed x positions:
  - Text Editor's New sat past the right edge of a window at its smallest
    size;
  - Image Viewer's Fit sat off a narrow window, and in the middle of a
    wide one.
- **Overlaps.** Settings' permission buttons covered what had been
  decided.
- **Fixed sizes.**
  - Files' list kept 340 pixels whatever the window.
  - Calculator's keys followed the width alone.
  - Settings and Calculator could not be resized at all.
- **Not following a resize.** A fitted picture was not fitted again after
  a resize.
- **The desktop.**
  - Launchpad showed three rows of seven, however much screen there was.
  - The Terminal stayed 960 × 600.
  - A dialog was 560 wide even where the area was not.

## Decision

### Layout from the room there is: `oceans_draw::layout`

Pure geometry in logical pixels, host-tested:

- **`Bar`:** a toolbar's row. Items are taken from its left and its right
  end, and the room between them goes to a field or a title. It says
  whether an item still fits.
- **`Flow`:** items placed left to right that wrap onto the next line. It
  has a gap between items and one between lines, and says where its last
  line ends.
- **`window_size(base, min, area)`:** the size a window opens at.
  - It starts from the size its app was drawn for on 1280 × 800 (the area
    1280 × 692, `REFERENCE_AREA`).
  - It grows in proportion as the area the windows have is larger.
  - It is never smaller than the app's minimum, nor larger than the area.
  - On QEMU's screen a window keeps exactly the size it asks for.
- **`grid(room, cell, min, max)`:** how many columns and rows of tiles fit.

### The toolkit

- **Opening size.** `run_resizable` asks the screen's size (`SCREEN`,
  ADR-0136) before it opens the window and opens it at `window_size`. A
  window of a fixed size keeps it.
  - A 1080p laptop at 125 % gives Settings 912 × 568 instead of
    760 × 520.
  - A 1080p monitor at 100 % gives 1140 × 730.
- **New helpers.**
  - `Ui::button_width` says how wide a button is drawn.
  - `Ui::readable(max)` keeps text and rows of facts readable in a wide
    window.
- **`Ui::row`.** It keeps the value at the right and clips the name to
  what is left, so the two never overlap.
- **Keys after Tab.** Keys typed after Tab or Shift+Tab wait for the frame
  that moves the focus, and go to the widget it moved to.
  - **The bug.** Before, Tab and the keys after it in one batch of events
    went to the same frame, where Tab had not moved the focus yet. A
    window still starting (as Settings now is a little longer, asking the
    screen's size) lost them. Tab then End did not reach the sidebar, and
    the later keys went one widget too far.

### The apps lay out in their window

At the size each window opens at on QEMU, every position below is the
one it had, pixel for pixel.

- **Text Editor.** The name field takes what the buttons leave:
  - 320 pixels at 760;
  - from 160 to 480 in narrower and wider windows.

  Open, Save and New follow it, so at its smallest New is inside the
  window.
- **Image Viewer.**
  - Fit / Actual size is anchored at the right.
  - The name field shrinks.
  - Previous and Next become ‹ and › when the window is narrow.
  - The picture is fitted again whenever the window's size changes.
- **Files.**
  - Back is at the left; New folder and Delete at the right.
  - The path takes the room between them.
  - The list is half of what the margins leave: 340 at 760, from 220 to
    440.
  - Open and Open with… wrap in a narrow side panel, and the apps to open
    with are never wider than it.
- **Settings.** Resizable, from 600 × 420, its pages at most 720 wide.
  - The accents and the lock-after minutes wrap.
  - Apps' list is half the page.
  - A permission's Allow, Deny and Ask go on a line of their own when the
    page is too narrow for them beside what was decided.
- **Calculator.** Resizable, from 240 × 340. Its keys fill the width and
  are as tall as five rows fit, never taller than wide.
- **Activity Monitor.** A card's value is drawn smaller when it would not
  fit at the title's size.

### The desktop uses the screen

- **Launchpad.**
  - Its grid has as many columns as fit with 200 pixels free at either
    side: seven on a 1280-wide screen, from three to ten.
  - It has as many rows as fit above Restart and Shut Down.
  - The hit test, the drawing and the "more in the Terminal" note share
    the count.
- **The Terminal.** 960 × 600 on a small screen, seven tenths of the
  area's width and three quarters of its height on a larger one.
- **A system dialog.** Never wider than the area; its buttons are placed
  from its right end.
- **The desktop's icons.** Home and every app kept in Wave can be shown.
  A column ends above the dock, and the next starts to its left.

## Consequences

- **Screen size.**
  - **1280 × 800 (QEMU, every smoke probe).** Nothing moves.
  - **Larger screens.** Windows, Launchpad and the Terminal use the room.
- **Window size.** Every system app on the toolkit can be resized, except
  Music (its own milestone, ADR-0135). Notes stays the example app that
  draws its own pixels.
- **New apps.** They lay out with `Bar` and `Flow` from `ui.area`, rather
  than from fixed positions.
- **Not yet.**
  - **Scrolling.** Lists longer than their window are cut off at its
    bottom. Wheel events are not yet sent to windows, so scrolling is its
    own milestone.
  - **Short screens.** The notifications' panel and the Wi-Fi list keep
    their counts of cards and rows, which fit every screen the scales
    allow (at least 720 logical pixels high).

## Alternatives considered

- **A constraint or flexbox layout engine in the toolkit.** It would be
  much more code for immediate-mode widgets that are placed one call at a
  time. `Bar` and `Flow` cover every layout the system's apps have.
- **Opening windows at a fraction of the screen.** On QEMU's 1280 × 800
  that changes every window's size, and so every smoke probe. Growing in
  proportion from the reference area keeps the sizes the apps were drawn
  for there.
- **Making the manager shrink fixed-size windows to the area.** Their apps
  lay out for one size, so the content would be cut. They are resizable
  apps now instead.

## Checklist (master spec §48)

- **Purpose.** Apps and the desktop that use the room they have, at any
  window and screen size.
- **Architecture.** Pure geometry in `oceans-draw`; the toolkit opens
  windows to fit; apps and the desktop lay out from it.
- **API.**
  - `layout::{Bar, Flow, window_size, grid, REFERENCE_AREA}`;
  - `Ui::button_width`, `Ui::readable`.
- **Dependencies.** None new.
- **Security implications.** None: the window manager still bounds every
  size an app asks for (ADR-0059, ADR-0097).
- **Testing strategy.**
  - Host tests for `Bar`, `Flow`, `window_size` and `grid`, including
    the reference sizes.
  - The smoke tests' pixel probes on 1280 × 800, which hold only if
    nothing moved there.
  - Other sizes tried with `OCEANS_RESOLUTION` (ADR-0136).
- **Failure behaviour.**
  - Without the screen's size, a window opens at the size its app asks
    for.
  - A window too small for a row wraps it, or clips text.
