# ADR-0155: Legibility: more contrast, less transparency, and a contrast test

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0113 (the design values, the dark appearance),
  ADR-0117 and ADR-0121 (Wave and the top bar), ADR-0146 (the promises)
- Part of Phase 10 (Alpha). No ABI change.
- Plan: WP-7 of [development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md).

## Context

Promise P10 (ADR-0146) is legibility before effects. macOS Tahoe's Liquid
Glass is the warning: translucent menus over busy wallpaper, which
people found hard to read. Oceans' top bar has no band of its own. Its
white text sits on the wallpaper under a soft shade, as do the names of the
icons on the desktop.

Nothing measured whether Oceans' own colours are legible either. Each
design value (ADR-0113) was chosen by eye.

## Decision

### Two options beside light and dark

`oceans_draw::theme::Options`, a byte of bits:

| Bit | Option | What changes |
|---|---|---|
| 1 | **Increase contrast** | Text full black or white; secondary text, lines, borders and controls stronger; accents deep enough for white text on them (`CONTRAST_ACCENTS`); the shades behind white text over the wallpaper twice as dark |
| 2 | **Reduce transparency** | The top bar a solid dark band (`1c1c1e`); the shades behind the desktop icons' names and Wave's tooltips opaque |

- `Theme::with(appearance, accent, options)` makes the colours, and
  `Theme::new` is `with` without options. The colours are unchanged when
  no option is set.
- `theme::set_options` and `current()` hold the options for each process,
  as the appearance is held.
- The desktop's `colour::backing(alpha)` is the one rule for every shade
  behind white text: it is opaque with less transparency, doubled with more
  contrast, and otherwise as designed.

### How the options travel, without breaking older apps

- **Core:**
  - `APPEARANCE` answers three bytes, `[appearance][accent][options]`. A
    reader of the first two still works.
  - `SET_APPEARANCE` takes two bytes (the kept options stay, as Control
    Center sends) or three.
  - `/system/appearance` (each user's own, ADR-0116) is `A C O`. A file
    from an older Oceans (`A C`) reads as no options.
  - Options above 3 are refused.
- **Windows:** `kind::THEME` keeps `key` (0 light, 1 dark) and `button`
  (the accent) as they were, and carries the options in `x`. An app built
  before this reads only `key` and `button`. If the appearance byte itself
  carried the bits, an older app would take "dark with more contrast" for
  light.
- **`oceans-ui`** sets the options from `THEME`, so every native app
  follows at once.

### Where the user sets them

- **Settings → Appearance → Legibility:** "Increase contrast" and
  "Reduce transparency", each a button filled while it is on.
- **The shell:** `appearance contrast | plain | solid | clear`, beside
  `light | dark` and the accent.

### The contrast test

`oceans_draw::theme`'s `text_is_legible_everywhere` computes WCAG 2.x
contrast ratios for every appearance (2), option (4) and accent (6):

| Pair | Needs |
|---|---|
| Text on window, sidebar, surface, button (and hovered), selected row, panel, dialog, both title bars | 4.5:1 |
| Secondary text on window, sidebar, surface, panel, dialog | 4.5:1 |
| White labels on the accent (bold, WCAG's large text) | 3:1; 4.5:1 with more contrast |
| Borders and lines on window and surface, with more contrast (WCAG 1.4.11) | 3:1 |

A design value changed below these fails the build.

## What the user sees change

- Two new buttons under Settings → Appearance.
- The test found two values below WCAG, and they are corrected. This is
  the only change seen without an option:
  - The light appearance's secondary text, `6e6e73` → `66666b`, which was
    4.19:1 on the sidebar.
  - The Orange accent, `f58f3b` → `e06c0a`, which was 2.37:1 under white
    labels.

## Consequences

- People who find the translucent bar hard to read have a way out, in
  Settings → Appearance rather than deep in an accessibility panel, where
  macOS and Windows keep theirs.
- Every later change to a colour is checked against WCAG, not by eye.
- Go apps draw their own colours, and do not follow the options yet.
- The options make the top bar darker in the light appearance too. A light
  solid bar would need its text and icons drawn dark, which is a larger
  change, left for later.

## Alternatives considered

- **Put the options in the appearance byte (values 2–7).** This is
  simpler, but apps built before it would read `from_byte(3)` as light.
- **A blur behind the bar instead of a shade.** It costs a pass over
  pixels every frame on a software-drawn screen, and it solves looks, not
  legibility.
- **Check contrast by eye from screenshots.** That cannot be repeated, and
  it would not catch a value changed later.

## Checklist (master spec §48)

- **Purpose:** text that can always be read, and options for those who
  need more.
- **Architecture:**
  - `oceans_draw::theme::{Options, Theme::with, CONTRAST_ACCENTS}`;
  - Core's appearance file and ops;
  - the window manager's `THEME`;
  - the desktop's `colour::backing` and solid bar;
  - Settings and the shell.
- **API:**
  - `APPEARANCE` answers 3 bytes, and `SET_APPEARANCE` takes 2 or 3;
  - `kind::THEME`'s `x`;
  - `theme::set_options`.

  The kernel ABI is unchanged.
- **Dependencies:** none.
- **Security:** options are display only. They are validated by Core, and
  each user's are their own (ADR-0116).
- **Testing:**
  - the WCAG test (96 combinations);
  - the options' byte round trip;
  - unchanged colours without options;
  - the window manager carries `x`;
  - the smoke test turns both options on from the shell, checks the solid
    bar's pixel, then turns them off.
- **Failure behaviour:**
  - a damaged or older appearance file reads as no options;
  - an unknown option bit is refused by Core and ignored by apps.
- **Promises touched:** P10 (kept by this ADR), P7 (nothing moves; two
  buttons added, two colours deepened).
