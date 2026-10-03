# ADR-0033: USB hubs

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0032 (xHCI driver)

## Context

ADR-0032 saw only devices on root ports. Real machines have hubs
everywhere: inside laptops (internal USB 2 hubs serve the webcam and
Bluetooth), in docks, keyboards and monitors, and as separate boxes.
Without hub support most devices are invisible.

Hubs are a standard class (09). The host controller must be told about
them in the slot context. Devices behind hubs are addressed by a route
string. Low- and full-speed devices behind a high-speed hub travel
through that hub's transaction translator (TT), which the controller
must also be told about.

## Decision

- **Hub support lives in the xHCI driver.** Enumeration needs the hub's
  port control and the controller's slot contexts together. A separate
  hub service would need a protocol for both and would gain no isolation:
  the xhci driver already controls every device.
- **Devices are identified by their place,** not by a root port: root
  port, route string (4 bits per tier, at most 5 tiers), depth, and
  parent hub and port.
  - Each device has a table entry (16 entries) whose DMA pages are reused
    by the next device in that entry.
  - `lsusb` and the log show places as `ROOT.PORT.PORT`, for example
    `6.1`.
- **Configuring a hub:**
  1. Read the hub descriptor: type `0x29`, or `0x2a` for USB 3 hubs.
  2. Select the configuration, then set the hub depth (USB 3 only).
  3. Tell the controller that the slot is a hub: the hub flag, the port
     count, and the TT think time.
  4. Configure the hub's status-change interrupt endpoint and keep two
     reports queued on it.
  5. Power every port, wait the hub's power-good time (at least 100 ms),
     then look at each port.
- **Ports:**
  - On a change, the driver reads `GET_STATUS` and clears every reported
    change.
  - On a connect: reset the port, wait for the reset to finish (up to
    500 ms), wait 10 ms for recovery, take the speed from the port
    status, then enumerate the device as on a root port.
  - On a disconnect, the device is forgotten.
- **Transaction translators:**
  - A low- or full-speed device directly behind a high-speed hub gets
    that hub's slot and port as its TT.
  - Deeper ones inherit their parent's TT.
  - Multi-TT hubs are used in their default single-TT mode (alternate
    setting 0).
- **Hot plug:**
  - A hub's status-change report marks ports pending. They are handled
    after the event ring is drained, never while another transfer waits.
  - Removing a hub first removes everything behind it, depth first.
- **Limits:** ports 1–15 of each hub (route strings cannot name more),
  five tiers, 16 devices.
- **Tests:** xtask now waits until the boot devices are enumerated
  before typing. A late enumeration log line could otherwise split the
  first commands' output. The kernel's smoke-mode limit on the scripted
  session rose to 90 s, the same as xtask's.

## Consequences

- Keyboards and other devices behind hubs work, including hot plug at
  any depth.
- **Not exercised by QEMU:** QEMU's only hub is full speed, so the
  smoke test does not exercise the high-speed TT path or USB 3 hubs.
  Both follow the xHCI and USB specifications, and the slot-context
  encoding is host-tested, but they are unproven on hardware.
- **No port power management:** no suspend, over-current recovery, or
  per-port power switching beyond powering all ports at start. An
  over-current change is cleared and logged through the normal path.

## Alternatives considered

- **A separate hub driver service:** see above. It adds a protocol and
  gains no isolation.
- **Polling hub ports instead of using the interrupt endpoint:** simpler,
  but slower to react, and wasteful.
- **Multi-TT mode:** more bandwidth for many full-speed devices on one
  hub. Not worth the alternate-setting handling yet.

## Checklist (master spec §48)

- **Purpose:** devices behind hubs.
- **Architecture:** inside the xhci service; protocol pieces in
  `oceans-usb` (`hub` module, slot contexts with route, TT and hub
  fields).
- **API:**
  - `lsusb` shows places (`6.1`);
  - `LIST` pages with a skip count;
  - records carry the route string.
- **Dependencies:** none.
- **Security:**
  - descriptors and port status are parsed defensively;
  - depth and port counts are bounded;
  - removing a hub removes its devices.
- **Testing:**
  - host tests for hub descriptors (USB 2 and 3), port status and change
    features, requests, route strings, change bitmaps, slot contexts and
    path display;
  - the smoke test boots with QEMU's hub and a tablet behind it (`6.1`),
    hot-plugs a mouse into the hub (`6.2`), lists and removes it, and
    then removes the hub with its tablet.
- **Failure behaviour:**
  - a port that fails to reset or enumerate is logged and left alone;
  - the hub and its other ports keep working.
