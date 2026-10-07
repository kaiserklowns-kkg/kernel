# ADR-0090: Image Viewer

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0080 (the app toolkit, apps that come with the system),
  ADR-0081 (system apps' rights), ADR-0053 (`files`)
- Part of Phase 10 (Alpha: basic apps).

## Context

The system's own apps could do a lot with files: show them in Files, edit
them in Text Editor, play them with `play`. They could not show a picture.
A picture viewer is among the first things a person tries on a new desktop.

Decoding images is a classic attack surface: complex formats, files from
anywhere. PNG also needs DEFLATE.

## Decision

### `oceans-image` (`libs/image`, host-tested, `no_std` + `alloc`)

- **DEFLATE and zlib, its own** (`inflate`), written after the reference
  decoder `puff` (Mark Adler):
  - stored, fixed and dynamic blocks, decoded a bit at a time;
  - code sets checked: over-subscribed sets are refused, and incomplete
    ones fail only when an unused code appears;
  - every length and distance is checked;
  - output is bounded by the size the image says;
  - zlib's header and Adler-32 are checked, and preset dictionaries
    refused.
- **PNG:**
  - every colour type and every depth the standard allows (1 to 16 bits),
    palettes, `tRNS` transparency (palette alphas and colour keys);
  - all five row filters, and Adam7 interlacing;
  - chunk CRCs are checked;
  - an unknown critical chunk is refused, and unknown ancillary chunks are
    skipped.
- **BMP:**
  - 1, 4 and 8 bits with a palette: entries are read up to the pixel
    data, as files often say "all" and carry fewer;
  - 16 bits (5-5-5 or bit fields), 24 bits, and 32 bits (bit fields, with
    alpha when a mask names it);
  - bottom-up and top-down;
  - RLE-compressed bitmaps are refused.
- **Limits, checked before allocating:** 16,384 pixels a side,
  4,194,304 pixels (16 MiB decoded), and 32 MiB of file or compressed data.
- **Errors, not panics.** A decoding error is an `Error` value (unknown
  format, cut short, corrupt, unsupported, too large) with a message for
  the user.
- **Showing a picture:**
  - `fit`: the largest size in a box, never enlarged;
  - `scale`: box-filter averages when shrinking, weighted by alpha so
    transparent pixels do not darken; nearest pixel when growing;
  - `over_checker`: transparency over an 8-pixel checkerboard.

### Image Viewer (`app.oceans.viewer`, comes with the system)

- **Rights:** `window` and `files` (the user's files), like Text Editor.
- **Opening a picture:**
  - a name in Home (`folder/name.png` in a folder) and Open;
  - or the name it is started with (`app start app.oceans.viewer NAME`).
- **Moving through a folder:** Previous and Next (Page Up and Page Down,
  Space) go through the folder's pictures by name. The ends of the
  folder are said, not wrapped.
- **Fit** (the default, scaled once per picture and size) or **Actual
  size**, where the arrows move around a larger picture. F switches.
- **The status bar:** name, size, format and zoom, or what went wrong
  ("the image file is cut short").

## Consequences

- Pictures open on the desktop, PNG from any modern tool and BMP from old
  ones.
- **The smoke test:**
  - the host serves a 200 × 120 RGBA PNG (stored blocks: the left half
    orange, the right half transparent);
  - the guest fetches it into Home and starts the viewer with its name;
  - the screen shows the orange and, through the transparent half, the
    checkerboard.

  An xtask unit test decodes the same file with `oceans-image`. The
  library's tests decode streams and a PNG made by the reference zlib
  (`libs/image/testdata/gen_fixtures.py`), so the Huffman paths are
  checked against real compressor output. A small encoder in the tests
  round-trips every colour type, depth, filter and Adam7.
- **Limits:**
  - **No JPEG, GIF or WebP.** JPEG is the next format worth adding: most
    photographs are JPEG.
  - **Large pictures:** a 16-megapixel photo is refused (limits above).
    Decoding is done in one go, and Fit scales the whole picture when it
    is opened.
  - **No zoom steps beyond Fit and 100 %, no rotation, no editing.**
  - Files does not open pictures in the viewer yet. An "open with"
    between apps needs a system API (a later ADR).

## Alternatives considered

- **A crate for PNG (`png`, `miniz_oxide`):** proven and fast. But these
  are `std`-oriented or pull in more than a `no_std` app needs, and a
  decoder for untrusted files is code Oceans wants to read, bound and test
  itself. `puff`'s design is small and well understood.
- **Decoding in a separate, sandboxed process:** more isolation, at the
  price of a protocol and a second process per picture. The viewer
  already holds only `window` and the user's files. A bug in decoding
  can at worst crash the viewer or show a wrong picture.
- **Scaling on every frame:** simpler, but it costs a full resample of the
  picture whenever the window redraws.

## Checklist (master spec §48)

- **Purpose:** look at pictures.
- **Architecture:**
  - `libs/image` (decoding and scaling, host-tested);
  - `user/apps/viewer` (the app);
  - bundled like the other system apps (`viewer.opk`).
- **API:**
  - `oceans_image::{decode, format, fit, scale, over_checker,
    is_image_name, Image, Format, Error}`;
  - the app's start argument.
- **Dependencies:** none new.
- **Security:**
  - untrusted input is bounded before allocation, every index is checked,
    and checksums are verified;
  - the app's rights are `window` and the user's files.
- **Testing:**
  - unit: the inflater against zlib, PNG of every type and Adam7, BMP of
    every depth, broken files, fit, scaling and the checkerboard;
  - smoke: the picture on screen;
  - `smoke-hw`: the app listed.
- **Failure behaviour:**
  - a file it cannot read is said in the status bar with the reason, and
    the last picture stays;
  - limits refuse what would not fit in memory.
