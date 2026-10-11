# ADR-0117: Wave, Oceans' own taskbar, and Launchpad

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0078 (desktop look), ADR-0105 (UI direction), ADR-0108
  (search), ADR-0109 (apps kept), ADR-0112 (Lock), ADR-0115 (several
  users)
- Supersedes: the dock and the apps panel of ADR-0078 and ADR-0108.
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

After using the desktop running, the user asked for a new direction
(2026-10-11): keep what suits a PC (the top bar, floating windows), take
from phones what they do well (the lock screen, Control Center,
notifications, app cards), and give Oceans a **taskbar of its own**:
simple, uncluttered, unlike any other system's, yet easy.

Three taskbars were drawn first and turned down: capsules naming what
each app does, cards for groups of windows, and icons shrinking with
age. All three were still a shelf of things along the bottom, and busy.

**What the research says** (sources in the conversation that chose
this):
- **Fitts's law:** a target on the screen's edge is reached fastest; the
  middle of the bottom edge is where a pointer lands without aiming.
- **Hidden bars:** a bar that shows when the pointer touches a whole edge
  opens by accident; a small target that opens it does not.
- **macOS:** the Dock (a shelf of icons, magnified under the pointer) and
  Launchpad (every app in a grid across the screen, with search).
- **Windows 11, GNOME:** a full-width taskbar; an overview to start apps;
  both criticised for clutter or for an extra step to switch.

## Decision

### Wave

- **Closed** (almost always): one round glass button, 48 pixels, at the
  bottom's middle, with the Oceans logo and a dot for each app with a
  window open (and the Terminal), up to five. Nothing else: the desktop
  is the wallpaper and the windows.
- **Open**, while the pointer is on the button or its icons: the
  Terminal, the apps kept (ADR-0109) and the open windows rise over the
  button in a row that **crests in the middle like a wave**; a short bar
  under what is open, the name over the one pointed at, a minimized
  window faded. The button lights in the accent. Leaving the icons
  closes it; Launchpad, a dialog or the lock screen close it too.
- **The same actions** as the dock: a click shows or minimizes a window,
  starts a kept app, shows the Terminal; a right click keeps an app or
  takes it out ("Calculator kept in Wave").
- Windows keep the bottom band they had (80 pixels): the button never
  covers one; open, the icons lie over windows, and clicks there are
  Wave's.

### Launchpad

- **Wave's button or Super** opens it: the apps in a
  grid of seven columns across the screen under the top bar (the
  Terminal first, up to twenty apps), a running app marked, on an opaque
  surface in the appearance's colour so it reads at a glance.
- **The search** (ADR-0108) at the top in the middle: typing finds apps
  and the files of Home, in rows under the field.
- **Lock, Restart, Shut Down** in the bottom right corner; the count of
  apps in the bottom left.
- A click between the icons, Escape or the button again closes it.

### The top bar, the window buttons, the logo

Seen running, the user asked for three more changes, made here:

- **The top bar is clear:** no band of its own, the wallpaper through it
  under a soft shade, in white. The **time and date at the left**, as
  phones show them (the notifications will open from it); at the right,
  from the end in, the system's state, the speaker, the keyboard layout
  and the app in use. No logo: it is Wave's.
- **The window buttons at the right end** of the title bar, round as
  before: zoom (green), minimize (yellow), close (red) in the corner,
  where a PC's user looks for it; the Terminal's too. The title stays
  centred, clear of them.
- **The Oceans logo** (the wave in a circle, from the project's logo
  without its words) is Wave's button: 40 pixels on the glass, a ring in
  the accent when lit, the dots for open apps under it. Kept as
  `user/display/assets/oceans-logo.png` (512 pixels, its black made clear)
  and drawn from a 40-pixel copy (`Canvas::image`).

### The wallpaper

- **A photograph of the ocean,** chosen by the user among CC0 pictures on
  Wikimedia Commons: "Ocean and waves" by Esther Cuan (Unsplash), its
  1920 × 1280 copy kept as `user/display/assets/wallpaper-ocean.png`
  (credited in `CREDITS.md`). The display service decodes it once at
  start (`oceans-image`), cuts the screen's shape from its middle and
  scales it to fill any screen. The drawn ocean of ADR-0078 stays for a
  picture that cannot be read.

### Still to come (the rest of the design, each its own ADR)

- The lock screen with a large clock, the accounts and the password,
  opened with Enter or by dragging up.
- The top bar's right: Control Center (round toggles, media, keyboard
  layout, appearance, volume) from the status icons, and a phone-like
  notification centre from the clock.
- Recents: a card for each open app, two in Split View together; a click
  returns, dragging up closes.

## Consequences

- The desktop shows only the top bar, the windows and one small button;
  everything else is a pointer's move away.
- The smoke test opens Launchpad from Wave's button, starts apps from its
  grid, keeps Calculator and starts it from Wave's crest, and locks from
  Launchpad's corner.
- **Not yet:** choosing another wallpaper, the battery and network in the
  top bar (with Control Center, which needs the same), pages in Launchpad
  past twenty apps (the Terminal's `app
  list` names the rest), arranging its icons, Wave on other edges, and a
  keyboard way into Wave's icons (Super+Tab will come with Recents).

## Alternatives considered

- **A full-width taskbar or a dock:** what every other system has, and
  what the user turned down.
- **Opening Wave at the whole bottom edge:** reached by accident when
  moving to a window's bottom; the button is a small, sure target.
- **A translucent Launchpad:** the windows behind showed through and made
  it busy in the dark appearance.

## Checklist (master spec §48)

- **Purpose:** a taskbar of Oceans' own that keeps the desktop clear, and
  every app a click away.
- **Architecture:** the display service draws Wave and Launchpad as
  functions of the screen's size (`wave_button`, `wave_item`,
  `wave_zone`, `start_menu`, `tile`, `search_field`, `power_button`) and
  of `Desktop::wave_open`, which the pointer sets (`track_wave`).
- **API:** none new to apps.
- **Dependencies:** none.
- **Security implications:** none: the same actions as the dock and the
  apps panel, through the same paths; Wave shows only what the dock did.
- **Testing strategy:** the boot smoke clicks Wave's button, Launchpad's
  tiles and corner buttons, and Wave's icons at their computed places,
  and checks Launchpad's colour in both appearances.
- **Failure behaviour:** with no apps, Launchpad shows the Terminal and
  says so; Wave with nothing open shows only the Terminal.
