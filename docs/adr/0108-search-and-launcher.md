# ADR-0108: Search in the launcher: apps and the files of Home

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0078 (the apps panel), ADR-0099 (opening files in
  apps), ADR-0105 (UI direction), ADR-0045 to ADR-0048 (Oceans Core and
  its rights)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

The apps panel showed fifteen tiles, and said to use `app list` for the
rest. Starting an app meant finding its tile with the mouse; opening a
file meant starting Files and walking to it. Nothing could be found by
typing its name.

**What users expect, on every system:**
- **Windows 11:** press the Windows key and type: the Start menu searches
  apps and files; Enter opens the best match.
- **macOS:** Command+Space opens Spotlight; typing finds apps and files;
  Enter opens; the arrows choose; Escape closes.
- **GNOME:** the Super key opens the Activities overview, whose search
  finds apps and files.
- **Everywhere:** a key, a few letters, Enter.

## Decision

### The key

**Super pressed and let go alone** sends `display::KEY_SEARCH` (0xa9),
from the PS/2 keyboard and the USB keyboard: not when another key was
pressed while it was held (Super+arrows arrange windows, ADR-0107). The
desktop takes it whoever has the focus: it opens the apps panel with an
empty search, or closes it. The Oceans mark and the dock's apps button
open the same panel.

### The panel searches

- **A search field** beside the panel's title. While the panel is open,
  what is typed goes into it, not to a window or the Terminal (the volume
  and media keys still work).
- **Empty:** the tiles, as before. **Typed:** the results in their place,
  one row each, up to nine:
  - the installed apps whose names match (up to four), with their icons;
  - the files of Home whose names match, with the folder they are in.
- **The arrows** choose a result; **Enter** opens the chosen one (the
  best at first); **Escape** closes the panel; a click opens a result.
- **Opening:** an app starts as from its tile (its permission dialog
  first if needed); a file opens in the app that opens its kind
  (ADR-0099), or a note says no app does. The desktop logs it
  (`desktop: opening picture.png with app.oceans.viewer, from search`).

### Matching (`oceans-search`)

`libs/search` (host-tested), shared by the desktop and Core: a name
matches when it holds every word of the query, case aside. The best come
first: the whole name, then its start, then the start of one of its words,
then anywhere inside; then the shorter name.

### Files come from Core (`op::FIND`)

The desktop has no access to files. Core holds Home and answers:
- **`FIND`** (op 24, the `query` right): `[query]` → the files of Home
  whose names match, best first, as paths joined by `\n`, as many as fit
  in a reply (up to eight).
- **Bounded:** it looks at 1000 entries at most, four folders deep, skips
  hidden names, and reads names only, never what files hold.
- The desktop asks from the query's second letter on.

## Consequences

- An app or a file is a key, a few letters and Enter away.
- Every installed app can be started from the panel, not only the first
  fifteen.
- The smoke test opens the search with Super, starts Calculator by "calc"
  and opens picture.png by "pict".
- **Not yet:**
  - apps kept on the dock (the next decision);
  - searching what files hold, settings, or the web;
  - an index: each search walks Home (bounded), which is slow for a very
    large one;
  - recent files and suggestions before anything is typed;
  - typing Thai into the search (the keyboards send ASCII only).

## Alternatives considered

- **Giving the desktop the filesystem:** the desktop would hold every
  file; Core answering with names only keeps it to what search needs.
- **A search window of its own** (Spotlight): a second launcher; one panel
  for tiles and search is what Windows and GNOME do.
- **Ctrl+Space or Alt+Space:** taken by input methods and window menus
  elsewhere; Super alone is what Windows and GNOME use.
- **An index kept up to date by Core:** worth it once Home is large;
  bounded walks are enough now and need no new state.

## Checklist (master spec §48)

- **Purpose:** finding and opening apps and files by typing.
- **Architecture:** the desktop draws and routes; `oceans-search` ranks;
  Core walks Home.
- **API:** `display::KEY_SEARCH`; `op::FIND` (`query` right);
  `oceans_search::{rank, best, file_name}`.
- **Dependencies:** none outside the repository.
- **Security implications:** `FIND` gives names in Home, never contents,
  and only to a holder of the `query` right (the desktop, the shell); it
  is bounded, so a query cannot keep Core busy; what is typed into the
  search never reaches an app; opening still goes through the permission
  dialogs.
- **Testing strategy:** host tests for the ranking and the USB Super key;
  the boot smoke opens the search, starts an app and opens a file by name.
- **Failure behaviour:** without Core, or if `FIND` fails, the search
  shows apps only; nothing found says so; a file no app opens says so.
