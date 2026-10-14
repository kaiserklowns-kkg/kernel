# ADR-0144: The wheel of a USB mouse

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0042 (USB mice and pointer input), ADR-0138 (scrolling
  with the wheel)
- Part of Phase 4 (Hardware).

## Context

ADR-0138 made the wheel scroll what the pointer is over. But `usb-hid`
put every boot mouse (subclass 1, protocol 2: nearly every USB mouse) in
the **boot protocol** (ADR-0042) and decoded its fixed report:

- **The boot report.** HID 1.11 appendix B.2 defines it as three bytes:
  the buttons, X and Y.
- **The wheel.** Many mice send no wheel in it at all. Others send it in a
  fourth byte only by convention.
- **Why the tests missed it.** QEMU's mouse sends the fourth byte, so the
  smoke tests saw a wheel that real mice need not send.

The wheel was therefore a matter of luck. On a laptop with a USB mouse,
the scrolling ADR-0138 brought may simply not work.

## Decision

- **Every pointer by its descriptor.** `usb-hid` now reads every pointer's
  report descriptor first, boot mice included. With a layout it can use,
  it asks a boot mouse for the **report protocol** (`SET_PROTOCOL 1`,
  `Setup::hid_set_report_protocol`) and decodes its reports by the
  descriptor (`oceans_usb::pointer::Layout`): the buttons, X, Y, the wheel
  and the horizontal wheel the mouse says it has.
  - The device may have been left in the boot protocol by the firmware;
    the request also covers that.
- **The boot protocol, as a fallback.** It is used only when:
  - the descriptor cannot be read or used;
  - or the mouse refuses the report protocol.
- **Logs and the smoke tests.** The log says what was found, e.g. `mouse,
  5 buttons, wheel, pointer 2`. The smoke tests expect that for QEMU's
  mouse, and still see its wheel reach the input service.

## Consequences

- **Wheels.** USB mice scroll in the report protocol, their wheel read
  from where their descriptor puts it. Tilt wheels (pan) come too where
  the descriptor has them.
- **Broken descriptors.** A mouse whose descriptor is wrong but whose
  boot report is right falls back to the boot protocol, as before.
  - **Not covered:** a descriptor that parses but lies. A list of such
    devices can be added when one is met.
- **ADR-0042's choice.** Boot mice used to go to the boot protocol first;
  now that is only the fallback.

## Alternatives considered

- **Keep the boot protocol and read a fourth byte when it comes.** That is
  what was done: it depends on the mouse.
- **The report protocol for every mouse, without the descriptor.** The
  report's layout is the descriptor's to say. The parser exists
  (ADR-0042) and is host-tested.

## Checklist (master spec §48)

- **Purpose.** The wheel on real USB mice.
- **Architecture.** `usb-hid` decides the protocol per pointer; decoding
  is unchanged.
- **API.** `Setup::hid_set_report_protocol`.
- **Dependencies.** None.
- **Security implications.** None: the same reports, read by the same
  bounded parser.
- **Testing strategy.** The setup packet is host-tested. The smoke tests
  plug QEMU's mouse in and out and turn its wheel, now in the report
  protocol.
- **Failure behaviour.**
  - Refused or unusable: the boot protocol, as before.
  - Neither works: the pointer's reports are ignored, and the log says so.
