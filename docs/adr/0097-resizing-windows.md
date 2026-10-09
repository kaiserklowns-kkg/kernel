# ADR-0097: Resizing and maximizing windows

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0059 (windows), ADR-0076 and ADR-0078 (the window
  buttons), ADR-0080 (the toolkit)
- Part of Phase 10 (Alpha: basic apps).

## Context

A window kept the size its app opened it at. The zoom button was drawn
grey (ADR-0076, ADR-0078), and a long file in Text Editor or a large
picture in Image Viewer could not have more of the screen.

**What users expect, on every desktop:**
- **Windows:** drag any edge or corner; Maximize in the title bar;
  a double click on the title bar maximizes and restores.
- **macOS:** drag any edge or corner; the green button zooms (or goes
  full screen); a double click on the title bar zooms.
- **Linux (GNOME, KDE):** drag edges and corners; maximize in the title
  bar; a double click on the title bar maximizes.
- **Everywhere:** a window has a smallest size its app sets; while the
  edge is dragged the app lays itself out again; maximizing remembers
  where the window was.

**What Oceans adds:** apps draw into memory the display shares with them
(ADR-0059), of a fixed size. A new size needs new memory, and an app that
was never written for other sizes (Notes, Tiles, Calculator) must keep
working as it is.

## Decision

**Apps choose** (`op::RESIZABLE`, `[window][min width][min height]`):
- A window can be resized only once its app says so, with the smallest
  size it can lay out (at least 64×32, at most its size then). Other
  windows keep their size; their zoom button stays grey.
- In the toolkit, `run_resizable` (with the smallest size); a frame laid
  out from `ui.area` needs nothing more.

**The user resizes:**
- **Edges and corners:** caught within 5 pixels outside the frame, or on
  its border (a corner within 14 pixels along an edge); never the title
  bar's inside. The frame follows the pointer; the window stays at least
  the app's smallest size and at most the area, 1024×768 (the protocol's
  limit) and the pixels the other windows leave. A top edge never takes
  the title bar above the area.
- **Maximize:** the zoom button (green when the window has the focus) or
  a double click on the title bar (two presses within 500 ms and 4
  pixels, timed when the input service read them, so that a slow frame
  between them does not part them) gives the window the area's size, in its middle if the largest
  size is smaller, and remembers where it was; again, and it goes back.
  Resizing by an edge forgets that.

**The app follows** (`kind::RESIZE`, `op::RESIZE`):
- When a resize ends (the button comes up, or the window is maximized or
  restored), the app gets a `RESIZE` event with the new width and height.
- `RESIZE` gives it new pixel memory of the window's size now, painted in
  the windows' colour; the old memory goes. The app draws and presents.
- While the edge moves, and until the app presents, the old pixels are
  shown, as much as fits, the rest in the windows' colour: the frame
  never waits for the app.
- The display logs each resize (`display: Text Editor's window resized
  to 1024x663`).

**Apps that come with the system:** Text Editor (smallest 480×300),
Files (600×320), Image Viewer (480×320, the picture fitted again) and
Activity Monitor (480×380) are resizable, and their versions go up so
that disks with the older ones install them. Calculator and Settings,
laid out for one size, are not (yet).

The policy (edges, bounds, maximize and restore, which app hears what)
is in `oceans-window`'s `Manager`, host-tested; the display service
moves the memory.

## Consequences

- The windows people work in can use the screen.
- An app that never asks keeps its size: nothing old breaks.
- **Not yet:**
  - a pointer that shows the edge can be dragged;
  - snapping to half the screen, and keyboard shortcuts for it;
  - full screen;
  - resizing for Go apps (`go/oceans/window` neither asks nor reads the
    `RESIZE` event);
  - screens larger than 1024×768 of content per window (the protocol's
    limit, ADR-0059).

## Alternatives considered

- **Every window resizable, the app's pixels scaled:** blurry text, and
  apps laid out for one size look broken; Windows' and macOS' own apps
  choose.
- **The app asked for every step of a drag:** each step would wait for
  the app to allocate and draw; following the outline and telling the
  app once keeps the frame smooth whatever the app does.
- **The display sending the new memory with the event:** events are
  fixed-size records without handles; the app asks when it is ready, and
  an app that never asks keeps its old pixels.
- **Full-screen mode instead of maximize:** the menu bar and the dock are
  how windows are switched on Oceans; maximize keeps them.

## Checklist (master spec §48)

- **Purpose:** let the user give a window the size they need.
- **Architecture:**
  - the policy in `oceans-window` (`Frame::min`, `restore`, `edges_at`;
    `Manager::zoom`, `double_click`, `set_resizable`, `size`);
  - the memory in the display service (`share_pixels`, `resize`), the
    drawing of pixels of another size and the green zoom button
    (`desktop.rs`), double clicks (`main.rs`);
  - `Window::set_resizable` and `Window::resize` in
    `oceans-display-proto`; `run_resizable` in the toolkit.
- **API:** `op::RESIZABLE`, `op::RESIZE`, `kind::RESIZE`; the toolkit's
  `run_resizable`.
- **Dependencies:** none.
- **Security:**
  - an app resizes only its own window, and only to the size the user
    gave it: `RESIZE` takes no size;
  - sizes stay within the area, the protocol's limit and the pixel
    budget of all windows;
  - the display's mapping of an app's pixels stays read-only.
- **Testing:**
  - unit (`oceans-window`): only windows that asked can be resized; edges,
    corners and the grip; the smallest and largest sizes, the left and top
    edges; maximize, restore, the double click and what it ignores; the
    pixel budget;
  - smoke: Text Editor's zoom button maximizes it (its page then covers a
    point outside its old frame, and its old title bar); a double click on
    the title bar restores it; its right edge dragged 200 pixels narrows
    it, and the page is laid out again; the display logs the three sizes.
- **Failure behaviour:**
  - **An app that never takes the new pixels:** its old ones are shown,
    the rest in the windows' colour.
  - **No memory for the new pixels:** `RESIZE` answers `NoMemory`; the
    app keeps drawing into its old pixels, shown as far as they fit.
  - **A smallest size out of bounds:** `RESIZABLE` is refused; the window
    keeps its size.
