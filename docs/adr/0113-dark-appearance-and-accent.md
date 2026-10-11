# ADR-0113: The dark appearance, the accent, and one set of design values

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0105 (UI direction: one set of design values, a dark
  appearance, an accent), ADR-0078 (desktop look), ADR-0080 (app toolkit)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

The desktop and the toolkit each kept their own colour constants, light
only, in one blue. A user who works at night, or who wants their own
accent, had no choice, and a change of look meant editing both.

**What users expect, on every system:**
- **macOS:** Appearance Light, Dark (or Auto); an accent colour from a
  row of swatches; every app follows at once.
- **Windows:** Light or Dark for apps and for the system; an accent
  colour.
- **GNOME:** Default or Dark style; an accent colour (GNOME 47).
- **Everywhere:** chosen once in Settings, kept, and applied to the
  system's parts and its apps alike.

## Decision

### One set of design values (`oceans_draw::theme`)

- **`Theme`:** every colour the desktop and apps draw surfaces, text and
  controls with: window, sidebar, surface, text, muted text, accent (and
  pressed, and what goes on it), buttons, the selected row, lines, danger,
  success, and the desktop's own (the apps panel, dialogs, title bars,
  borders, idle window buttons).
- **Two appearances,** light (the colours Oceans always had, the blue
  accent's shades exact) and dark (light text on dark surfaces), and six
  **accents**: blue, purple, pink, orange, green, graphite. The selected
  row and the pressed accent follow the accent.
- **Each process draws with its theme** (`set`, `current`): the toolkit's
  `colour::text()` and the others, and the desktop's, read it. Colours
  are functions now, not constants: the theme changes while apps run.

### Kept by Core, followed by everything

- **Core:** `APPEARANCE` (op 34, `query`) → `[appearance u8][accent u8]`;
  `SET_APPEARANCE` (op 35, `decide`), kept in `/system/appearance`.
- **The desktop** reads it at start and every two seconds; on a change it
  draws in it (the menu bar and dock's glass, the panels, dialogs, title
  bars and borders) and tells every window.
- **The window protocol's `kind::THEME`** (event 8: `key` the appearance,
  `button` the accent) comes to each window when it opens, before its
  first frame, and whenever the theme changes. `oceans-ui` sets the app's
  theme from it and draws again: every app on the toolkit follows with no
  change of its own. Older apps, and Go apps, skip events they do not
  know.
- **Fixed whatever the theme:** the wallpaper, the Terminal (already
  dark), the window buttons' signal colours, app icons, and the lock
  screen's white field.

### Choosing it

- **Settings' General:** Appearance, Light or Dark; Accent, a button for
  each by its name with its colour under it, the one in use filled (the
  keyboard reaches them all). Settings becomes 1.4.0.
- **The shell:** `appearance [light | dark] [ACCENT]`.

## Consequences

- One place decides how Oceans looks; a dark appearance and six accents
  come from it for the desktop and every app.
- The smoke test chooses dark in the shell, sees the desktop's apps panel
  dark, and finds it kept after the reboot.
- **Not yet:**
  - following the time of day (Auto), and apps choosing their own;
  - the web experience following the device's theme;
  - Go apps' windows (`go/oceans/window` draws its own);
  - a dark wallpaper of its own, and high contrast;
  - more of ADR-0105's design values (spacing, corner radii and type
    sizes in the theme).

## Alternatives considered

- **Each app asking Core for the theme:** every app would hold a Core end
  and poll it; the window protocol already reaches every window.
- **Recolouring windows' pixels in the desktop:** apps draw images and
  text the desktop cannot tell apart; inverting would spoil them.
- **Keeping the constants and adding dark ones beside them:** every app
  would choose between two names at each use, and forget some.

## Checklist (master spec §48)

- **Purpose:** a dark appearance and the user's accent, everywhere at
  once.
- **Architecture:** values in `oceans_draw::theme`; Core keeps the choice;
  the desktop draws in it and tells windows; the toolkit follows.
- **API:** `oceans_draw::theme::{Theme, Appearance, ACCENTS, set, current}`;
  `oceans_ui::colour::*()`; `kind::THEME`, `Manager::set_theme`;
  `op::APPEARANCE`, `SET_APPEARANCE`, `ACCENT_COUNT`; the shell's
  `appearance`.
- **Dependencies:** none.
- **Security implications:** choosing needs the `decide` right; the event
  carries two bytes and nothing of anyone's; an app cannot change another
  app's look.
- **Testing strategy:** host tests for the theme's values, the process
  theme, and every window hearing of a change (and new ones at open); the
  boot smoke chooses dark and checks the desktop's pixels, and the next
  boot's start.
- **Failure behaviour:** a missing or damaged `/system/appearance` is
  light and blue; an app that ignores the event keeps drawing light.
