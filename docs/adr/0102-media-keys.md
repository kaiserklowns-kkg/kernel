# ADR-0102: Media keys: HID consumer controls, and keys for the player

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0101 (the volume keys), ADR-0032 and ADR-0042 (USB
  keyboards and HID report descriptors), ADR-0094 (Music), ADR-0059
  (windows)
- Part of Phase 10 (Alpha: basic apps).

## Context

ADR-0101 read the volume keys a USB boot keyboard sends as keys. Most USB
keyboards do not: their media keys (volume, mute, play/pause, stop,
previous, next) are **consumer controls** (HID Usage Tables §15, page
0x0c), on a second HID interface or in a report of their own, described
by a report descriptor. Laptops and PS/2 keyboards send the media keys as
extended scancodes.

**What users expect, on every desktop:**
- **Windows:** the media keys control whatever plays (the System Media
  Transport Controls), focused or not; the volume keys the system's level.
- **macOS:** Play/Pause, Previous and Next go to the app playing ("Now
  Playing"), even in the background.
- **Linux (MPRIS):** the desktop sends them to the media player that is
  playing, or was last.
- **Everywhere:** the keys work from any app, and reach the player, not
  the window in front.

## Decision

**Reading consumer controls** (`oceans_usb::consumer`, the `xhci`
driver):
- `Layout::parse` finds the first Consumer Control application collection
  with a known key, as bits of their own (variable fields) or as values of
  an array (any usage the device names), with or without report IDs.
- `Keys` turns its reports into key bytes, one per key newly pressed:
  Mute, Volume Down and Up (ADR-0101's bytes), and four new ones:
  `KEY_PLAY_PAUSE` (0xa0), `KEY_STOP`, `KEY_PREVIOUS`, `KEY_NEXT`
  (0xa1–0xa3).
- The USB driver looks for such an interface on every device but hubs
  (not the boot keyboard or mouse, not the pointer), sets up its interrupt
  endpoint next to the keyboard's (two pages more per device), and types
  the bytes where the keyboard's go (`port 6.1: interface 1: media keys`).
- **PS/2:** `0xe0 0x22` Play/Pause, `0x24` Stop, `0x10` Previous, `0x19`
  Next.

**The media keys go to the player** (`op::MEDIA_KEYS` on the window end):
- An app asks for them for one of its windows; they come to it as key
  events, whatever has the focus, until another app asks. When the user
  focuses a window of an app that asked, they go to it again; when that
  window closes, to the one that asked before, or nowhere.
- A media key is no input of the app's (it allows no copy or open,
  ADR-0095, ADR-0099); the desktop logs where it went (`display: media key
  Play/Pause to Music`).
- **Music** asks when it starts: Play/Pause, Stop, Previous and Next do
  what its buttons do. Toolkit apps ask with `Ui::want_media_keys`.

## Consequences

- USB keyboards' media keys and laptops' media keys work; the volume
  keys too, sent either way.
- A song keeps playing under another app, and the keys still reach it.
- **Not yet:**
  - other consumer controls (browser keys, calculator, brightness);
  - consumer controls inside the pointer's interface (some mice);
  - "now playing" shown by the desktop;
  - QEMU has no USB keyboard with consumer controls: the smoke test
    reaches Music through QEMU's keyboard, and the descriptor reading is
    covered by host tests of real descriptors' shapes.

## Alternatives considered

- **Media keys to the focused window:** a player in the background would
  never get them; every other system sends them to the player.
- **The app that played sound last (asked of the audio service):** the
  audio service does not know windows or apps, and a notification sound
  would take the keys; asking is explicit.
- **A parser per device:** the report descriptor says where the keys are;
  one parser reads them all.

## Checklist (master spec §48)

- **Purpose:** media keys of USB keyboards, laptops and PS/2 keyboards;
  play, pause, stop and skip from anywhere.
- **Architecture:** `oceans_usb::consumer` (sharing the pointer parser's
  pieces); `MediaRing`, `find_media`, `start_media` in `xhci`; the PS/2
  scancodes in the kernel; `MEDIA_KEYS`, `Manager::want_media_keys`,
  `media_owner` in `oceans-window`; `Ui::want_media_keys`; Music.
- **API:** `display::KEY_PLAY_PAUSE`, `KEY_STOP`, `KEY_PREVIOUS`,
  `KEY_NEXT`; the window op `MEDIA_KEYS` (12).
- **Dependencies:** none.
- **Security:**
  - descriptors and reports come from the device and are parsed
    defensively (bounded fields, reports, usages);
  - an app gets media keys only by asking, and only for its own window;
    they never count as its user's input.
- **Testing:**
  - unit: keys as bits after a keyboard report, keys as an array's values,
    held keys sent once, descriptors without known keys or cut short
    (`oceans-usb`); routing to the player that asked or was looked at last,
    and when windows close (`oceans-window`);
  - smoke: Music asks for the media keys; Play/Pause and Stop pressed on
    QEMU's keyboard reach it while it is in the background of the shell.
- **Failure behaviour:**
  - **A descriptor that does not parse, or no known keys:** the interface
    is left alone.
  - **No app asked:** the media keys do nothing.
