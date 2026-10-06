# ADR-0078: The desktop's look: a menu bar, a dock, light windows

- Status: Accepted
- Date: 2026-10-06
- Amends: ADR-0076 (the taskbar becomes a dock and a menu bar; the look
  becomes light)
- Part of Phase 10 (Alpha).

## Context

ADR-0076 gave the desktop a Windows-style taskbar. Looking at it, the
owner asked for the look of the macOS Big Sur UI kit
([Figma community file](https://www.figma.com/community/file/949158727443209284/macos-big-sur-ui-kit),
by UI8, CC BY 4.0), with a new bottom bar in place of the taskbar.

## Decision

**Oceans follows the Big Sur design language and draws everything
itself.**
- **The language:**
  - light, translucent surfaces;
  - a menu bar across the top;
  - large rounded corners and soft shadows;
  - round window buttons at the left of the title bar;
  - a floating dock.
- **Nothing is taken from Apple or the kit:**
  - no images, icons, fonts or the Apple mark;
  - the shapes, colours and wallpaper are Oceans' own.

### The layout (1280 x 800 shown; every position follows the screen's size)

- **The menu bar,** 28 pixels along the top, white and translucent over
  the wallpaper.
  - **At the left:** the Oceans mark (two waves), which opens the apps
    panel, then the app in use in bold (the focused window's app, the
    Terminal, or "Oceans").
  - **At the right:** memory use, then the day, date and time (UTC).
- **The dock replaces the taskbar.** It is a floating, rounded,
  translucent shelf, 8 pixels above the bottom edge, as wide as its
  icons.
  - **Its icons, 48 pixels each:** the apps button, the Terminal, then
    each open window in the order opened.
  - **Marks and labels:** a dot under what is open; a minimized window's
    icon is faded. Pointing at an icon shows its name in a dark label above
    the dock.
  - **Clicks** behave as in ADR-0076: a window's icon brings it forward,
    or minimizes it if it is already in front with the keyboard.
- **The apps panel,** above the dock: the Terminal and the installed apps
  as tiles on a light rounded panel.
- **Windows,** the Terminal's too:
  - **The frame:** a light title bar, the focused one a shade darker; a
    hairline border; 10-pixel corners; a soft shadow.
  - **The title:** centred, the app's verified name in bold, then its own
    title.
  - **The buttons,** round at the left:
    - close (red) and minimize (yellow);
    - resize (green in Big Sur), drawn grey: windows cannot be resized
      yet;
    - a window without the focus has all three grey;
    - pointing at them shows what they do.
  - **The Terminal's** close button is grey: the shell stays. Its body is
    dark.
- **The wallpaper,** drawn once at start and copied at each frame: a night
  sky over shallow water, and three waves rolling in, their crests catching
  the light.
- **Notifications:** at the top right, under the menu bar, on light
  rounded cards.
- **System dialogs:** light rounded sheets in the middle, over a dimmed
  desktop; the primary button is in the accent.

### In the code

- **The window manager:** its button rectangles move to the left
  (`close_button`, `minimize_button`, `zoom_button`). Nothing else
  changes, and neither does the protocol.
- **The canvas** gains:
  - translucent tints with rounded corners;
  - anti-aliased circles;
  - soft shadows;
  - a cached wallpaper.

## Consequences

- The desktop looks current and light, and matches what many people
  know.
- **The colours change from dark to light.** The web experience stays
  dark for now. Matching it is later work.
- **Still not done:**
  - resizing windows (the grey button);
  - menus in the menu bar;
  - dock magnification and pinned apps;
  - real blur behind translucent surfaces (a plain tint here).

## Alternatives considered

- **Keeping the Windows-style taskbar (ADR-0076):** the owner chose this
  look instead.
- **Using the kit's assets:** they are Apple's design and icons. Oceans
  must not ship them, and does not need them.

## Checklist (master spec §48)

- **Purpose:** the desktop's look, as the owner asked.
- **Architecture:**
  - `desktop.rs` (layout, drawing, the wallpaper);
  - `canvas.rs` (tints, circles, shadows, the cached wallpaper);
  - the window manager's button rectangles.
- **API:** `Frame::zoom_button`, `BUTTON_STEP`; no protocol change.
- **Dependencies:** none.
- **Security:**
  - app names in title bars and the dock are the ones Core verified;
  - dialogs are drawn by the system and modal, as before.
- **Testing:**
  - unit: the window manager, with its buttons at the left;
  - smoke: the apps panel's colour where the layout puts it; Hello
    started from it, with its permission dialog; Notes' window and the
    focus on the captures (title bar shades); the menu bar light over a
    dark sky; the install dialog and Tiles' window at their places.
- **Failure behaviour:** unchanged. A screen under 900 x 420 keeps the
  kernel console.
