# ADR-0151: Control Center you arrange

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0116 (each account's own settings), ADR-0118 (Control
  Center), ADR-0147 (the screen's brightness), ADR-0149 (the network
  button), ADR-0150 (Control Center in glass)
- Part of Phase 10 (the desktop experience).

## Context

ADR-0150 gave Control Center macOS's look, but its layout was fixed. The
user asked for it to be arranged as macOS lets it be: "Edit Controls…"
at its foot, a badge on each control to take it away, a gallery of the
others to add, and controls dragged into another order. Each account
should keep its own arrangement, as it keeps its dock, appearance and
keyboard layout (ADR-0116).

## Decision

### The controls

Each control has an id (`oceans_account::controls`) and a size on a grid
four cells wide, as macOS's is:

| Control | Size | Does |
|---|---|---|
| Network | 2 × 1 | the Wi-Fi panel, or Settings (ADR-0149) |
| Now Playing | 2 × 2 | the app's icon, the title; Previous, Play/Pause, Next |
| Dark Mode | 2 × 1 | dark on and off |
| Keyboard | 2 × 1 | the next layout |
| Lock Screen | 1 × 1 | locks |
| Settings | 1 × 1 | opens Settings |
| Display | 4 × 1 | the brightness slider, only where it can be set |
| Sound | 4 × 1 | the speaker mutes, the slider sets |
| Accent | 1 × 1 | the next accent |
| Launchpad | 1 × 1 | opens Launchpad |
| Notifications | 1 × 1 | opens the notifications |
| Terminal | 1 × 1 | shows the Terminal; offered only to the administrator |

The first eight are shown until the account chooses (`DEFAULT`).
`oceans_draw::layout::pack` places them in order, each in the first
place on the grid it fits, row by row, so a small control fills a gap a
wide one left. The panel is as tall as its rows, but never shorter than
the Wi-Fi panel needs (ADR-0130).

### Editing

- **Edit Controls…** at the foot turns editing on; it becomes **Done**.
  Closing Control Center ends it too.
- **Badges.** A grey badge with a minus at each control's corner takes it
  away.
- **The gallery.** A glass panel left of Control Center lists the
  controls not shown, each with its picture, its name and a green +; a
  click adds it at the end. **Reset** gives back the defaults.
- **Dragging.** A control pressed while editing is picked up (it fades)
  and the one under the pointer is ringed in the accent; let go there, it
  takes that one's place (`controls::moved`: before it when it came from
  after it, after it when it came from before).
- While editing, the controls do nothing else.

### Kept by Core, per account

Core op `CONTROLS` (49, a query) answers the account's ids in order, one
byte each, or `DEFAULT` while it has none or its file is damaged;
`SET_CONTROLS` (50, a decision, as the account's other settings are)
takes known ids, each at most once (`controls::valid`), and keeps them
in `/system/controls.<login>` as decimal ids. The file is one of the
account's own (`USER_FILES`): it goes with the account. The desktop
reads the list at start and when another account signs in.

### Glass that is not blurred again

Moving the pointer over Control Center redrew it, and with it blurred
what is behind it again. `Canvas::frost` now hashes what is under the
panel (FNV-1a) and takes the blur kept from last time when it is the
same, for up to four places (Control Center and the gallery).

## Consequences

- **Each account its own Control Center**, kept across reboots.
- **The smoke tests** follow the new places (what plays, the network,
  the keyboard, the sound) and edit: Edit Controls, Accent added from the
  gallery and taken away by its badge, Settings dragged onto Lock Screen
  and back, Done. The host tests check the list's checks and moves and
  the packing.

## Alternatives considered

- **Free placement on the grid** (a control stays where dropped, gaps
  allowed). macOS packs, and an order is all Core needs to keep.
- **Resizing controls** (macOS lets some be small or large). Later: each
  control has one size for now.
- **Keeping the arrangement in the display service.** It keeps nothing
  across reboots, and Core already keeps each account's settings.

## Checklist (master spec §48)

- **Purpose.** Control Center arranged as the user wants it.
- **Architecture.** Core keeps the list; the display service lays it out
  and edits it; `oceans_account::controls` is the list's rules,
  `oceans_draw::layout::pack` the packing.
- **API.** Core ops `CONTROLS` (49) and `SET_CONTROLS` (50). No ABI
  change.
- **Dependencies.** None.
- **Security implications.** `SET_CONTROLS` needs the right to decide,
  as the other settings do; it takes only known ids, and the file is
  the account's.
- **Testing strategy.** Host tests of `controls` and `pack`; the smoke
  tests' clicks, drags and log lines.
- **Failure behaviour.** A damaged or missing file gives the defaults; a
  refused change leaves Control Center as it was and says why.
