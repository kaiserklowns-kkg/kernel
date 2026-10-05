# ADR-0076: The desktop shell: taskbar, Start menu, windows that minimize

- Status: Accepted
- Date: 2026-10-05
- Amends: ADR-0057 (the desktop's layout), ADR-0059 (window placement)
- Part of Phase 10 (Alpha: a desktop people recognise).

## Context

The desktop of ADR-0057 was a working surface, not one people know their
way around:
- an "Oceans Bar" at the top;
- a fixed launcher panel on the left;
- the Terminal filling the rest, with app windows placed over its top
  left corner.

People who boot the alpha expect the conventions of Windows and macOS:
- a taskbar with what is open;
- a Start menu of apps;
- windows that can be put away and brought back.

## Decision

### The layout

A desktop in the manner of Windows 11, in the colours of the web experience
(ADR-0058).

- **The wallpaper:** a vertical gradient, deep water darker at the top.
- **The taskbar,** 48 pixels along the bottom.
  - **In the middle:** the Start button (the Oceans mark), the Terminal,
    then each open window in the order opened. Each shows its app's icon
    and a mark: a short grey bar for open, a wide accent bar for the
    keyboard focus.
  - **On the right:** memory use, the time and the date (UTC).
  - **A click on a window's button** brings it forward, or minimizes it if
    it is already in front with the keyboard. The same goes for the
    Terminal's button.
- **The Start menu,** above the Start button: the Terminal and the
  installed apps as tiles. Each tile is an icon (the app's initial on a
  colour chosen by its name) and its name. Running apps have a green dot.
  - A tile starts its app, with the permission dialog first if needed
    (ADR-0047).
  - A click outside closes the menu.
- **The Terminal** is a window in the middle of the screen. Its minimize
  button puts it away; the taskbar brings it back. The shell keeps running
  either way.
- **App windows** open in the middle of the screen, each a step (32
  pixels) after the last.
  - The frame the system draws has the app's verified name, its own title,
    and minimize and close buttons. A soft shadow sits underneath.
  - **A minimized window:**
    - is not drawn;
    - takes no clicks;
    - gives up the keyboard focus to the topmost window still shown, else
      the Terminal;
    - comes back on top, with the focus, from the taskbar or Ctrl+Tab.
- **Notifications** appear in the bottom right corner, above the taskbar.
- **System dialogs** (permissions, installs) appear in the middle of the
  screen, over a dimmed desktop.
- **The drawing:**
  - rounded corners, with the corner pixels softened;
  - gradients;
  - shadows that darken what is under them.

### Where the decisions live

- **The window manager** (`oceans_window::Manager`, host-tested):
  minimizing, restoring, centred placement.
- **The desktop** (`user/display`): the layout. Every position is a
  function of the screen's size, so the smoke test clicks and checks where
  the layout puts things.
- **The window protocol does not change.** Apps see focus events as before.

## Consequences

- The desktop looks and behaves as people expect, in the same colours as
  the web experience.
- **Not yet:**
  - **Resizing and maximizing windows:** an app draws into a buffer of the
    size it asked for, so this needs a protocol change (a resize event,
    and new shared memory).
  - **Text beyond Basic Latin** (Thai among it): the bitmap font has only
    ASCII.
  - **Pinned apps** on the taskbar.
  - **A search box** in the Start menu.
  - **Animations.**
  - **More than 14 apps:** the Start menu shows 14, and says to use
    `app list` for the rest.
- A minimized Terminal still has the keyboard when it had the focus: what
  is typed goes to the shell, as before.

## Alternatives considered

- **A macOS layout** (a menu bar at the top, a dock at the bottom): as
  feasible. The taskbar was chosen as the more common convention. Every
  position comes from a few functions, so the layout can change without
  touching the window manager.
- **Rendering the SvelteKit experience on the device:** that needs a web
  engine on Oceans, which is far bigger work (ADR-0056).

## Checklist (master spec §48)

- **Purpose:** a desktop people know how to use.
- **Architecture:**
  - minimizing in `oceans_window::Manager`;
  - the layout functions and drawing in `user/display` (`desktop.rs`);
  - rounded fills, gradients and shadows in `canvas.rs`.
- **API:**
  - `Frame::minimized`, `Frame::minimize_button`, `Manager::minimize`;
  - no protocol change.
- **Dependencies:** none.
- **Security:**
  - app names stay the ones Core verified;
  - the taskbar and dialogs are drawn by the system;
  - a modal dialog still takes every click and key.
- **Testing:**
  - unit (window manager): placement in the middle; minimized windows hide,
    pass the focus on, take no clicks, and come back on top;
  - smoke: the Start menu on screen; Hello started from it, with its
    permission dialog in the middle; Notes started from it, its window and
    the focus checked on screen captures; the install dialog and Tiles'
    window at their new places.
- **Failure behaviour:** a screen too small (under 900 x 420) keeps the
  kernel console, as before.
