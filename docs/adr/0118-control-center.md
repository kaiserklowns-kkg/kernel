# ADR-0118: Control Center, and the status icons

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0117 (the top bar), ADR-0100 (volume), ADR-0103 (what
  plays), ADR-0113 (appearance and accent), ADR-0114 (keyboard layouts),
  ADR-0096 (reader ends)
- Supersedes: the sound panel of ADR-0100 and ADR-0103.
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

The design the user chose (ADR-0117) puts, at the top bar's right, the
status icons phones show and a **Control Center** opening from them:
round toggles, what plays, the keyboard, the appearance and the volume,
each in a module of its own (macOS Big Sur's Control Center, iOS's and
Android's Quick Settings, drawn by the user). The desktop had only a
sound panel under the speaker, and could not see the network at all.

**What users expect, on every system:**
- **macOS Big Sur:** Control Center from the menu bar: Wi-Fi, Bluetooth
  and AirDrop as round toggles in one module; Do Not Disturb; display and
  sound sliders; Now Playing; the status icons (battery, Wi-Fi) beside.
- **iOS, Android:** the same modules from a swipe; toggles light in the
  accent when on; a tap on a tile does the obvious thing.

## Decision

### The status icons

- **From the top bar's end in:** Control Center's own (two switches), the
  **network** (three joined nodes; struck through when no address is
  configured; absent if the desktop cannot read it), the **speaker**.
  Each opens and closes Control Center. Left of them stay the keyboard
  layout and the app in use.
- **The network is read, not used:** Core's new `NET_READER` (op 42,
  `manage`) hands the desktop a reader end of the network stack, as
  Settings has had since ADR-0096: `INFO` answers, nothing opens. The
  desktop asks every two seconds and logs a change (`desktop: network
  connected, 10.0.2.15`).

### Control Center

Under the top bar's right end, 340 × 318, on the appearance's panel
colour, each module on its own surface:

- **Round toggles,** two by two, lit in the accent when on: the
  **network** (on when connected; opens Settings, which shows it),
  **Dark** (the appearance, kept by Core for the account in use),
  **Lock** (the screen locks), **Settings**.
- **Now playing** (ADR-0103): the title and the app, and Previous,
  Play/Pause and Next, which press the media keys for the player; or
  "Nothing".
- **Keyboard:** the layout's name; a click goes to the next, as
  Super+Space does.
- **Accent:** its colour and name; a click goes to the next accent.
- **Sound:** the speaker mutes and unmutes; a thick slider sets the
  level (grey while muted). Without a sound device it says so.
- **A line of the system's state:** memory in use and the network's
  address ("Ethernet 10.0.2.15") or "Not connected". Memory left the top
  bar for it.

A click outside closes it; the icons in the bar toggle it.

## Consequences

- The top bar shows what phones show, and every quick setting is one
  click from it.
- The smoke test opens Control Center from the speaker, presses Next for
  Music, sets the level on the slider and mutes, switches the keyboard
  from its tile, and expects the desktop to have read the network.
- **Not yet:**
  - **Wi-Fi and Bluetooth:** Oceans has wired network drivers only; the
    icon is the wired one;
  - **the battery:** Oceans cannot read a battery yet (ACPI's battery
    methods need more of AML than `\_S5`); its icon will come with it;
  - the screen's brightness, Do Not Disturb, and choosing the modules;
  - the notification centre from the clock (its own ADR).

## Alternatives considered

- **Giving the desktop the network itself (`use = net`):** it would hold
  what opens sockets; a reader end is all it needs.
- **A Wi-Fi icon now:** it would claim what the system cannot do.
- **Keeping the sound panel beside Control Center:** two panels for the
  same speaker.

## Checklist (master spec §48)

- **Purpose:** quick settings and the system's state, one click from the
  top bar.
- **Architecture:** the display service draws Control Center as a
  function of the screen's width (`control_center`, `cc_module`,
  `cc_toggle`, `media_button`, `volume_track`, `volume_mute`,
  `status_icon`) and acts through Core and the window manager as before.
- **API:** `op::NET_READER`; `Hit::Toggle`, `Hit::Accent`.
- **Dependencies:** none (`oceans-net-proto`, already in the tree).
- **Security implications:** the desktop gets a reader end only: it sees
  the address and opens nothing; `NET_READER` needs `manage`, which apps'
  ends do not have. Every action is one the desktop could take before.
- **Testing strategy:** the boot smoke clicks Control Center's modules at
  their computed places and checks its colours and the desktop's log.
- **Failure behaviour:** without the network stack or Core, no network
  icon and no address; without a sound device, no slider.
