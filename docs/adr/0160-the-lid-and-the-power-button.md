# ADR-0160: The lid and the power button

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0085 (switching off and restarting), ADR-0110 (the lock
  screen), ADR-0148 and ADR-0153 (AML read and run; the firmware system
  calls), ADR-0152 (the battery's files)
- Part of Phase 10 (Alpha). ABI 29.

## Context

On a laptop, pressing the power button or closing the lid did nothing in
Oceans. macOS and Windows ask what to do when the button is pressed, and
lock (or sleep) when the lid closes. ADR-0153 left these, and the
embedded controller's events, for later.

## Decision

### Watched by the `acpi` service, polled

No interrupt is taken: the SCI stays masked, and the status bits stay
set until cleared, so the service looks every 200 ms.

- **ACPI mode.** At start, if the firmware still handles power events
  itself (SCI_EN clear in PM1 control), the service writes `acpi_enable`
  to the SMI command port and waits for SCI_EN, as the kernel already did
  before switching off.
- **The fixed power button** (the FADT says it has one): PM1's status
  register, bit 8, cleared by writing it back. Its enable bit is set,
  because some chipsets, QEMU among them, set the status only then.
- **A power button device** (`PNP0C0C`): pressed when the firmware
  notifies it (0x80).
- **The embedded controller's events**: while its status shows SCI_EVT,
  a query (0x84) gives the event's number, and `_Qxx` runs (at most 8
  each look). This is where firmware notifies the lid, a button device or
  a battery.
- **The lid** (`PNP0C0D`): its `_LID` every second, and at once when it
  is notified.
- **The FADT**: `FIRMWARE_TABLE` gives it at index `FIRMWARE_FADT`
  (0x1000, ABI 29), so the service knows where PM1's registers are and
  whether the button is fixed (`oceans_acpi::Fadt` reads PM1's event
  blocks, the button flag and the SCI's interrupt).
- The service now runs while there is anything to watch (a battery, a
  lid, a button, an EC) and logs what it watches:
  `acpi: watching: power button fixed, lid yes, embedded controller yes`.

### To the desktop

`/system/battery/events` (`oceans_window::battery::Events`, beside the
battery's status) holds `lid open` or `lid closed`, and `button N`, a
count of presses since the service started. It is written at start, so a
first press is seen as one. The desktop reads it twice a second:

- **A press** asks "Shut down Oceans?", Cancel or Shut Down, the dialog
  Launchpad's button already shows (ADR-0085), unless the screen is
  locked or another dialog is open.
- **The lid closed** locks the screen if there is a password (ADR-0110).
- **Escape** now answers no to any system dialog, as on other systems.

### The firmware's ports, kept tighter

A review asked for more of the ports a compromised `acpi` service could
reach to be kept from it. `FIRMWARE_PORT` now also refuses:

- the speaker and NMI control (0x61);
- the fast reset (0x92);
- the legacy disk controllers (0x170–0x177, 0x1f0–0x1f7, 0x376, 0x3f6);
- VGA (0x3c0–0x3df);
- any port a PCI device's I/O BAR decodes.

Port 0x80 stays open, because firmware writes POST codes there.

## Consequences

- **The ZBook**: the power button asks to shut down; closing the lid
  locks. Its EC's `_Qxx` handlers run, so the firmware's own
  notifications (the lid, the battery, the AC adapter) arrive.
- **The smoke tests**: QEMU's power button (`system_powerdown`) is pressed
  during the desktop test. The service logs it, the desktop's dialog
  shows (its pixel is checked), and Escape cancels it. The service's
  `watching` line is expected: a fixed button, no lid, no EC.
- **Host tests**: the FADT's new fields, and the events file's lines.
- Not yet: sleep (S3) when the lid closes, the brightness keys through
  `_Qxx` to the backlight (they arrive, as notifications of the display
  device, but nothing acts on them), GPE blocks (`_Lxx`, `_Exx`),
  and allowing the firmware only the ports it declares (its `SystemIO`
  regions would have to be known to the kernel first).

## Alternatives considered

- **Take the SCI** (an interrupt per event): faster than 200 ms, but the
  kernel would have to know GPE blocks and hand the interrupt to the
  service; polling is enough for a button and a lid.
- **Switch off at once on a press**, as some systems do: one press too
  many loses work; asking is what macOS and Windows do by default.
- **Sleep on the lid**: Oceans has no S3 yet; locking is the safe part
  of it.

## Checklist (master spec §48)

- **Purpose.** A laptop that answers its power button and its lid.
- **Architecture.** The `acpi` service polls and runs the firmware's
  handlers; a file to the desktop; the desktop's existing dialog and lock.
- **API.** `FIRMWARE_FADT` (ABI 29); `oceans_acpi::Fadt`'s PM1 event
  blocks, `fixed_power_button`, `sci_interrupt`;
  `oceans_window::battery::Events`.
- **Dependencies.** None.
- **Security implications.** No new right. The ports kept from the
  firmware grow, and a device's I/O BARs are refused. A press only asks;
  switching off still needs the user's click and the `power` right.
- **Testing strategy.** Host tests; QEMU's power button in every smoke
  test.
- **Failure behaviour.** Without a FADT or a fixed button, no button is
  watched; a failed `_Qxx` or `_LID` is skipped and tried again at the
  next look.
