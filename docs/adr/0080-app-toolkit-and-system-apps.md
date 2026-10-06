# ADR-0080: The app toolkit, and apps that come with the system

- Status: Accepted
- Date: 2026-10-06
- Depends on: ADR-0059 (windows), ADR-0077 (interface text), ADR-0078 (the
  look), ADR-0046 (packages), ADR-0045 (Oceans Core)
- Part of Phase 10 (Alpha: basic apps).

## Context

The owner asked for the basic apps a system comes with: Settings, a
browser, and the like.

Two things were missing:
- **A way to build them.** An app drew its window pixel by pixel (Notes:
  its own bitmap font, its own layout), so every app would cost much more
  than its purpose and would look different from the others.
- **A way to ship them.** Apps came only from `.opk` packages a user
  installs, so a fresh system had no apps at all.

## Decision

### `oceans-draw`: one drawing library for the desktop and apps

`libs/draw` (host-tested) holds what the display service drew with:
- **[`Surface`]:** a pixel buffer borrowed for drawing, with fills, rounded
  rectangles, translucent tints, outlines, anti-aliased circles and soft
  shadows;
- **[`Typesetter`]:** Noto Sans with Noto Sans Thai (ADR-0077), and the
  fonts with their license.

The display service and every app draw with it, so they look the same and
the code exists once.

### `oceans-ui`: an immediate-mode toolkit

`user/ui`: each frame, the app calls widgets in order (`heading`, `label`,
`row`, `button`, `primary_button`, `button_in`, `list`, `text_field`,
`sidebar`, `separator`). A widget:
- draws itself in the desktop's light look (ADR-0078);
- answers the click or keys that landed on it.

Nothing is kept between frames but the app's own state, so an app is a
function from its state to its screen.

`oceans_ui::run`:
- opens the window;
- turns window events (pointer, clicks, keys, focus, close) into input;
- draws a frame after each batch, and once more when the input changed
  something.

`Ui` works on any `Surface`, not only an app's window.

### Apps that come with the system

- **The image carries packages** (`.opk`) for its basic apps. xtask signs
  them with the image's own key:
  - the development key for development images;
  - the release key for releases (ADR-0072);
  - the hardware smoke test's key for its image.
- **init grants them to Oceans Core as modules** (`grant =
  module:NAME.opk`).
- **At start, Core installs each one** that is missing, or older than the
  one in the image, through the same checks as any install:
  - the signature and a trusted key;
  - the same key as the installed version;
  - a newer version.
- **Afterwards they are apps like any other:** the same permissions,
  dialogs and storage. The user can remove one, but a system update (a
  newer image) brings it back.

### The first one: Calculator

- **Use:** buttons or the keyboard (digits, `+ - * / %`, Enter, Backspace,
  Escape).
- **Permissions:** `window` only.

It proves the toolkit and the bundling. Settings, Files, Activity Monitor,
a text editor, a music player and a browser follow on the same footing.

## Consequences

- An app is short. Calculator is a page of logic and a frame function.
- A fresh system has its basic apps.
- **Each app embeds the fonts** (1.3 MB). Sharing them (a font service, or
  a read-only font module) is later work.
- **Settings needs more than an app's permissions** (deciding permissions,
  configuring the network). Those system rights are their own decision,
  the next one.
- **A browser** is a large piece of work of its own, beyond this toolkit:
  HTML, CSS and, later, JavaScript.

## Alternatives considered

- **A retained-mode toolkit** (widget trees, callbacks): more machinery
  and more state to keep in sync, for apps of a few screens.
- **System apps inside the display service:** a crash would take the
  desktop with it, and they would hold the desktop's authority.
- **Preinstalling by copying files to the disk image at build time:** the
  hardware image's root disk is formatted on the machine, and a release
  could not update its apps.

## Checklist (master spec §48)

- **Purpose:** build basic apps quickly, and ship them with the system.
- **Architecture:**
  - `libs/draw` (shapes, text);
  - `user/ui` (widgets, the window loop);
  - Core's install of bundled packages at start;
  - xtask's bundling and signing;
  - `user/apps/calculator`.
- **API:**
  - `oceans_draw::{Surface, Typesetter}`;
  - `oceans_ui::{Ui, run}`;
  - `grant = module:NAME.opk` to Core.
- **Dependencies:** none new (`ab_glyph` and the fonts moved into
  `libs/draw`).
- **Security:**
  - bundled packages pass every check an install does, against the image's
    trust list;
  - no new permission;
  - an app's widgets draw only in its own window.
- **Testing:**
  - unit (`libs/draw`): clipping, rounded corners, tints, circles, text and
    Thai glyphs;
  - smoke: Calculator installed by Core on a fresh disk, started, and its
    window on the screen.
- **Failure behaviour:** a bundled package that does not verify is not
  installed, and Core logs why and goes on.
