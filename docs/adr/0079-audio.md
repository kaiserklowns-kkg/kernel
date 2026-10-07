# ADR-0079: Sound: Intel High Definition Audio

- Status: Accepted
- Date: 2026-10-06
- Depends on: ADR-0021 (userspace drivers, DMA), ADR-0016 (services)
- Part of Phase 4 (Hardware: audio), done during Phase 10.

## Context

The master spec's Phase 4 lists audio among the hardware Oceans drives.
Nothing played sound. HD Audio (Intel, 2004) is the sound controller of
nearly every PC since: one PCI function (class 04, subclass 03) and one or
more codecs on its link. The Tier 1 machines (ADR-0068) all have one.
QEMU emulates it (`intel-hda`, with `hda-output` or `hda-duplex`) and can
write what it plays to a WAV file, so the driver can be tested end to end.

## Decision

### The driver: `hda`, a userspace service

- **Its authority:** one controller (`grant = device-class:040300`), the
  endpoint it serves (`provide = audio`) and a log.
- **The controller:**
  - BAR 0 is mapped;
  - the controller is reset, and the codecs that answer are found;
  - commands go to the codecs through the CORB/RIRB rings, polled, as
    every driver does on real hardware;
  - the response status is cleared after every answer: at the response
    count, some controllers (QEMU's among them) stop reading commands until
    it is.
- **The output:** the first codec with an audio function. Its widgets
  (converters, mixers, selectors, pins) are read, and the path is chosen
  by `oceans_hda::find_output` (host-tested):
  - **which pin:** one that can drive an output and is wired to a speaker,
    then line out, then headphones;
  - **which path:** the first of at most six widgets back to a converter;
  - **setting it up:** every widget on it is powered, switched to the path
    and unmuted at 0 dB; the pin drives its output (and an external
    amplifier, where it has one).
- **One stream,** the first output stream, in one format: 48 kHz, 16-bit,
  stereo.
  - The samples come from a 64 KiB cyclic buffer in DMA memory, a third of
    a second, described by two buffer descriptors.
  - The stream runs while something plays. What has not been written yet
    is silence, so a client that pauses hears nothing rather than old
    sound.
- **Polling:** the kernel delivers only MSI-X (ADR-0021) and HD Audio
  offers MSI or pin interrupts. The position register is read while a
  client waits for room, sleeping 2 ms between looks.
- **No controller, no codec or no output:** the driver logs it once and
  answers every request with `IoError`.

### The audio protocol (`oceans-audio-proto`)

The same session pattern as disks (ADR-0021).
- **`OPEN`:** sends a memory object; the answer is a session handle.
- **`PLAY [offset][len]`:** copies whole frames of that buffer into the
  cyclic buffer, waiting while it is full. A client that keeps playing is
  paced by the sound.
- **`DRAIN`:** waits until everything queued has been played, then the
  output rests.
- **`STOP`:** rests at once.
- **`INFO`:** the device and output, in words.
- **The format:** one, fixed. Clients convert.

### `play`

- **`play info`:** the device and its output.
- **`play tone [HZ] [SECONDS]`:** a sine tone with soft edges.
- **`play wav PATH`:** a 16-bit PCM WAV file, mono or stereo, at any rate
  from 8 to 192 kHz, converted to 48 kHz stereo (linear interpolation).
- The shell holds `use = audio`; apps do not, yet.

## Consequences

- **Oceans plays sound** on QEMU and, by the specification, on HD Audio
  hardware.
- **The smoke test checks it:**
  - QEMU's controller writes what it plays to `build/smoke-audio.wav`;
  - the shell plays a 440 Hz tone;
  - the host finds a tone of about 440 Hz in the file.
- **Limits:**
  - **Output only:** no microphone (input streams) yet (since ADR-0087:
    input too).
  - **One client at a time:** sessions share one stream, with no mixing.
  - **No volume control or jack detection:** the output is at 0 dB, and a
    plugged-in headphone does not move the sound.
  - **Codecs:** one codec, short-form connection lists only. HDMI audio and
    codecs that need vendor fixes may stay silent.
  - **A machine without HD Audio:** the grant cannot be met, so init fails
    to start `audio` and retries a few times, without effect on the rest.
  - **Apps:** no `audio` permission yet. Only the shell plays.

## Alternatives considered

- **The immediate command interface** (one command at a time through
  registers): simpler, but optional in the specification and missing on
  some controllers. The rings always exist.
- **Mixing several clients in the driver:** a sound server's job, later.
- **USB audio:** fewer machines need it; HD Audio covers the Tier 1 list.

## Checklist (master spec §48)

- **Purpose:** sound on Tier 1 hardware.
- **Architecture:**
  - `libs/hda` (registers, verbs, the path search, buffer descriptors, the
    cyclic buffer's accounting; host-tested);
  - `user/hda` (the driver);
  - `user/audio-proto` (the protocol and client API);
  - `play`.
- **API:**
  - the audio protocol (`INFO`, `OPEN`, `PLAY`, `DRAIN`, `STOP`);
  - `play info | tone | wav`.
- **Dependencies:** none new.
- **Security:**
  - the driver holds one device and its endpoint;
  - clients never see physical addresses;
  - every request is bounded by its session's buffer;
  - only the shell is granted `audio`.
- **Testing:**
  - unit: capabilities, verb encoding, answers, path search (a simple and
    a laptop-like codec, no path), buffer descriptors, the cyclic buffer's
    accounting;
  - smoke: a 440 Hz tone found in QEMU's recording;
  - the hardware smoke and the hardware list include the controller.
- **Failure behaviour:**
  - no device or output: `IoError`, said once;
  - a stuck position: `DRAIN` gives up after the queued time plus a
    second, and the output rests.
