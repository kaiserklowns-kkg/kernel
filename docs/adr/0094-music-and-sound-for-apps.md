# ADR-0094: Music, and sound for apps

- Status: Accepted
- Date: 2026-10-08
- Depends on: ADR-0079 (sound), ADR-0087 (recording), ADR-0047 (what
  Core grants), ADR-0080 (the toolkit, apps that come with the system)
- Part of Phase 10 (Alpha: basic apps).

## Context

Only the shell could make sound (`play`, with `use:audio`). An app had no
way to ask for it. Handing apps the audio service's endpoint would not do:
since ADR-0087 it also opens capture sessions, so an app that may play
could also listen.

## Decision

**Players.** The audio service gains `PLAYER`. On its own (unbadged)
endpoint it returns a client end badged `PLAYER_BADGE`. On that end:
- `OPEN` opens playing sessions only;
- asking for a capture session, or `INPUT_INFO`, is refused;
- `INFO` answers as before.

Session badges count up from 1 and never reach the player badge.

**The `sound` permission** (`Permission::Sound`, number 9, "play sound"):
- **Granting:** Core asks the audio service for a player end when it
  starts an app that has it, and hands it over as `use audio`. Core
  therefore uses `audio`, so the audio service now comes before Core in
  `services.conf`.
- **Not asked:** like a window, it reaches only what the user sees or
  hears, and nothing comes back in (a player cannot record). Recording
  for apps, when it comes, is a permission of its own, and asked.

**`oceans-wav` (`libs/wav`)** is the WAV reading and the conversion to
48 kHz stereo that `play` had, made to stream:
- `Wav::parse` needs only a file's start;
- `Wav::render` converts any run of output frames from the source frames
  `Wav::source_range` names;
- it is host-tested, including that rendering in pieces equals rendering
  whole;
- `play wav` uses it now.

**Music** is an app on the toolkit, brought by the image, with `window`,
`files` and `sound`:
- **The songs:** the `.wav` files in Home and in Home's `Music` folder.
- **The controls:** Play/Pause, Stop, Previous, Next and Refresh; a click
  on the time bar goes there.
- **Playing on:** a song that ends goes on to the next.
- **A song by name:** started with one (`app start app.oceans.music
  NAME`), it plays it.
- **Streaming:** a song is read a piece at a time through a shared buffer,
  never held whole.
- **Never blocking the window:** on the toolkit's tick (every 100 ms) the
  queue is topped up to half a second. The driver's buffer grows from a
  third of a second to 1.4 s (256 KiB, as the input's): with 200 ms of
  lead, ticks made late by drawing the window still let it run dry.
  `STOP` drops what is queued, so Pause and seeking stay immediate. The
  buffer is never full, so `PLAY` never waits and the window never
  stalls.
- **The device's clock, not the app's:** how much is still queued comes
  from the audio service (`QUEUED`, new: the bytes written and not played
  on a playing session). An app clock drifts from the sound device's: a
  first version that estimated the queue from `clock_ms` left gaps of 10
  to 60 ms under emulation. The time shown comes from the same count.

**A fix in the sound driver (ADR-0079).** The driver zeroed the cyclic
buffer's free space only when a `PLAY` came. If a client stopped
feeding it without `DRAIN` (a song paused, an app busy), the controller
went on round the buffer and played again what it had already played: a
2 s tone was recorded as 3.9 s. Now:
- what the controller has played is made silence each time the driver
  looks at its position;
- while the output runs, a 50 ms timer on a notification bound to the
  endpoint (`endpoint_bind`) has the driver look between requests;
- an output that has played all it was given rests.

A starved output is therefore silent, never an echo. The smoke check
fails a tone that lasts longer than it was.

**A fix in Files (ADR-0082).** Files reopened Home as `.` to list it, and
`.` names nothing (`valid_name`), so the top of Home showed "This folder
cannot be opened". Its smoke step only looked at the toolbar. Files and
Music now list Home through the handle Core gave them, and open only the
folders inside it. The smoke clicks Files' first entry in Home and waits
for it to be selected.

## Consequences

- Apps can play sound and cannot listen; the system's own and
  third-party apps alike.
- A song is a few tens of KiB in memory however long it is.
- **Not yet:**
  - other formats than 16-bit PCM WAV (MP3, AAC, FLAC need decoders);
  - a volume control (the driver sets none);
  - mixing: two apps playing at once take turns in the driver's queue;
  - recording for apps.

## Alternatives considered

- **Giving apps the audio endpoint:** it records too (ADR-0087).
- **A second endpoint from the audio service (`provide = audio-play`):**
  init gives each service one `provide` per name. A badge on the one
  endpoint needs nothing new from init, and the driver already tells
  sessions apart by badge.
- **Asking before sound plays:** the user hears it at once and can stop
  the app; other systems do not ask either.
- **A thread that feeds the sound:** the toolkit's tick and a short lead
  keep the app single-threaded.

## Checklist (master spec §48)

- **Purpose:** sound for apps; a music player.
- **Architecture:**
  - `PLAYER` in `oceans-audio-proto` and `hda`;
  - `Permission::Sound` in `oceans-package` and Core;
  - `libs/wav` (`oceans-wav`), `user/apps/music`.
- **API:**
  - `op::PLAYER`, `PLAYER_BADGE`, `oceans_audio_proto::player`;
  - `op::QUEUED`, `Output::queued`;
  - the `sound` permission (also in the bridge's list);
  - `oceans_wav::Wav`.
- **Dependencies:** none.
- **Security:**
  - a player end cannot record or read about the input;
  - Music reads only `/home` (`files`).
- **Testing:**
  - unit: `oceans-wav` (the header, refusals, a file cut short,
    conversion, rendering in pieces), the permission list against the
    bridge's;
  - smoke: Music plays a 660 Hz WAV (44.1 kHz mono) from the host by
    name; its time bar fills and empties; the host finds a second of
    660 Hz in what QEMU recorded, besides the shell's 440 Hz tone.
- **Failure behaviour:**
  - **No sound output, or no player:** Music says it may not play sound.
  - **A file that is not 16-bit PCM WAV:** the reason is shown.
  - **A song that fails mid-way:** it stops with a message.
