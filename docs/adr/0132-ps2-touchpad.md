# ADR-0132: The PS/2 touchpad (and mouse)

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0029 (the PS/2 keyboard), ADR-0042 (input events),
  ADR-0059 (the keyboard to the desktop)
- Part of Phase 4 (Hardware).

## Context

Most laptops' touchpads are PS/2 devices behind the same i8042 controller
as the built-in keyboard (the HP ZBook 15u G3's is a Synaptics). Oceans
read only the keyboard. On the ZBook, touching the touchpad left its byte
in the controller's one output register, raised on IRQ 12, which nothing
handled. From then on the keyboard's bytes could not get through: the
keyboard died too, and the machine could only be used with USB devices.

## Decision

### The controller in the kernel, both ports

The i8042 stays the kernel's (ADR-0029). Its auxiliary port is now
handled:

- At start, before anything is routed, the port is switched off, so a
  device the kernel cannot drive can never block the keyboard.
- Then the pointing device is set up through `0xd4`: set defaults; the
  IntelliMouse knock (sample rates 200, 100, 80; ID 3 means a wheel). Its
  interrupt (ISA IRQ 12, from ACPI) is routed to the keyboard's vector,
  and only then are its reports turned on (`0xf4`), so no byte arrives
  before something takes it.
- One handler drains both ports: key bytes to the console as before, the
  pointer's bytes to its decoder.

A touchpad in its PS/2 mouse mode is a mouse to it: moving, the buttons,
and tapping as its firmware turns taps into clicks. Gestures and
two-finger scrolling need the touchpad's own protocol (Synaptics,
ALPS, Elan), later work.

### `oceans_input::ps2`

The packets, host-tested:
- 3 bytes, or 4 with the wheel;
- bit 3 of the first byte always set, which lets a stream that lost a
  byte find its packets again;
- 9-bit signed motion, with overflowed axes dropped;
- Y turned to screen direction;
- the wheel as `Sample::wheel`.

`Tracker` makes them events, as for USB pointers.

### To the desktop: `DISPLAY_POINTER` (ABI 22)

The events queue in the kernel, at most 64, the oldest dropped, for the
keyboard's holder (the desktop, ADR-0059). Its notification is signalled
as for keys. The desktop reads them with `DISPLAY_POINTER` (encoded
`oceans_input::Event`s, device 255) and handles them as a USB pointer's.
Nobody else can read them.

## Consequences

- **Laptops' touchpads move the pointer and click,** and the built-in
  keyboard no longer dies when the touchpad is touched.
- **A PS/2 mouse works too** (QEMU's is found, with its wheel).
- Not yet: the touchpad's own protocol (multi-finger gestures, palm
  rejection), and settings for speed and tapping.

## Alternatives considered

- **A user-space PS/2 driver:** the keyboard and the touchpad share one
  controller and one output register. Two owners of the ports would race;
  moving the keyboard out of the kernel too is a larger change.
- **Through the `input` service (usb-hid):** it serves USB pointers, with
  no way for the kernel to hand it events; the desktop already holds the
  keyboard from the kernel.

## Checklist (master spec §48)

- Purpose: touchpads and PS/2 mice; a working keyboard beside them.
- Architecture: the kernel's i8042 handles both ports; events to the
  keyboard's holder.
- API: ABI 22, `DISPLAY_POINTER` (50); `display::MAX_POINTER_EVENTS`.
- Dependencies: the kernel uses `oceans-input` (no allocation).
- Security: only the keyboard's holder reads the events, as for keys;
  device bytes are untrusted and decoded without panics.
- Testing: host tests of the decoder; the smoke checks the PS/2 mouse is
  found and that the keyboard keeps working beside it; the real touchpad
  is the ZBook's.
- Failure: no device on the port, or no answer: the port stays off and
  the keyboard works.
