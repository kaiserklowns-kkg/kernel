# ADR-0139: The system's icons

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0078 (the desktop's look), ADR-0117 (Wave and
  Launchpad), ADR-0118 (Control Center), ADR-0136 (outlines, the top
  bar's icons)
- Part of Phase 10 (the desktop experience).

## Context

ADR-0136 drew the top bar's status icons from outlines. Everything else
still looked like a sketch:

- **Apps' icons.** In Launchpad, Wave, the desktop, search results and
  notifications, every app had the same coloured square with its initial
  ("A", "C", "S"). macOS and Windows show each app as a picture of what
  it does: a gear for settings, a folder for files, a calculator's keys.
- **Control Center.** Dark, Lock and Settings were circles laid over
  circles in the panel's colour, a moon cut out by painting over it.
- **Media.** Play, pause and the skip buttons were stacks of one-pixel
  rows.
- **Spoofing.** The icon's colour came from the app's name, so any app
  could choose a name to look like another.

## Decision

### Apps' icons, as macOS and Windows draw them: `oceans_draw::apps`

Each app that comes with the system has its own icon, drawn from outlines
on a 64-unit grid:

- **The tile.** A rounded square (the radius near a quarter of its side, as Big
  Sur's) with a faint edge, its colour going lighter towards the top. The
  light falls on it as on macOS's and Windows 11's tiles
  (`Surface::fill_path_gradient`).
- **The pictures.**

  | App | Picture |
  |---|---|
  | Terminal | A green prompt, `>_`, on a dark tile |
  | Settings | A gear in a gear on grey, as System Settings has it |
  | Files | A white folder on blue |
  | Calculator | Its keys, the operators down the right in orange |
  | Text Editor | A page of lines with a pencil across it |
  | Image Viewer | A photograph of hills under the sun, on blue |
  | Music | Two notes beamed together, on red |
  | Activity Monitor | A graph's green line over its grid, on dark |

- **By id, not by name.** An icon is chosen by the app's id as Core
  verified it (`apps::for_app`), never by its name. Only the system's own
  `app.oceans.*` ids have one.
  - Another app named "Settings" keeps the tile with its initial, so it
    cannot dress as the system's.
  - For that, the windows' apps (`Owner.id`, `WindowView::app_id`) and
    notifications (`Notice::app_id`) now carry the id.
- **Where they show.** Launchpad, Wave, the desktop's icons, Launchpad's
  search results and the notifications.

### Control Center and the player in outlines: `oceans_draw::icon`

As SF Symbols and Fluent icons draw them:

- **Dark.** A crescent moon: the arc of one circle that another leaves,
  cut where they cross, not painted over.
- **Lock.** A padlock with its keyhole cut out.
- **Settings.** A gear of eight tapered teeth with a hole.
- **Media.** Play (rounded), Pause, Next and Previous (two triangles and a
  bar).
- **Back.** A chevron for the Wi-Fi panel.

Holes are holes (ADR-0136's outlines), so they show what lies under them
in either theme.

## Consequences

- **Recognition.** System apps are recognised at a glance in every place
  an app is shown, and stay sharp at any scale.
- **Other apps.** They keep their initial on a coloured tile.
  - **Not yet:** an app's own icon from its package, its manifest naming
    a picture. That is its own milestone: the package format and Core
    would carry it, and the desktop would draw it bounded and decoded
    safely.
- **The smoke tests.** Their probes do not fall on icons; the icons' shapes
  are host-tested.
  - **Tests:** every app icon fills its tile with more than one colour at
    100 % and 200 %; ids decide; Control Center and media icons draw.

## Alternatives considered

- **Bitmap icons per size.** They need a set per scale and a tool to make
  them; outlines are a few lines each.
- **Choosing by name.** It is spoofable, as above.
- **Copying macOS's or Windows' artwork.** It is theirs; these are drawn
  for Oceans in the same language.

## Checklist (master spec §48)

- **Purpose.** Icons people recognise, as on the systems they know.
- **Architecture.** Outlines in `oceans-draw` (`apps`, `icon`); the
  desktop chooses by verified id.
- **API.**
  - `apps::{App, for_app, draw}`;
  - `icon::{moon, lock, gear, play, pause, skip, back}`;
  - `Surface::fill_path_gradient`.
- **Dependencies.** None new.
- **Security implications.** An app cannot take a system app's icon: icons
  follow Core's verified id, not the name an app gives itself.
- **Testing strategy.** Host tests as above; the smoke tests' probes,
  unchanged.
- **Failure behaviour.** An id with no icon gets the initial's tile.
