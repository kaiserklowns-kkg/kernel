# ADR-0042: USB mice and pointer input

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0006 (security model), ADR-0032 (xHCI), ADR-0033
  (hubs), ADR-0034 (class drivers)

## Context

The xHCI driver types boot keyboards into the console, but mice and
tablets were only listed: `mouse (no driver)` and `no driver`. Phase 4
needs mouse support, and the desktop of Phase 7 needs pointer events:
relative motion from mice, absolute positions from tablets and touch
screens, buttons and wheels, each with the time it happened.

Pointer input is also sensitive. Whoever reads it sees what the user
does, and a future compositor must be able to hold it alone. Reading it
must therefore take a capability that is granted on purpose (ADR-0006),
not something every program can open.

Two questions follow:

- **Where the driver lives.** Keyboards are driven inside xhci because
  they feed the console's input, which xhci holds anyway (ADR-0032).
  Storage became a class driver (ADR-0034): a separate service that is
  handed one interface and holds no device capability.
- **How much of HID to understand.** Boot mice send a fixed 3-byte
  report (buttons, X, Y), and most add a wheel byte. Tablets, QEMU's
  `usb-tablet` included, have no boot protocol: their reports are
  described only by the report descriptor (HID 1.11 §6.2.2).

## Decision

- **A class driver, `usb-hid`, in the model of ADR-0034.**
  - **Grants:** `use = usb`, `provide = input`, a log. No device
    capability.
  - xhci identifies pointers and hands them over. The class driver
    decodes reports and serves events.
- **xhci identifies pointers** while it enumerates a device:
  - Every HID interface (alternate setting 0, with an interrupt IN
    endpoint) is looked at, with the report descriptor length from its
    HID descriptor.
  - Boot keyboards stay with xhci.
  - A boot mouse (subclass 1, protocol 2) is a pointer.
  - Otherwise xhci selects the configuration, reads the report
    descriptor (at most 1 KiB) and parses it with
    `oceans_usb::pointer::Layout`. A mouse or pointer collection with X
    and Y makes the interface a pointer: a **mouse** if X and Y are
    relative, a **tablet** if they are absolute.
  - xhci never interprets reports; it only queues them.
  - `lsusb` shows the kind, and whether a class driver holds the device
    (`Record` kind byte, bit 7): `mouse (pointer input)`,
    `tablet (pointer input)`, or `… (no driver)`.
- **The claim protocol (ADR-0034), extended:**
  - **`CLAIM` with `POINTER`**, the pseudo triple `(3, 0xff, 0xff)`
    (HID has no such subclass or protocol), hands over the first
    unclaimed pointer. A pointer's buffer may be as small as 4 KiB
    (`MIN_POINTER_BUFFER`): it only receives the report descriptor.
    Watchers of `POINTER` are signalled when one is plugged in.
  - **`Claim`** grows to 20 bytes. It adds the interrupt IN endpoint,
    the interface's subclass and protocol, and the report descriptor's
    length.
  - **`CONTROL`** on a pointer's session also allows the standard
    `GET_DESCRIPTOR(Report)` to the claimed interface, and nothing else
    beyond the class requests already allowed. `BULK` and `CLEAR_HALT`
    are refused on pointer sessions.
  - **`REPORTS`** (new): the reports xhci has received since the last
    call, oldest first, as many as fit in one reply (240 bytes). Each
    report carries the clock time of its completion (milliseconds). A
    `lost` byte counts reports dropped because the queue was full.
    - The first call configures the interrupt endpoint and starts
      polling. The class driver therefore sends `SET_PROTOCOL` and
      `SET_IDLE` first.
    - Polling then continues until the device goes.
    - Every report arriving signals the claim's notification.
    - Up to 4 pointers are polled at once, each with an 8-report
      queue. The queues live in the controller, not in every device:
      xhci has a 64 KiB stack.
  - xhci selects a device's configuration only once (`configure_device`).
    The slot context's Context Entries now covers every configured
    endpoint, so a keyboard and a pointer on one device can coexist.
- **`usb-hid`:**
  - It claims pointers until xhci answers `NOT_FOUND` or its 4 slots
    are full. One shared 4 KiB memory object serves every claim.
  - **A boot mouse** is set to the boot protocol (`SET_PROTOCOL 0`) and
    decoded as buttons, X, Y and, where the device sends a fourth byte,
    the wheel (`pointer::boot_report`). This is how most systems read
    boot mice, and it works where the report descriptor is wrong. If
    the boot protocol is refused, the report descriptor is used.
  - **Any other pointer:** its report descriptor is read through
    `CONTROL` and parsed. The resulting `Layout` locates the fields:
    - the report ID, if the device numbers its reports;
    - up to 32 buttons;
    - X and Y with their logical range;
    - the wheel (Generic Desktop Wheel), and the horizontal wheel
      (Consumer AC Pan), relative only.
  - `SET_IDLE(0)` is sent too (reports only on change); a stall is
    accepted.
  - A pointer it cannot decode keeps its claim, so it is not claimed
    over and over, but its reports are dropped. This is logged.
  - When xhci signals, the driver fetches every pointer's reports until
    the queue is empty. A `GONE` answer ends that pointer. Then it
    claims again if it has room.
  - `oceans_input::Tracker` turns successive samples into events. It
    reports only what changed, in this order: motion or position first
    (so a click lands where the pointer now is), then buttons in
    ascending order, then the wheel. An absolute pointer's first report
    always gives its position.
- **Events (`libs/input`, `oceans-input`), host-tested:**
  - `Event { time_ms, device, kind }`. `device` numbers pointers in the
    order they arrive.
  - The kinds:
    - `Motion { dx, dy }`;
    - `Absolute { x, y, x_max, y_max }`: the position in
      `0..=max`, so clients scale by the range;
    - `Button { button, pressed }`: 1 left, 2 right, 3 middle (HID
      usages);
    - `Wheel { vertical, horizontal }`.
  - The wire format is a fixed 32 bytes. Decoding refuses unknown kinds
    and values no encoder makes.
- **The input protocol (`user/input-proto`, `oceans-input-proto`):**
  - **`SUBSCRIBE`** on the service endpoint: carries a notification and
    the bits to signal, and returns a subscription (a badged endpoint).
    At most 4 subscriptions.
  - **`READ`** on a subscription: returns `[lost u32]` and up to 7
    events. An empty list means none are waiting.
  - Each subscription has a 64-event queue. A client that falls behind
    loses its oldest events and is told how many. The service never
    blocks on a client.
  - Every subscriber gets every event. Focus and exclusive grabs are
    the compositor's job; it will hold the only `input` capability.
- **`mouse [COUNT [SECONDS]]`** in `user/utils`:
  - prints events (`28080 ms  pointer 2: button 1 (left) down`) until
    COUNT have arrived (default 10), or until SECONDS pass (default 30),
    then fails with a message;
  - its manifest requests `use:input`, which is not low-risk, so a bare
    `mouse` is refused: run it as `run mouse out use:input -- 5`.
- **Grants:** `services.conf` and `services-smoke.conf` gain `usbhid`.
  The shell gets `use = input` (it passes it on only when typed), and
  fs publishes `/bin/mouse`.

## Consequences

- **What works:**
  - USB mice and tablets, behind hubs too;
  - plugging them in and out while running;
  - several at once, told apart by `device`;
  - a compositor can consume pointer events through a single
    capability.
- **xhci stays small.** It parses report descriptors only to decide
  what a device is. Decoding, state and clients are in `usb-hid`, which
  can only reach the interfaces it was handed.
- **Latency:** each report wakes xhci, then `usb-hid` (one IPC), then
  the subscriber. That is small against the 10 ms clock that stamps
  events.
- **Not supported yet:**
  - **keyboards in `usb-hid`:** they stay in xhci for the console;
    keyboard events for a compositor are future work on this protocol;
  - **touch screens with contacts** (Digitizer page), pens, joysticks
    and gamepads;
  - **combined devices:** a device's first pointer interface is
    offered (next to a boot keyboard that xhci keeps typing with), and
    only one interface per device can be claimed at a time;
  - **pointers on a report ID other than the first pointer fields', and
    array-style button fields;**
  - **wheels with an absolute logical range, and resolution
    multipliers;**
  - **PS/2 mice:** the protocol takes any source, but no PS/2 driver
    feeds it yet.
- **QEMU routes monitor input to the pointer the guest began polling
  last.** QEMU activates a USB pointer's input handler when the guest
  first polls it, and relative motion goes to the most recently
  activated relative pointer. The smoke test relies on this: the
  hot-plugged mouse receives `mouse_move` and `mouse_button`. Once it
  is removed, the tablet receives the buttons.

## Alternatives considered

- **Pointers inside xhci, like keyboards:**
  - fewer IPC hops;
  - but xhci would grow decoding, per-client queues and a second served
    endpoint;
  - and the process holding the controller would also hold the
    user's input.
  - Keyboards are there only because of the console.
- **Matching pointers by interface triple:**
  - asking for boot mice `(3, 1, 2)` and for every non-boot HID
    interface `(3, 0, 0)`, and letting the class driver sort them out;
  - but it would claim keyboards' consumer-control interfaces,
    gamepads and security keys, and could not hand them back without
    looping.
  - Letting xhci identify pointers gives the class driver only what it
    drives (least privilege), and gives `lsusb` the right name.
- **Boot protocol only:**
  - simplest;
  - but it gives no tablets (QEMU's has no boot interface), no
    horizontal wheel and no extra buttons.
  - The report descriptor parser is about 300 lines without
    allocation, and is host-tested against QEMU's tablet and a packed,
    report-ID mouse.
- **Report protocol for boot mice too:**
  - one decoder for every device;
  - but boot mice with wrong descriptors exist, and the boot format is
    what the HID specification guarantees.
- **Reports through the claim's shared buffer as a ring, instead of
  `REPORTS` replies:**
  - saves a copy;
  - but needs a shared-memory protocol with indices both sides trust.
  - Pointer reports are a few bytes at most a few hundred times a
    second: an IPC reply is simpler and enough.
- **Blocking reads (a reply held until events arrive):** the services
  are single-threaded and reply in order. Notifications plus
  non-blocking reads keep the service from ever waiting on a client.
- **Events with only changed axes (separate X and Y events):**
  - closer to evdev;
  - but motion is naturally a pair, and the range belongs with an
    absolute position.

## Checklist (master spec §48)

- **Purpose:** USB mice and tablets, and an input event service a
  compositor can consume through a capability.
- **Architecture:**
  - xhci identifies pointers (boot mouse, or report descriptor) and
    queues their reports;
  - `usb-hid` (no device capability) claims them, decodes reports into
    events and serves them on `input`;
  - `mouse` prints them.
- **API:**
  - USB service: `CLAIM` with `POINTER`, `REPORTS`, `Claim` (20 bytes),
    `Record::claimed` and `role`, `Kind::Tablet`, `MIN_POINTER_BUFFER`,
    `ReportWriter`/`Reports`;
  - `oceans_usb::pointer::{Layout, Field, Error, boot_report}`;
  - `descriptor::{find_hid_interfaces, HidInterface, Item::Hid}`;
  - `Setup::hid_get_report_descriptor`;
  - `oceans_input::{Event, Kind, Sample, Axes, Tracker, button_name}`;
  - `oceans_input_proto::{op::SUBSCRIBE, op::READ, Subscription, Batch}`;
  - endpoint `input`; `mouse [COUNT [SECONDS]]`.
- **Dependencies:** none (QEMU's `mouse_move` and `mouse_button`
  monitor commands for the smoke test).
- **Security:**
  - reading pointer input requires the `input` endpoint, granted
    explicitly: the shell holds it and passes it only when typed;
    `mouse`'s manifest request is not low-risk;
  - `usb-hid` holds no device capability and can claim only interfaces
    xhci identified as pointers;
  - its control requests are limited to class requests and the report
    descriptor of the claimed interface; bulk requests are refused;
  - report descriptors and reports are untrusted. Parsing is
    bounds-checked:
    - fields are at most 32 bits;
    - reports at most 64 bytes;
    - collections nest at most 16 deep;
    - PUSH at most 4 deep.
    Truncated or malformed descriptors are refused, never guessed at.
    Absolute positions are clamped into their range.
- **Testing:**
  - **host tests:**
    - report descriptor parsing: QEMU's tablet; a mouse with report
      IDs, PUSH/POP and 12-bit packed X/Y; a keyboard (not a pointer);
      every truncation of the tablet's; malformed items, unbalanced
      collections, 33-bit fields, reserved item types, mixed
      relative/absolute axes, long items;
    - report decoding: signed fields, clamping, wrong report ID, short
      reports;
    - boot reports;
    - HID interface discovery: a combo keyboard/mouse receiver and the
      tablet;
    - the report descriptor request;
    - claims and records (claimed bit, roles);
    - `REPORTS` replies (writer, reader, truncation);
    - event wire format, and refusals of malformed records;
    - the tracker: motion, buttons, wheel and pan order, absolute
      first position and clamping.
  - **smoke test:**
    - the tablet behind the hub is identified and claimed at boot
      (`tablet, 5 buttons, wheel, 0..32767 x 0..32767, pointer 1`);
    - a hot-plugged boot mouse is claimed (`mouse, boot protocol,
      pointer 2`);
    - `lsusb` lists both with `(pointer input)`;
    - a bare `mouse` is refused for want of `use:input`;
    - `run mouse out use:input -- 4 20` receives `motion dx=10 dy=-5`,
      `button 1 (left) down`, `button 1 (left) up` and
      `wheel vertical=1 horizontal=0`, injected through the monitor
      while it runs (xtask's new `@when TEXT` script step);
    - the mouse is unplugged and `usb-hid` notices;
    - the tablet then delivers `absolute x=0 y=0 (of 32767 x 32767)`
      and right button down and up.
- **Failure behaviour:**
  - an unplugged pointer answers `GONE` and is forgotten; its
    subscribers simply stop getting its events;
  - an unreadable or unparsable report descriptor is logged. xhci then
    treats the interface as no pointer, and `usb-hid` keeps the claim
    but drops the reports;
  - a failed interrupt transfer is logged with its completion code;
  - full queues drop the oldest reports or events and say how many
    (`lost`), in xhci, in `usb-hid` and per subscription;
  - more than 4 pointers or subscriptions are refused (`IO_ERROR` from
    `REPORTS`, `NoSpace` from `SUBSCRIBE`).
