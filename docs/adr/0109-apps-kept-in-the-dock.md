# ADR-0109: Apps kept in the dock

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0078 (the dock), ADR-0108 (search in the launcher),
  ADR-0100 (settings Core keeps across reboots)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

The dock showed the apps button, the Terminal and the open windows. An app
the user starts every day had to be found again each time, in the apps
panel or by search.

**What users expect, on every system:**
- **macOS:** apps kept in the Dock ("Keep in Dock"), a dot under those
  running; a click starts one or brings its windows forward; dragging it
  out, or "Remove from Dock", takes it out.
- **Windows 11:** "Pin to taskbar" from the Start menu or a running app's
  button (a right click); a pinned app's button holds its windows.
- **GNOME:** "Pin to Dash" from the overview or a running app.
- **Everywhere:** chosen by the user, kept across restarts, a right click
  away.

## Decision

### What the dock shows (`oceans_window::dock`, host-tested)

After the apps button and the Terminal:
- **The kept apps,** in the order kept; a dot under one with a window
  open.
- **Then the windows of the other apps,** in the order opened, as
  before. A kept app's windows have no icons of their own: they are
  behind its icon.
- At most eight apps are kept.

### Using it

- **A click on a kept app's icon:** its first window comes forward (or is
  minimized if it is already in front with the keyboard, as a window's
  icon is); with no window open, the app starts, as from its tile.
- **A right click** (the second button):
  - on an app's tile or search result in the apps panel, or on a window's
    icon: its app is kept in the dock;
  - on a kept app's icon: it is taken out.
  - A note says which ("Calculator kept in the dock"); a full dock says
    so.

### Kept by Core across reboots

- **`PINNED`** (op 25, the `query` right) → the kept apps' ids, one per
  line; installed apps only.
- **`PIN`** (op 26, the `decide` right, as the user's setting of the
  volume is): `[keep u8][id]` keeps an installed app or takes one out →
  the kept apps. Core writes them to `/system/dock` and logs it.
- An app removed is no longer listed; installed again, it is not kept
  again until the user says so.
- The desktop reads them at start and whenever apps come or go.

## Consequences

- An app used every day is one click away, after every reboot.
- **The smoke boot's limit becomes 900 s** (xtask and the kernel's self-test,
  from 600, ADR-0090): the debug build of the first boot, with the
  desktop's steps of ADR-0105 to ADR-0109, reached 560 s.
- The smoke test keeps Calculator by a right click on its search result,
  starts it from the dock, and finds it kept after the reboot.
- **Not yet:**
  - reordering kept apps by dragging, and dragging one out;
  - a menu on the right click (Windows' jump lists, macOS' Dock menu):
    the click acts at once;
  - apps kept in the dock when the system is first installed;
  - keeping apps from the keyboard.

## Alternatives considered

- **Kept by the desktop itself:** the desktop has no storage, and a
  restart of the display service would forget them; Core already keeps
  the user's settings.
- **Every window its own icon, kept apps apart** (Windows 7's "never
  combine"): two icons for one app, and the dock grows with each window.
- **A context menu with "Keep in Dock":** clearer for a first-time user,
  but a menu system of its own; the note after the click says what
  happened, and a second right click undoes it.

## Checklist (master spec §48)

- **Purpose:** the user's apps at hand in the dock, kept.
- **Architecture:** the dock's contents decided in `oceans_window::dock`;
  drawn and clicked in the desktop; kept by Core.
- **API:** `op::PINNED`, `op::PIN`, `MAX_PINNED`;
  `oceans_window::dock::{entries, first_window, toggle, encode, decode}`.
- **Dependencies:** none.
- **Security implications:** keeping an app needs the `decide` right, as
  the user's other settings do; only installed apps can be kept; starting
  one from the dock goes through its permission dialogs.
- **Testing strategy:** host tests for the dock's contents, toggling and
  the stored text; the boot smoke keeps an app, starts it from the dock
  and checks it across the reboot.
- **Failure behaviour:** without Core the dock shows no kept apps; a
  missing or damaged `/system/dock` is an empty dock; ids of apps no
  longer installed are skipped.
