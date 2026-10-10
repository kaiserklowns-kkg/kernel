# ADR-0105: The direction of the UI: native on the device, the web from afar

- Status: Accepted
- Date: 2026-10-10
- Amends: ADR-0001 (language boundaries: the on-device apps), ADR-0056
  (UI architecture: the on-device web view)
- Depends on: ADR-0058 (web experience and bridge), ADR-0078 (desktop
  look), ADR-0080 (app toolkit and system apps)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

ADR-0001 put Settings, Store, AI Center and Control Center in SvelteKit.
ADR-0056 kept that for the web experience, and left showing it on the
device for later, "once Oceans has an HTML engine"; it called that the
largest open item of Phase 7.

Since then, the system's apps on the device have been written natively,
with `oceans-draw` and `oceans-ui` (ADR-0080): Calculator, Settings,
Files, Activity Monitor, the text editor, the image viewer and Music. The
direction was never written down, and the language boundaries still said
otherwise.

**What users expect, on every system:**
- **macOS, Windows, GNOME, KDE:** the system's own apps are native: they
  start at once, look like the system, and work without a network.
- **Everywhere:** settings, the store and the security prompts are part
  of the system, not a web page that could look like one.
- **Also:** managing a machine from another one's browser (a router's or
  a NAS's admin page, Cockpit on Linux) is common and welcome, but it
  comes second.

## Decision

### What the device shows is native

- **The desktop and every app that comes with the system** are Rust,
  drawn with `oceans-draw` and `oceans-ui`. This includes Store, AI
  Center and Control Center when they come to the device.
- **Apps by others** are native too: Rust with `oceans-ui`, or Go with
  its window package (ADR-0060).
- **No HTML engine** is planned for the device. Web apps (ADR-0064) are
  served by the bridge to a paired browser, as now.
- **It is reconsidered** only if apps by others need the web on the
  device, as a decision of its own.

### The web experience stays, from afar

- **SvelteKit**, built with Bun, served by the bridge (ADR-0058): the
  system from any paired browser.
- **It offers nothing the device lacks.** What it can do goes through the
  same Core rights, and the device confirms what needs the user there
  (installs, ADR-0061).

### The look: Big Sur's language, polished

The layout of ADR-0078 stays: a menu bar, a floating dock, light
translucent windows with round buttons at the left. It gains, as the work
below touches each part:
- **one set of design values** (colours, corner radii, spacing, type
  sizes) in `oceans-draw`, which the desktop and the toolkit both read;
- **a dark appearance**, chosen in Settings, from the same values;
- **an accent colour**, chosen in Settings.

### Accessibility is laid down now

`oceans-ui` gives every widget it draws a **role** (button, field, list,
item, heading, label) and a **name**, and:
- **Tab** and **Shift+Tab** move a keyboard focus through what can be
  used, in the order drawn;
- **Enter** and **Space** press the focused button; the arrows move in a
  list;
- the focused widget shows a **focus ring** in the accent.

What a screen reader needs later (the tree of roles and names, sent to an
accessibility service) builds on this, as its own decision.

### The order of the work

1. Accessibility's foundation in `oceans-ui` (above).
2. **Arranging windows:** halves of the screen, full screen, keyboard
   shortcuts.
3. **Search and the launcher:** a search field for apps and files; apps
   kept on the dock.
4. **The lock screen and users:** accounts, a password, a lock screen,
   each user's own data (several decisions).

## Consequences

- **The language boundaries change:** Rust for what the device shows;
  SvelteKit only for the web experience. ADR-0001 and the architecture
  overview say so.
- **Phase 7's largest open item is closed**, by deciding not to build an
  HTML engine for now rather than by building one.
- **Each app the system has is written once, natively.** The web
  experience is a second view of the same rights, kept smaller.
- **An app that wants accessibility gets it** by using the toolkit's
  widgets; one that draws its own must give its own names.
- **Not yet:**
  - a screen reader;
  - dark appearance and accent in the apps by others that draw their own;
  - the web on the device.

## Alternatives considered

- **A minimal HTML engine for the SvelteKit apps:** HTML, CSS layout and
  a JavaScript engine are each larger than what the device runs today,
  and parsing the web is a large attack surface next to the permission
  dialogs. It would hold up every other UI work for months.
- **One declarative UI for both** (apps send a tree of widgets, drawn
  natively or as HTML): a new toolkit and a new protocol, while the
  immediate-mode one is already in use by every app.
- **A look of Oceans' own:** a new design language to draw and to learn;
  the one in place is current and familiar.
- **Back to the Windows layout (ADR-0076):** a third change of look for no
  gain in what the system can do.
- **Accessibility later:** every app built until then would need its
  widgets named again.

## Checklist (master spec §48)

- **Purpose:** one way to build what the device shows, written down.
- **Architecture:** native Rust on the device (`oceans-draw`,
  `oceans-ui`, the display service); SvelteKit through the bridge for
  other devices.
- **API:** no change now. The toolkit's focus and roles come in the
  milestone that builds them.
- **Dependencies:** none added; an HTML engine is not taken on.
- **Security implications:** what the device shows, permission dialogs
  among it, depends on no web engine; the bridge keeps no authority of
  its own (ADR-0058).
- **Testing strategy:** each milestone's own (host tests, the QEMU
  smokes clicking and checking the screen).
- **Failure behaviour:** unchanged: if the display service stops, the
  kernel console takes the screen back (ADR-0057); the web experience
  works without the device's screen.
