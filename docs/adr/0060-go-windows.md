# ADR-0060: Windows for Go apps

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0050 (Go on Oceans), ADR-0052 (Go apps as packages),
  ADR-0059 (keyboard focus and app windows)

## Context

ADR-0059 gave apps windows, but only native Rust apps could use them:
- the client API (`oceans-display-proto`) maps the window's shared pixel
  memory and writes into it;
- a Go app runs as WebAssembly in the Go host (`gohost`), and its linear
  memory belongs to the interpreter. A memory object cannot be mapped into
  it.

Most Oceans apps are meant to be Go or web apps (master spec §6), so they
need windows too.

## Decision

- **A host function `memory_write (memory, offset, buf, len)`.** It copies
  `len` bytes of the module's memory into a memory object the process
  holds:
  - the handle needs `WRITE` and `MAP`;
  - all of it fits, or nothing is written;
  - the host maps the object, copies straight from the module's memory
    (no intermediate buffer: a frame is hundreds of KiB), and unmaps it.

  The binding is `oceans.MemoryWrite`.
- **`go/oceans/window`**, the Go side of the window protocol:
  - `Open (windows, notification, bits, width, height, title)` returns a
    `*Window` with `Pixels []uint32` in Go memory;
  - `Present` copies `Pixels` into the shared memory and sends `PRESENT`;
  - `Events` and `Close` complete the protocol;
  - the same limits, kinds and encodings as `oceans_window::proto`.
- **The two sides cannot drift apart:** a test in `libs/window` (Rust) and
  one in `go/oceans/window` (Go) check the same event and `OPEN`
  encodings, byte for byte.
- **The example: Tiles** (`app.oceans.tiles`, `runtime = wasm`, `window`):
  - a 320×200 window of one colour;
  - every key moves it to the next colour;
  - the close button ends it.

## Consequences

- **Go apps open windows with the same security as native ones:** a
  badged end from Core, a frame naming the app, keys only with the focus.
  The host function only reaches memory objects the process already holds
  with write rights.
- **Cost:** a frame is copied twice (Go memory to shared memory, then to
  the back buffer). For the sizes allowed (at most 1024×768), that is
  acceptable next to interpreting Go. A later Go host could map windows
  directly if wasm memories ever may be shared.
- **Smoke tests:**
  - a new step, `@screen X Y RRGGBB WHAT`, captures the screen until a
    pixel has a colour, for up to a minute, so slow drawing (a Go app in
    the interpreter) does not make the test flaky;
  - the kernel's smoke-mode limit for the scripted session now matches the
    host's 300 s per boot (ADR-0059 raised only the host's, and boot 1 had
    started to come close).

## Alternatives considered

- **Mapping the pixel memory into the wasm module:** wasmi's linear memory
  is one contiguous host allocation. Another mapping cannot be placed
  inside it.
- **Sending pixels over IPC:** a 256-byte message limit would mean about
  1800 calls per 480×240 frame.
- **A host-side window API (open/draw in Rust, called from Go):** it
  duplicates the protocol in the host and widens the host's surface. A
  generic memory write is smaller and serves other shared buffers too.

## Checklist (master spec §48)

- **Purpose:** windows for Go apps.
- **Architecture:** a gohost host function; a Go client package over the
  existing window protocol.
- **API:**
  - `memory_write` (gohost);
  - `oceans.MemoryWrite`;
  - `go/oceans/window` (`Open`, `Present`, `Events`, `Close`, `Event`,
    the limits and kinds).
- **Dependencies:** none.
- **Security:**
  - only memory objects the process holds, with `WRITE` and `MAP`;
  - bounds are checked against both the object and the module's memory;
  - nothing else changes from ADR-0059.
- **Testing:**
  - unit: Go (wire format against Rust's bytes, unknown events, `OPEN`
    bounds; Tiles' palette) and Rust (the same bytes);
  - smoke: Tiles is installed and started in the background; its window
    shows its first colour; a key typed into it shows the next; it is
    stopped.
- **Failure behaviour:**
  - a write that does not fit: `InvalidArgument`, nothing written;
  - a bad module range: `BadAddress`;
  - no `window` permission: Tiles exits with code 3.
