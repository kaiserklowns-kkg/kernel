# ADR-0135: MP3 and the radio in Music

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0094 (Music and sound for apps), ADR-0028 (the HTTP
  client), ADR-0031 (TLS), ADR-0047 (permissions), ADR-0103 (now
  playing)
- Part of Phase 10 (Alpha).

## Context

Music played only WAV files. People keep their music as MP3 and listen to
radio stations online. A station is a long HTTP response of MP3 frames
(Icecast answers `HTTP/1.0 200`, SHOUTCAST `ICY 200 OK`), on `http://` or
`https://`. Music runs a window and a tick every 100 ms: it cannot wait on
the network.

## Decision

- **`oceans-mp3`** (host-tested): MPEG audio (Layer III, I and II)
  decoded by `nanomp3-core` 0.2.0, a safe port of minimp3 (MIT or
  Apache-2.0, `#![forbid(unsafe_code)]`, no allocation), and converted to
  48 kHz stereo by linear interpolation. A `Stream` takes bytes in pieces
  of any size and gives output frames as far as they go. It decodes only
  with 16 KiB in hand (or at the end), since the decoder knows a frame by
  the ones after it and would take a cut one for junk. Junk and corrupt
  frames are skipped and counted.
- **`oceans-web`** (`user/web`): `GET` for apps over `http://` and
  `https://`. It follows redirects (never `https` to `http`), then gives
  the body as it arrives, either waiting or taking what is there. The TLS
  client's transport may answer "nothing yet", and the read is tried
  again later; `Client::transport_mut` sets how long reads wait. A stream
  that ends without TLS's `close_notify` has ended: the HTTP framing still
  catches a cut body that had a length.
- **`oceans-http`**: an `ICY` status line is read as HTTP/1.0, and the
  head keeps `Content-Type`.
- **Music 1.1.0**:
  - It lists `.mp3` files beside `.wav`.
  - An MP3 song's length and seeking come from its bitrate (exact for
    constant bitrates), after an ID3v2 tag.
  - A field takes a station's address: Listen plays it, and Save keeps it
    in `Music/Stations.txt` (`NAME = ADDRESS` lines). The kept stations
    follow the songs in the list.
  - A station plays live: its bar is full and shows the time listened.
    A station that is not MPEG audio is refused by its `Content-Type`.
  - Started with an address (`app start app.oceans.music http://…`), it
    plays that station.
  - It asks for `network` as a system app (ADR-0081: granted as the
    system's own).
- **The display** logs what an app tells it plays (`display: APP plays
  TITLE`, `paused`, `stopped`).

## Consequences

- Host tests:
  - a 2 s, 880 Hz MP3 decodes to about 2 s at 880 Hz;
  - the result is the same in pieces of 1 byte to 20 KB;
  - junk, a cut and noise are skipped without a panic;
  - `ICY` heads parse.
- The smoke test starts Music with a station on the host: SHOUTCAST
  style, the 880 Hz MP3, then closing. It waits for Music to play and to
  stop, and finds 880 Hz in the recording beside the shell's and Music's
  tones.
- Not yet:
  - AAC stations (many are AAC; they are told so);
  - the station's song titles (ICY metadata);
  - VBR length from a Xing header;
  - connecting without the window waiting (up to the 10 s connect and
    15 s head timeouts);
  - a list of stations that comes with the system.
- `fetch` keeps its own copy of the TLS glue (to be moved onto
  `oceans-web`).

## Alternatives considered

- **Symphonia:** its core needs `std`.
- **Decoding in a service:** another process and protocol for what one
  library does inside the app, which already holds the sound end.
- **A thread for the network:** processes are single-threaded (ADR-0014);
  reads that do not wait do the same job.

## Checklist (master spec §48)

- Purpose: MP3 and online radio.
- Architecture: a decoding library, an app HTTP library, and Music.
- API: `oceans_mp3::Stream`, `oceans_web::get` and `Response::read`.
- Dependencies: `nanomp3-core` (MIT or Apache-2.0).
- Security: the network only through the `network` permission. Streams
  are untrusted: the HTTP head is bounded, and the decoder is safe Rust
  that skips what it cannot use.
- Testing: host tests and the smoke test above.
- Failure: a refused or failed station is reported in Music's status
  line, and a dropped stream stops playing.
