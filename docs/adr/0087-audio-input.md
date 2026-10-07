# ADR-0087: Sound input: recording from HD Audio

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0079 (HD Audio output, the audio protocol)
- Part of Phase 4 (Hardware: audio), done during Phase 10.

## Context

ADR-0079 gave Oceans sound output and left input for later. Every Tier 1
PC has a microphone or a line-in jack on its HD Audio codec. Recording is
the mirror of playing: an input converter (ADC) behind a path of widgets
from an input jack, an input stream descriptor, and a cyclic buffer the
controller writes and the driver reads.

QEMU emulates codecs with inputs (`hda-duplex`, `hda-micro`). Its `wav`
backend, which the smoke test uses to check what is played, cannot
record.

## Decision

### `oceans-hda` (host-tested)

- **Jacks:** a pin's configuration now also tells line in (device 0x8)
  and a microphone (0xA). `input_preference` puts a microphone first,
  then line in. `pin_can_input` reads `PIN_CAPS` bit 5.
  `pin_input_control` turns the input on, and gives a microphone its
  80 % bias voltage when the pin offers it.
- **`find_input`:** for each input pin by preference, and each input
  converter, a depth-first search of at most six widgets. It goes from
  the converter through mixers and selectors to that pin, against the
  flow of the sound, as connection lists are written. The output search
  now shares the same search, with a goal and a "through" rule.
- **`Capture`:** the input's counterpart of `Ring`. It keeps running
  totals of bytes captured (from the position register) and taken. What
  is not taken before the controller comes round again is dropped,
  counted, and the oldest goes first.

### The driver

- **Codecs:** each codec is read once (`read_codec`). The output goes on
  the first codec with an output path, as before, and the input on the
  first with an input path. These are often the same codec, but not in
  QEMU's test setup.
- **The input path is set up** like the output path:
  - every widget is powered;
  - selectors are switched;
  - input amps (a mixer's at the chosen input, a microphone boost) are
    unmuted at 0 dB;
  - the pin is set to input.
- **The first input stream** (tag 2) has its own buffer descriptor list
  in the command page. It uses a 256 KiB cyclic buffer (1.4 s) at the
  same fixed format, 48 kHz 16-bit stereo. Programming a descriptor is
  now one function for both directions.
- **It runs** from a capture session's first `RECORD` until `STOP` or
  until the session closes. If its position does not move for a second,
  the input is stuck: it stops, and `RECORD` answers `IoError`.
- No input on the machine: the driver says so once (`hda: no input`).
  Output is unaffected.

### The protocol

- **`OPEN`** takes one data byte, `open_flags::CAPTURE`. The client's
  memory must then also carry `WRITE`, and the driver maps it writable.
  - There is one capture session at a time; another gets the new `Busy`
    status ("another program is recording").
  - A playback session's buffer stays read-only to the driver.
- **`RECORD [offset][len]`** (capture sessions only) is answered once
  those bytes hold what came in next.
- **`STOP`** on a capture session stops the input.
- **`INPUT_INFO`** describes the input in words.
- **The client side:** `oceans_audio_proto::Input` (`open`, `record`,
  `buffer`, `stop`) and `input_info`.

### `play`

- `play info` also names the input.
- `play record PATH [SECONDS]` (1 to 60, default 5) writes a 48 kHz
  16-bit stereo WAV file and reports the peak level. It needs
  `use:audio` and `use:fs`.

## Consequences

- Oceans records from a PC's microphone or line in.
- **The smoke test** adds QEMU's `hda-micro` codec at address 1, on the
  `none` backend. That backend delivers silence at the real rate, so the
  boot checks:
  - that the driver found the microphone;
  - `play info`;
  - the missing-`fs` refusal;
  - a 2 s recording of exactly 384,000 bytes with a 0 % peak.

  `smoke-hw` checks that the microphone is found on the production
  manifest. `cargo xtask run` on Windows records from the host's
  microphone (DirectSound).
- **Limits:**
  - **Only silence is tested:** QEMU cannot feed a known signal into a
    codec, so the signal path is checked by length, pace and level.
    What a real microphone sounds like needs a real machine.
  - **No privacy permission yet:** only the shell holds `use:audio`, as
    before. No app can record. A `microphone` app permission, asked in a
    system dialog and visible while it records (master spec §24), comes
    with app access to audio.
  - **No input volume, no jack detection, no choice of input:** 0 dB on
    the preferred pin.
  - **One recorder, no mixing**, as for output.
  - **Long-form connection lists** are still not read, so a few codecs'
    inputs stay out of reach, as for outputs.

## Alternatives considered

- **Testing with `hda-duplex` on the `wav` backend:** QEMU's `wav` backend
  has no capture, so the input stream would never move.
- **A separate input service:** the controller is one PCI function and
  one driver must own it. Input and output share its link and command
  rings.
- **Pushing captured sound to the client (a notification per period):**
  needs interrupts the kernel does not deliver for HD Audio (MSI, not
  MSI-X; ADR-0021). Polling while a `RECORD` waits matches the output
  side.

## Checklist (master spec §48)

- **Purpose:** sound input on Tier 1 hardware.
- **Architecture:**
  - `libs/hda`: input jacks, `find_input`, `Capture`;
  - the driver's input stream;
  - the protocol's capture sessions;
  - `play record`.
- **API:**
  - `OPEN` with `CAPTURE`, `RECORD`, `INPUT_INFO`, the `Busy` status;
  - `oceans_audio_proto::{Input, input_info}`;
  - `play record`.
- **Dependencies:** none new.
- **Security:**
  - recording needs `use:audio`, which only the shell holds;
  - a capture session's memory must be given writable by its owner, and
    the driver writes only within it;
  - there is one recorder at a time;
  - playback buffers stay read-only to the driver.
- **Testing:**
  - unit: input jacks and pin control, input paths (QEMU's duplex codec
    and a laptop-like codec with selectors), unreachable inputs, the
    capture accounting with wrap and overrun;
  - smoke: a timed recording of the right length;
  - `smoke-hw`: the microphone found.
- **Failure behaviour:**
  - no input: `IoError`, said once at start;
  - a stuck input: stopped, `IoError`, logged;
  - sound not taken in time: dropped, the oldest first, counted and
    logged when the session stops;
  - a second recorder: `Busy`.
