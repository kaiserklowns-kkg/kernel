# ADR-0100: The system volume

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0079 (sound), ADR-0094 (players), ADR-0096 (reader
  ends), ADR-0045 (Core), ADR-0078 (the menu bar)
- Part of Phase 10 (Alpha: basic apps).

## Context

Sound played at the level its program wrote, and nothing could make it
quieter: ADR-0079 and ADR-0094 left "a volume control" for later. Music
(ADR-0094) made the gap obvious.

**What users expect, on every desktop:**
- **Windows:** a speaker in the taskbar; a click shows a slider, with
  mute; the volume keys; per-app levels in the Volume Mixer; the level
  kept across restarts.
- **macOS:** Sound in the menu bar and Control Center: a slider; the
  keys; kept across restarts; no per-app levels.
- **Linux (GNOME, KDE, PipeWire):** a slider in the top bar's menu, mute,
  the keys; per-app levels in Settings; kept.
- **Everywhere:** one level for everything, one click from anywhere,
  remembered.

**What matters for Oceans:** an app must not turn the sound up behind the
user's back, so the level is the user's, set through the system, never
through a player end.

## Decision

**The driver applies it** (`oceans_hda::volume`, `user/hda`):
- A level of 0 to 100 and mute, applied to every sample played before it
  reaches the controller: the gain is the level squared (half the level
  is a quarter of the power, about 12 dB down), none when muted. The same
  on every codec, whatever amplifiers it has.
- It applies to what is played from then on; a third of a second already
  queued plays at the old level.
- `op::VOLUME` answers on every end and session (players and readers too:
  Settings may show it); `op::SET_VOLUME` only on the driver's own,
  unbadged end, which only Core holds (ADR-0094). The driver logs each
  change (`hda: volume 40%`).
- Until Core sets it, sound plays as written (100%).

**Core keeps it** (`op::VOLUME`, `op::SET_VOLUME` on the `core` endpoint):
- Setting it needs the `DECIDE` right, as the user's decisions do: the
  shell and the desktop have it, apps and agents do not.
- Core writes it to `/system/volume` (`70`, or `70 muted`) and logs it
  (`core: volume 70%`); at boot it sets the driver to what it kept
  (`core: volume 70%, kept from before`).

**The user sets it:**
- **The menu bar:** a speaker left of the system's state, in one place
  (the clock and the state have slots of their own), drawn with one to
  three bars for the level, or a cross when muted. A click opens the sound
  panel: the level, a slider (a click or a drag; set when the button comes
  up) and Mute or Unmute. A press elsewhere closes it.
- **The shell:** `volume`, `volume 0-100` (which also unmutes, as volume
  keys do), `volume mute`, `volume unmute`.
- No speaker without a sound device.

## Consequences

- One level for everything, set by the user, remembered.
- Apps cannot change it: a player end only plays.
- **Not yet:**
  - volume keys (the boot keyboard has none; HID consumer controls are a
    later decision);
  - per-app levels (mixing, ADR-0094's note, comes first);
  - the codec's own amplifiers (quality at very low levels);
  - an on-screen indicator when it changes from the shell.

## Alternatives considered

- **The codec's amplifier (`SET_AMP_GAIN_MUTE`):** codecs differ in their
  steps and ranges, and some outputs have none; software gain behaves the
  same everywhere and is host-tested. The amplifier can come later, under
  this same API.
- **A linear gain:** half the slider would sound almost as loud as full;
  the square is closer to what ears hear, as other systems' curves are.
- **The desktop keeping the level:** it has no storage, and the shell
  would set another; Core already keeps the user's decisions on disk.
- **A `volume` permission for apps:** no app needs to set the system's
  level; a player's own samples are its volume.

## Checklist (master spec §48)

- **Purpose:** let the user make sound quieter, mute it, and keep the
  level.
- **Architecture:**
  - `oceans_hda::volume::{gain, apply}`; the driver's `copy_scaled` and
    `set_volume`;
  - `oceans_audio_proto::{volume, set_volume}`, `MAX_VOLUME`;
  - Core: `volume_now`, `set_volume`, `restore_volume`, `/system/volume`;
  - the desktop: the speaker, the sound panel; the shell's `volume`.
- **API:** audio `VOLUME` (11), `SET_VOLUME` (12); Core `VOLUME` (22),
  `SET_VOLUME` (23).
- **Dependencies:** none.
- **Security:**
  - only Core's end of the driver sets the level, and only holders of
    `DECIDE` ask Core;
  - every change is logged.
- **Testing:**
  - unit (`oceans-hda`): the gain curve and its ends, scaling both signs,
    full volume untouched, mute is silence;
  - smoke: a fresh disk's level is 100%; `volume 40` reaches the driver
    and Core; the panel's slider sets 75% and Mute mutes it (the panel
    and the lit Mute button on the screen); `volume 70` before the reboot,
    and the next boot starts at 70%.
- **Failure behaviour:**
  - **No sound device:** no speaker; `volume` says Core found none.
  - **The file cannot be written:** the level is set but not kept; the
    error is returned.
  - **A bad request:** refused; the level stays.
