# ADR-0120: Icons on the desktop

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0117 (Wave, Launchpad), ADR-0109 (apps kept), ADR-0082
  (Files)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

With Wave closed and no window open, the desktop was the wallpaper alone.
The user asked to try apps on the desktop itself, as PCs have them.

**What users expect, on every system:**
- **macOS:** the desktop's icons down its right side, from the top: the
  disks, then files and folders; a click chooses, a double click opens.
- **Windows:** shortcuts down the left side; the same clicks.
- **Phones:** the home screen is apps; nothing sits under windows.

## Decision

- **What is on it:** **Home** (a blue folder; it opens in Files), then the
  **apps kept in Wave** (ADR-0109), in their order, at most six. Keeping
  an app in Wave puts it here too, so there is one list to look after.
- **Where:** a column down the desktop's right side under the top bar,
  as macOS has it: each a 48-pixel picture with its name under it, in
  white on a soft shade so it reads on the photograph; under every window
  and the Terminal.
- **Clicks:** one chooses an icon (lit), a double click opens it (`desktop:
  opening Calculator, from the desktop`), a click on the wallpaper lets the
  choice go.

## Consequences

- The desktop has something to start from even with Wave closed.
- The smoke test sees Home's folder and double-clicks the kept
  Calculator, which starts.
- **Not yet:** files and folders on the desktop (a Desktop folder in
  Home), arranging the icons, adding an app to the desktop alone, and the
  keyboard reaching them.

## Alternatives considered

- **Every installed app:** Launchpad already shows them all; the desktop
  would fill.
- **A list of its own:** a second list to keep beside Wave's, for the
  same apps.

## Checklist (master spec §48)

- **Purpose:** apps to start from the desktop itself.
- **Architecture:** `Desktop::desktop_items` (Home and the kept apps),
  `desktop_icon` (where), `Hit::DesktopIcon`, `icon_chosen`; the display
  service's click passes on a double click.
- **API:** none new to apps.
- **Dependencies:** none.
- **Security implications:** none: the same start as Wave's and
  Launchpad's, through Core, which asks about permissions as before.
- **Testing strategy:** the boot smoke checks Home's colour and starts
  Calculator with a double click on its icon.
- **Failure behaviour:** without Files, no Home; without kept apps, Home
  alone.
