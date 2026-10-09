# ADR-0101: The volume keys

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0100 (the system volume), ADR-0029 (PS/2), ADR-0032
  (the USB keyboard), ADR-0059 (the keyboard focus)
- Part of Phase 10 (Alpha: basic apps).

## Context

ADR-0100 gave Oceans one system volume, set from the menu bar or the
shell, and left the volume keys for later. Nearly every keyboard has
them, and on a laptop they are the usual way (Fn with F-keys).

**What users expect, on every desktop:**
- **Windows:** Mute, Volume Down and Up change the level by 2 and show it
  in a flyout; Up or Down while muted unmutes.
- **macOS:** steps of one sixteenth, shown over the screen; Mute toggles;
  Up unmutes.
- **Linux (GNOME, KDE):** steps of 5 %, an on-screen display; Up and Down
  unmute.
- **Everywhere:** the keys work whatever has the focus, and no app sees
  them.

**How keyboards send them:**
- **PS/2, and laptops' built-in keyboards** (set 1, through the 8042):
  `0xe0 0x20` Mute, `0xe0 0x2e` Volume Down, `0xe0 0x30` Volume Up; the
  laptop's controller turns Fn plus the key into these.
- **USB keyboards:** the boot keyboard's own usages `0x7f` Mute, `0x80`
  Volume Up and `0x81` Volume Down; many keyboards send them as consumer
  controls instead (usage page 0x0c), on an interface of their own.

## Decision

**Three key bytes** (`oceans_abi::display`): `KEY_MUTE` (0x8c),
`KEY_VOLUME_DOWN` (0x8d), `KEY_VOLUME_UP` (0x8e), sent by the PS/2 driver
for the codes above and by the USB boot keyboard for its usages, with or
without Shift or Ctrl.

**The desktop's, always:**
- The window manager hands them to the desktop whoever has the focus
  (`KeyRoute::Volume`): never to an app or the Terminal, and a press is no
  input of an app's (it does not allow a copy or an open).
- They work while a permission dialog asks, when other keys go nowhere.

**What they do** (`VolumeKey::apply`):
- Up and Down move the level by 5 (to 0 and 100 at most) and unmute;
  Mute turns mute on or off.
- The desktop sets it through Core, which applies and keeps it (ADR-0100),
  and shows the level for a second and a half above the dock: a speaker
  and a bar.

## Consequences

- The volume keys of PC keyboards and laptops work.
- **Not yet:**
  - HID consumer controls (a USB keyboard's media keys on their own
    interface), and the play, pause, next and previous keys;
  - a key held down repeating;
  - brightness keys.

## Alternatives considered

- **Letting the focused app have them:** a player could then turn the
  system up; other systems keep them for the system.
- **Steps of 2 (Windows) or a sixteenth (macOS):** 5 reaches silence in
  20 presses and is a round number on the panel's slider.
- **Steps on the gain, not the level:** the level is already heard as
  even steps (ADR-0100's curve).

## Checklist (master spec §48)

- **Purpose:** the keyboard's volume keys change the system volume.
- **Architecture:** the key bytes in `oceans-abi`; the PS/2 driver
  (kernel) and the USB boot keyboard (`libs/usb`); `KeyRoute::Volume`
  and `VolumeKey` in `oceans-window`; the desktop's `volume_key` and the
  level shown.
- **API:** `display::KEY_MUTE`, `KEY_VOLUME_DOWN`, `KEY_VOLUME_UP`.
- **Dependencies:** none.
- **Security:** the keys reach only the desktop, which sets the level
  through Core with the user's authority (ADR-0100).
- **Testing:**
  - unit: the USB usages and their bytes, held keys sent once
    (`oceans-usb`); routing to the desktop whoever has the focus, no app
    input; steps, the ends, unmuting (`oceans-window`);
  - smoke: Volume Up, Down and Mute pressed on QEMU's USB keyboard move a
    muted 75 % to 80 %, back to 75 % and muted again, as Core, the driver
    and `volume` report.
- **Failure behaviour:**
  - **No sound device:** the keys do nothing.
  - **Core refuses:** a notification says why; the level stays.
