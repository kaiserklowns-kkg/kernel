# ADR-0150: Control Center in glass, and Bangkok's time by default

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0118 (Control Center), ADR-0130 (choosing a Wi-Fi
  network), ADR-0136 (the scale), ADR-0139 (the system's icons), ADR-0143
  (the time zone), ADR-0147 (the screen's brightness), ADR-0149 (the
  network button says what it is)
- Part of Phase 10 (the desktop experience).

## Context

Two things were asked for after a look at the desktop next to macOS Big
Sur's Control Center:

- **The clock was seven hours off.** The top bar showed UTC until someone
  chose a time zone in Settings (ADR-0143). Oceans is made and first used
  in Thailand, and nobody had chosen one.
- **Control Center looked flat.** It was an opaque grey panel of round
  buttons with their names under them, a "Now playing" box in words, and
  a line of memory and addresses at its foot. macOS draws it as frosted
  glass: what is behind shows through, blurred, and controls sit on paler
  cards, each with its name and its state.

## Decision

### The time zone until one is chosen is Bangkok's

Core answers `TIME_ZONE` with `DEFAULT_TIME_ZONE` (420 minutes, UTC+7,
`oceans_core_proto`) while `/system/timezone` holds nothing valid. A zone
chosen in Settings is kept and wins, as before; UTC is still one choice
among the others. The top bar's clock already follows the wall clock
every tick and the zone every few seconds, so it now shows Thai time at
the first boot.

### Frosted glass

`Canvas::frost` blurs what is already drawn under a rounded rectangle
(three box blurs across and down, which is close to a Gaussian, in device
pixels so that it is as soft at every scale) and lays a tint over it:
light glass (`#f6f6f8` at 104/255) or dark (`#262628` at 140/255), with a
hairline of light round the edge. Control Center and its Wi-Fi panel are
drawn on it. Only the panel's own pixels are read and written, so the
cost is the panel's area, paid when the desktop redraws.

### Cards, as macOS lays them out

| Card | What it shows | A click |
|---|---|---|
| Connections (tall, left) | three rows, each a round button, a name and a state: Wi-Fi with the network joined, or Ethernet with the address (ADR-0149); Dark Mode, On or Off; Keyboard with the layout | as before: the Wi-Fi panel or Settings; dark on and off; the next layout |
| Lock Screen (wide, right) | a padlock button and its name | locks |
| Settings, the accent (small, right) | a gear; the accent's colour with its name | Settings; the next accent |
| Display (full width, where the brightness can be set) | "Display", a sun and the slider | sets the brightness |
| What plays (full width) | the app's icon (ADR-0139), the title and the app; Previous, Play/Pause and Next at the right; "Not Playing" when nothing does | the media keys |
| Sound (full width) | "Sound", the speaker and the slider | the speaker mutes, the slider sets |

The cards are white over the glass (100/255 in light, 22/255 in dark);
buttons that are on are the accent with a white icon, off a pale disc.
The sliders are thick tracks with a white knob at the level. The memory
and address line is gone: the address is the Ethernet row's state, and
memory is Activity Monitor's.

The panel is 340 × 320 logical pixels, 392 with the brightness card; the
Wi-Fi panel keeps its rows (six).

## Consequences

- **The first boot shows Thai time.** A machine elsewhere shows Bangkok's
  time until a zone is chosen, as one in Thailand showed UTC before.
- **The smoke tests.** Their clicks follow the new places: Next in what
  plays, the slider and the speaker in Sound, the keyboard row, and the
  network row that opens the Wi-Fi panel. They check Music's icon in the
  playing card, Ethernet's button lit in the accent, and the muted
  slider's grey.

## Alternatives considered

- **Guess the zone from the network** (an IP lookup). It sends a request
  out at every first boot and is wrong behind VPNs; the choice is
  Settings'.
- **A real Gaussian blur.** Three box blurs look the same at this radius
  for a fraction of the work.
- **Translucency by alpha only, no blur.** Text behind the panel then
  fights with the text on it; the blur is what makes glass readable.

## Checklist (master spec §48)

- **Purpose.** The right time at the first boot; Control Center that
  reads at a glance and looks like the platform it is compared with.
- **Architecture.** Core's default zone; the display service's drawing.
- **API.** `oceans_core_proto::DEFAULT_TIME_ZONE`. No ABI change.
- **Dependencies.** None.
- **Security implications.** None: the panel reads only the screen's own
  back buffer; the zone is no secret.
- **Testing strategy.** The smoke tests' clicks and captures, as above.
- **Failure behaviour.** A zone file that is unreadable or invalid gives
  Bangkok's, as it gave UTC before.
