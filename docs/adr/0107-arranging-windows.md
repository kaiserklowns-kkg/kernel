# ADR-0107: Arranging windows: halves, quarters, full screen, shortcuts

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0097 (resizing and maximizing), ADR-0078 (desktop
  look), ADR-0105 (UI direction)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

ADR-0097 let windows be resized and maximized, and left out snapping to
half the screen, full screen and keyboard shortcuts. Two windows side by
side meant resizing both by hand. Windows were also limited to 1024 × 768
of content, less than the screen.

**What users expect, on every system:**
- **Windows 11:** a title bar dragged to the screen's side takes that
  half, to a corner a quarter, to the top all of it, with a preview
  before letting go; Win+Left, Right, Up and Down do the same from the
  keyboard; a window dragged away from its place takes back its size.
- **macOS (Sequoia):** windows tile the same way by dragging to the edges
  and corners; full screen covers the screen, the menu bar and the Dock
  giving way.
- **GNOME and KDE:** halves by the sides and the Super arrows; the top
  maximizes.
- **Everywhere:** arranging windows is quick, previewed, and undone by
  dragging the window away.

## Decision

### Where a window can go

`oceans_window::Tile`, for a window its app made resizable (ADR-0097):
- **Halves:** `Left`, `Right`: half of the area, its full height.
- **Quarters:** `TopLeft`, `TopRight`, `BottomLeft`, `BottomRight`.
- **`Maximized`:** all of the area (between the menu bar and the dock).
- **`FullScreen`:** all of the screen, with no frame. The menu bar and
  the dock give way while it has the focus, and come back when another
  window or the Terminal takes it.

The window takes at least its smallest size, and no more than the pixels
the other windows leave (in its part's middle then). The frame keeps
where it was before (`restore`) and its place (`tile`).

### By dragging the title bar

- **To the screen's left or right side** (within 4 pixels): that half;
  near the top or bottom (80 pixels): that quarter.
- **To the area's top** (the menu bar included): all of the area.
- **A preview:** while the pointer is there, the desktop tints the place
  the window would take; let go, the window takes it.
- **A tiled window dragged away** (more than 6 pixels) takes back its size,
  grabbed as far along its title bar as before.
- A resize by an edge leaves the window where the user left it.

### From the keyboard

Super (the Windows key) with:
- **Left / Right:** that half; from the other half, back where it was.
- **Up:** all of the area.
- **Down:** back where it was; a window already there is minimized.
- **F:** full screen, or back.

The bytes are `display::KEY_TILE_LEFT` … `KEY_FULL_SCREEN` (0xa4–0xa8),
from the PS/2 keyboard (Super is `0xe0 0x5b`, `0xe0 0x5c`) and the USB
keyboard (the GUI modifiers). Other keys with Super send nothing. The
window manager takes them; no app sees them. A window of a fixed size can
only be minimized.

### Windows as large as the screen

The protocol's limit becomes 3840 × 2160 (4K; from 1024 × 768), so a
maximized or full-screen window covers the screen. The display service's
pixel budget (twice the screen) still bounds all windows together.

### Where it lives

- **The window manager** (`libs/window`, host-tested): the places, the
  drag's snapping, coming loose, the shortcuts, full screen.
- **The desktop** (`user/display`): it draws the preview, and leaves out
  the menu bar and the dock, and a full-screen window's frame.

## Consequences

- Two windows side by side, or four, take one drag each, or a shortcut.
- The zoom button and a double click maximize and restore as before, a
  tiled window included.
- The smoke test drags Text Editor to the left half, moves it with the
  shortcuts to the right half, all of the area and full screen, and
  checks the screen at each.
- **Not yet:**
  - choosing a second window for the other half (Windows' Snap Assist);
  - layouts (Snap Layouts), resizing two tiled windows together;
  - several desktops (Spaces, virtual desktops);
  - full screen by the zoom button (macOS); the zoom button maximizes;
  - the menu bar shown again at the top edge in full screen;
  - Go apps (`go/oceans/window` does not ask to be resizable, so its
    windows cannot be tiled).

## Alternatives considered

- **macOS' green button for full screen:** Windows' and Linux' users
  expect it to maximize, and it already does (ADR-0097); full screen has
  its shortcut.
- **Snapping fixed-size windows too:** their apps lay out for one size; a
  half would cut them or leave them small in a large frame.
- **Keeping the 1024 × 768 limit:** a maximized window would not fill a
  1280-wide screen, and full screen would not cover it.
- **Tiling automatically (a tiling window manager):** most users expect
  windows where they put them; tiling is the user's choice per window.

## Checklist (master spec §48)

- **Purpose:** windows side by side, all of the area or the screen, by a
  drag or a shortcut.
- **Architecture:** decisions in `oceans_window::Manager`; drawing in the
  display service; key bytes from both keyboards.
- **API:** `Tile`, `Manager::tile`, `full_screen`, `snap_preview`,
  `set_screen`, `TileKey`; `display::KEY_TILE_*`, `KEY_FULL_SCREEN`;
  `proto::MAX_WIDTH`, `MAX_HEIGHT` raised. No new request: apps hear of
  their new size by the `RESIZE` event, as before.
- **Dependencies:** none.
- **Security implications:** the keys are the desktop's, never an app's;
  an app cannot tile itself or take the screen; the pixel budget bounds
  the larger windows; a full-screen app cannot hide the permission
  dialogs, which the desktop draws over every window.
- **Testing strategy:** host tests for each place, the snap preview,
  coming loose, the shortcuts, full screen and the limit; USB Super keys;
  the boot smoke drags and presses them and checks the screen.
- **Failure behaviour:** a window that cannot be resized ignores all but
  Super+Down; an app that does not redraw at its new size shows its old
  pixels in the frame, the rest in the windows' colour (ADR-0097).
