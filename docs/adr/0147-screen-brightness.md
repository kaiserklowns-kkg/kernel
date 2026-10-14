# ADR-0147: The screen's brightness

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0021 (PCI and userspace drivers), ADR-0118 (Control
  Center), ADR-0100 (the sound's slider)
- Part of Phase 10 (Alpha).

## Context

On a laptop the screen's brightness could not be changed. Firmware
interfaces for it (ACPI `_BCM`) need an AML interpreter, which Oceans does
not have yet.

Intel graphics from Haswell to Kaby Lake (the PCHs of the 8, 9, 100 and
200 series) light the panel with a PWM the PCH generates, from two
registers in the graphics device's first BAR:

- `BLC_PWM_PCH_CTL1`, which enables it;
- `BLC_PWM_PCH_CTL2`, with the period and the duty cycle.

The firmware has set both by the time Oceans starts. The test laptop (an
HP ZBook 15u G3: Skylake, 8086:1916) is one of these.

There is a catch. Closing a PCI device turned its memory decoding off.
The firmware's framebuffer, which the desktop draws on, is behind the
graphics device's BARs, so a brightness driver that stopped (at shutdown,
or after a crash) would have frozen the screen.

## Decision

- **The kernel:** a display controller (class 03) whose memory decoding
  was on at boot keeps it on when closed. Bus mastering, the DMA that
  closing must stop, still goes off for every device.
- **The `backlight` service** (`grant = device-class:030000`):
  - It works on Intel graphics device IDs `04xx`, `0Axx`, `0Dxx`, `16xx`,
    `19xx` and `59xx`, and only if the firmware left the PWM on with a
    period. It writes only the duty cycle, never below 5 % (a panel at 0
    looks switched off).
  - Anywhere else it logs why (`backlight: none on this display (V:D)`)
    and answers that brightness is not available.
  - Its protocol (`oceans_window::proto::backlight`) is `GET`
    (percent, available) and `SET`.
  - Its `provide` comes before its device grant, so the name exists even
    without a display device. The desktop's `use = backlight` therefore
    always works, live or not.
- **Control Center** gets a brightness module under the sound's, with a
  sun and a slider, only where brightness is available. The panel grows
  to fit it, and dragging the slider sets the brightness at once.

## Consequences

- On the ZBook the slider should appear and work. Its log says the PWM's
  period and the level found, which the next update will show.
- The smoke test runs on QEMU's standard VGA, so it checks the service's
  refusal (`backlight: none on this display (1234:1111)`) and that the
  desktop starts with it. Control Center's layout there is unchanged.
- Not yet:
  - Coffee Lake and later (another register layout);
  - AMD and NVIDIA;
  - ACPI `_BCM`;
  - the keyboard's brightness keys (often ACPI events);
  - remembering the level across boots (the firmware's is used).

## Alternatives considered

- **ACPI `_BCL`/`_BCM`:** the portable way, once there is an AML
  interpreter (needed for the battery too).
- **A graphics driver:** far larger; the PWM is two registers.

## Checklist (master spec §48)

- Purpose: brightness on laptops.
- Architecture: a kernel rule, a service, Control Center.
- API: the `backlight` endpoint.
- Dependencies: none.
- Security: the service holds only the display device. It writes one
  register field, clamped.
- Testing: the smoke test (refusal path); the ZBook (hardware path).
- Failure: unknown hardware gets no slider; a stopped service leaves the
  screen as it was.
