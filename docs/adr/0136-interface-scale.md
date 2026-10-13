# ADR-0136: The interface's scale, and clearer status icons

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0057 (the display service), ADR-0059 (windows),
  ADR-0077 (interface text), ADR-0078 (the desktop's look), ADR-0080
  (`oceans-draw`, the app toolkit), ADR-0117 (the top bar)
- Part of Phase 10 (the desktop experience). ABI 23.

## Context

On the first real install (the HP ZBook, ADR-0132), the desktop looked
cramped. Every size was a fixed count of the screen's pixels, chosen on
QEMU's 1280 × 800 screen: 16-pixel text, a 28-pixel menu bar, 30-pixel
buttons. Limine sets the panel's native mode, so on a 15.6" 1920 × 1080
panel (141 pixels per inch) the same pixels are about a fifth smaller
than on a desktop monitor, and on a 4K panel half the size. Nothing
overflowed. Everything was small and dense, and the padding around text
shrank to slivers.

The top bar's icons were also drawn pixel by pixel. The network was three
joined boxes, which nobody reads as Wi-Fi.

## Decision

### Logical pixels, drawn at a scale

Everything is laid out in **logical pixels**, as before, and drawn at a
**scale** into the screen's own pixels:

- **Scales offered.** 100, 125, 150, 175, 200, 250 and 300 %.
- **What changes.** Shapes, curves and text are drawn at the screen's
  resolution, so a larger scale is sharp, not enlarged.
- **What does not.** At 100 % nothing changes, pixel for pixel.

`oceans_window::Scale` holds the arithmetic (host-tested):

- **Edges, not sizes.** Rectangles are turned edge by edge, so ones that
  touch in logical pixels still touch at 125 %.
- **Exact sizes.** A window's size survives the trip to its pixels and
  back.
- **Pointer mapping.** `logical` finds the logical pixel a device pixel
  lies in, so the pointer knows what it is over.

### `oceans-draw` draws at it

- **`Surface::scaled`.** A surface is given in device pixels and drawn on
  in logical ones; `Surface::new` stays 100 %.
  - Rounded rectangles, tints and shadows are made in device pixels from
    the turned rectangle and radius.
  - Outlines are one logical pixel thick.
  - Circles take sixteen samples a pixel around the logical pixel's
    middle.
- **`Typesetter`.** It rasterizes glyphs at the style's size times the
  scale, and `measure` answers in logical pixels. Text therefore keeps its
  logical width at any scale, and ends where it was measured to.
- **Pixels of one's own.** `Surface::device` and `device_size` let an app
  draw them at the buffer's resolution (Image Viewer's photographs: at
  "actual size" one of the picture's pixels is one of the screen's).

### Windows get the screen's pixels

- **`OPEN` and `RESIZE`.** Their replies carry a `proto::Pixels`: the
  device size and the scale (6 bytes). The size asked for, the manager's
  geometry and the events stay in logical pixels.
- **Memory.** The pixel memory is the device size, and its size is
  checked before the app maps it.
- **The toolkit.** `oceans-ui` draws each frame on `Surface::scaled`, so
  every app on it is scaled with no change of its own.
- **Notes.** The example app that draws its own pixels picks its bitmap
  font's size from the scale.
- **The desktop.** It copies app pixels one for one.
- **A new scale.** `Manager::rearrange` takes the new logical area:
  - tiled windows take their part of it;
  - the others are moved back inside it, a resizable one shrunk to fit;
  - every window is told `RESIZE`, so its app takes pixels at the new
    scale.

### The desktop at the scale

- **The canvas.** The display service's canvas is the screen's pixels;
  its `width × height` are logical. Every position in `desktop` is
  unchanged.
- **The pointer.** It moves over device pixels, so a larger scale does
  not make it coarser or faster. Its arrow is drawn there, as large as
  the scale makes it.
- **The Terminal.** It takes the bitmap font nearest the scale (16, 20,
  24 or 32).
- **The Oceans logo.** It is made from its 512-pixel picture at the size
  needed, once per size, and the 40-pixel copy goes.

### Which scale

- **The choice.** Core keeps it in `/system/scale` (`SCALE`/`SET_SCALE`;
  0 means automatic). It is the machine's, not an account's, because the
  lock screen is drawn at it too.
- **Where it is set.** Settings → Displays and the shell's `scale [auto |
  PERCENT]` set it. The desktop takes it up within two seconds.
- **Automatic.** It follows the screen's density:
  - The kernel reads the panel's size from its EDID (Limine passes it;
    the first detailed timing's millimetres, else the basic block's
    centimetres).
  - It reports the size in `DISPLAY_INFO`, whose record grows to 32 bytes
    (**ABI 23**).
  - The density over 112 pixels per inch, the density the interface was
    drawn for, gives the scale, to the nearest choice that fits.
  - Without a believable size (aspect ratios, projectors), it goes by
    resolution, as a laptop of it would want.

  | Screen | Density | Automatic scale |
  |---|---|---|
  | 15.6" 1080p | 141 ppi | 125 % |
  | 14" 1080p | 158 ppi | 150 % |
  | 24" monitor | 92 ppi | 100 % |
  | QEMU | 100 dpi | 100 % |
- **What fits.** A scale is offered only if the screen keeps at least
  1024 × 720 logical pixels. A 1080p panel takes up to 150 %, a 4K one up
  to 300 %. A chosen scale the screen cannot take falls back to
  automatic.
- **`SCREEN` (window op 14).** It tells an app the screen's pixels, the
  scale in use and the automatic one. Settings shows the resolution,
  "looks like" and the choices that fit with it.

### Icons from outlines

- **Outlines.** `oceans_draw::Path` adds lines, quadratic and cubic
  curves, arcs, circles, rounded rectangles and round-capped strokes.
  They are filled anti-aliased at the surface's scale by the rasterizer
  `ab_glyph` fills glyphs with; a shape drawn the other way round cuts a
  hole.
- **The icons.** `oceans_draw::icon` draws them on a 24-unit grid:
  - Wi-Fi: a fan of three bands over a dot, bands lit by signal;
  - Wi-Fi off: the fan dimmed and struck through;
  - Ethernet: `<···>`, struck through when the cable does not connect;
  - a speaker with up to three waves, or a cross when muted;
  - a bell;
  - two switches for Control Center;
  - a person in a circle.
- **The top bar.** It draws them 18 pixels square. The network icon is
  now Wi-Fi with its signal when Wi-Fi is joined, Ethernet when a cable
  connects, otherwise Wi-Fi off or Ethernet struck through. The Wi-Fi
  list and Control Center's network button use the same fan.

## Consequences

- **The ZBook.** Its 15.6" panel gets 125 % by itself: the interface is
  the size it was designed at, and sharper than QEMU's. Settings →
  Displays offers 100 % and 150 %.
- **QEMU.** The smoke tests' screen, 1280 × 800 at 100 dpi, stays 100 %,
  so every pixel the smoke tests probe is where it was.
- **Trying other scales on QEMU.** `OCEANS_RESOLUTION=1920x1080 cargo
  xtask run` has Limine set that mode (`resolution:`), since QEMU's
  firmware keeps 1280 × 800. Without an EDID size it comes up at 125 %,
  and a live change to 150 % in Settings rearranges the windows and
  redraws every app at it.
- **Apps on the toolkit.** They need nothing. An app that writes pixels
  itself must use `Window::scale`, as Notes and Image Viewer now do.
- **Fixed-size windows.** Settings, Calculator, Music and Notes keep
  their logical size, so they grow on the screen with the scale. Laying
  windows out to the space they have (responsive layout, windows sized
  from the screen) is the next milestone.
- **Memory.** Window memory grows with the scale's square, still bounded
  by the pixel budget (twice the screen, now counted in logical pixels).
- **Glyph cache.** It holds one scale and is dropped when the scale
  changes.

## Alternatives considered

- **A lower framebuffer mode** (Limine's `resolution`): it is blurry on
  an LCD, does not help a 4K panel's text, and leaves the layout
  problems.
- **Scaling every constant at its use:** hundreds of places in the
  desktop and every app, each one a chance to forget. Scaling at the
  drawing surface keeps the layout code as it was.
- **Rendering at 2× and shrinking** (as macOS does for "looks like"
  sizes): it costs four times the pixels on a machine with no GPU, and
  does not give sharp 125 %.
- **Only integer scales:** 1080p laptops need 125–150 %, which integer
  scales cannot give.
- **Icons as bitmaps** at each size: they need a set per scale and a tool
  to make them. Outlines on a grid are a few lines each and sharp at any
  size.

## Checklist (master spec §48)

- **Purpose.** An interface of the same physical size on any screen, and
  status icons that read at a glance.
- **Architecture.** Scale at the drawing surface; logical geometry
  everywhere else.
- **API.**
  - `Scale`, `Surface::scaled`, `Path`, `icon`;
  - `proto::Pixels` and `Screen`, op `SCREEN`;
  - Core's `SCALE` and `SET_SCALE`;
  - `DISPLAY_INFO`'s millimetres (ABI 23).
- **Dependencies.** `ab_glyph_rasterizer` and `libm`, both already
  `ab_glyph`'s, are now direct dependencies of `oceans-draw`.
- **Security implications.**
  - `SET_SCALE` is a user setting (`DECIDE`), like the appearance.
  - Pixel memory is checked against the size the reply gives before it
    is mapped.
  - The EDID is parsed bounds-checked, and only two sizes are taken.
- **Testing strategy.**
  - Host tests:
    - the scale's arithmetic: touching edges, inversion, choices,
      automatic for real panels;
    - scaled drawing and text;
    - paths and holes;
    - the icons at 100 and 200 %;
    - `Pixels`/`Screen` encoding;
    - rearranging windows;
    - EDID parsing.
  - The smoke tests check every existing screen at 100 %, the display's
    `scale` line, and the shell's `scale` command.
- **Failure behaviour.**
  - A scale Core cannot give or the screen cannot take falls back to
    automatic.
  - A screen with no EDID falls back to its resolution.
  - An app that has not caught up with a new scale shows its old pixels,
    the rest in the window's colour, until it presents new ones.
