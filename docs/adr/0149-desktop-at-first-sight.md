# ADR-0149: The desktop at first sight

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0076 (the desktop shell), ADR-0117 (Wave), ADR-0118
  (Control Center), ADR-0130 (choosing a Wi-Fi network), ADR-0136 (the
  top bar's network icon)
- Part of Phase 10 (the desktop experience).

## Context

Two things looked wrong the moment the desktop came up, on a screenshot
of `main` at 1920 × 1080:

- **The Terminal.** It opened in the middle of the screen at every boot,
  filled with the system's log. It made Oceans look like a developer's
  machine rather than a desktop. macOS and Windows open with a clear
  desktop and keep their terminals one click away.
- **Control Center's network button.** It always showed Wi-Fi's fan
  labelled "Network", even on a machine connected by cable. The top bar's
  icon (ADR-0136) already said Ethernet, so the two disagreed.

## Decision

### The Terminal starts put away

- **At start.** The desktop starts with the Terminal put away
  (`terminal_hidden`), as if minimized. It keeps its place in Wave (the
  first icon, no dot under it) and in Launchpad, and a click there shows
  it, as before.
- **Ctrl+Tab.** Going round to the Terminal with Ctrl+Tab (ADR-0059) now
  also shows it when it is put away, as Alt+Tab brings a window back on
  other systems. It does so only for the administrator, whose console it
  is (ADR-0115).
- **What it still does.** The shell behind it runs as before: the serial
  console and everything typed to it are unchanged.

### Control Center's network button says what it is

The button opens the Wi-Fi panel when there is Wi-Fi (ADR-0130), so it is
named and drawn for what it opens:

| Machine | Label | Lit | Icon |
|---|---|---|---|
| With Wi-Fi, joined | Wi-Fi | yes | Wi-Fi's fan, its signal's bands lit |
| With Wi-Fi, not joined | Wi-Fi | no | the fan, dimmed and struck through |
| Without Wi-Fi | Ethernet | yes, once the cable connects | `<···>`, struck through while it does not |

The icons are the top bar's own (`oceans_draw::icon`), so the two always
agree.

## Consequences

- **First sight.** The first thing seen is the wallpaper, the top bar and
  Wave. People who want the console know where it is: Wave, Launchpad or
  Ctrl+Tab.
- **The smoke tests.** They now check, on their screen captures:
  - the desktop at start shows no Terminal;
  - Notes' window opens with the Terminal still put away;
  - Ctrl+Tab gives the Terminal the keyboard and shows it with the focus
    colour.

  The rest of the run goes on with the Terminal shown, as before.

## Alternatives considered

- **Show the Terminal on the first boot only.** First boots are when a
  clear desktop matters most.
- **Drop the Terminal from the desktop.** It is the administrator's
  console and stays one click away; Oceans keeps its shell.
- **Keep "Network" as the label.** It does not say which network, and
  "Wi-Fi" or "Ethernet" is what both other systems show.

## Checklist (master spec §48)

- **Purpose.** A clear desktop at start; Control Center agreeing with
  the top bar.
- **Architecture.** The display service only.
- **API.** None.
- **Dependencies.** None.
- **Security implications.** None: the Terminal is still the
  administrator's alone, and showing it by Ctrl+Tab checks that.
- **Testing strategy.** The smoke tests' captures, as above.
- **Failure behaviour.** Unchanged.
