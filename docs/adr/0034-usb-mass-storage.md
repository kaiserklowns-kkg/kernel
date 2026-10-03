# ADR-0034: USB mass storage and class drivers

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0021 (block protocol), ADR-0032 (xHCI), ADR-0033 (hubs)

## Context

USB sticks and external disks are the most common removable storage.
They speak the mass storage class: Bulk-Only Transport (BOT) carrying
SCSI commands. They must be readable and writable through the same block
protocol the filesystem and `disk` already use with virtio-blk.

Storage is also the first USB class that needs real data transfers, and
not the last: network adapters, audio and serial adapters will follow.
Putting every class into the xhci driver would grow one large process
with authority over every device. Class drivers should be separate
services that can reach only the interface they drive.

## Decision

- **Class drivers claim interfaces from xhci** (on its `usb` endpoint):
  - `CLAIM` names an interface class, subclass and protocol, and carries
    two things:
    - a buffer (64 KiB to 1 MiB) that xhci maps;
    - a notification for events.
  - xhci configures the first unclaimed matching interface. It answers
    with a session handle (a badged endpoint) and the interface's number,
    bulk endpoints and place.
  - With none present it answers `NOT_FOUND` and keeps the notification,
    signalling it when one appears. It also signals a claim's
    notification when the claimed device is unplugged.
- **On a session:**
  - **`BULK`** moves up to 64 KiB between the session buffer and one of
    the claimed bulk endpoints.
    - xhci copies through its own DMA bounce buffer (the class driver
      never sees physical addresses), queues one chained TRB per page,
      and reports the bytes moved, a stall, an I/O error, or `GONE`.
    - A short packet ends the transfer; the length comes from that event.
    - After a stall or error, xhci resets or stops the endpoint and moves
      the dequeue pointer past what is left.
  - **`CONTROL`:** class requests, but only to the claimed interface, and
    with IN or no data. This is enough for the BOT reset and Get Max LUN.
    Standard requests (addresses, configurations) stay with xhci.
  - **`CLEAR_HALT`:** `CLEAR_FEATURE(ENDPOINT_HALT)` on the device plus
    the controller-side reset.
  - Closing the session releases the claim.
- **SuperSpeed:** bulk endpoint contexts carry the burst size from the
  SuperSpeed companion descriptor.
- **`usb-storage`, a service with no device capability:**
  - **Grants:** `use = usb`, `provide = usbdisk`, a log.
  - **On a claim:**
    1. Get Max LUN (a stall is accepted).
    2. INQUIRY: a direct-access device is required.
    3. TEST UNIT READY, up to 20 attempts. REQUEST SENSE between them
       handles unit attention, and "no medium" stops the attempts.
    4. READ CAPACITY(10), falling back to (16).
  - **BOT:**
    - Every command block is tagged.
    - A data stage that stalls still reads its status wrapper.
    - A status wrapper is valid only with the right signature, tag and
      residue.
    - A phase error, an invalid wrapper or a transport error leads to
      reset recovery: a BOT reset, then both halts cleared.
  - **The block protocol on `usbdisk`:** the same operations as
    virtio-blk (`INFO`, `OPEN` sessions, `READ`, `WRITE`, `FLUSH`).
    - READ and WRITE use READ(10)/WRITE(10), or the 16-byte forms past
      2^32 blocks, 64 KiB per command.
    - FLUSH is SYNCHRONIZE CACHE; a refusal counts as "nothing to
      flush".
    - Only 512-byte blocks are served. Other sizes are reported by
      `INFO` and refused for transfers.
  - Without a stick, requests fail with an I/O error. After xhci
    signals, the driver checks its stick (an unplugged one answers
    `GONE`) or claims a new one.
- **`libs/usb`, host-tested:**
  - command and status wrappers, SCSI commands, and their replies
    (INQUIRY, capacities, sense);
  - BOT class requests;
  - the claim records;
  - TRB commands to reset, stop and re-point endpoints, and chained
    TRBs;
  - companion descriptors and finding bulk interfaces.
- **`disk` accepts any granted block service:**
  `run disk out use:usbdisk -- info`.

## Consequences

- **Sticks work as block devices.** A blank stick could carry an Oceans
  filesystem. The fs service leaves disks with other contents untouched,
  so mounting removable media safely is a matter of starting an fs
  instance on `usbdisk`. Mounting at run time (several filesystems under
  one root) is future work.
- **Copies:** data is copied twice inside the stack (client buffer to
  usb-storage, then the shared buffer to xhci's DMA buffer). That is
  simple and safe; zero-copy would need device-visible client buffers.
- **Not supported yet:**
  - FAT and other foreign filesystems;
  - LUNs other than 0 (card readers);
  - UAS (USB Attached SCSI);
  - write-protect detection (MODE SENSE);
  - blocks other than 512 bytes.
- **Per-session limits:** xhci serves one transfer at a time and blocks
  on it, up to 10 s for slow media. A keyboard behind the same
  controller waits that long in the worst case.
- **Future class drivers** (network, serial) reuse `CLAIM`. Matching is
  limited to the storage triple until another class driver exists.
- **Smoke test:** the session now takes about 55 s in a debug build. The
  host and kernel limits on it rose to 180 s.

## Alternatives considered

- **Storage inside xhci:** fastest to write, but one process would hold
  every device, and the next class would grow it again.
- **Handing a class driver the controller, or its rings:** a class driver
  could then reach other devices through DMA.
- **The block protocol directly on the `usb` endpoint:** the labels
  collide, and it would allow only one disk.
- **Polling for sticks:** wastes time when none is present, and reacts
  slower than the notification.

## Checklist (master spec §48)

- **Purpose:** USB sticks as block devices; a class-driver interface for
  USB.
- **Architecture:** xhci owns the controller and DMA and hands out
  interfaces. `usb-storage` speaks BOT and SCSI and serves the block
  protocol.
- **API:**
  - `CLAIM`, `BULK`, `CONTROL`, `CLEAR_HALT` and the `GONE`/`STALL`
    replies;
  - endpoint `usbdisk` (the block protocol);
  - `disk` takes any block service.
- **Dependencies:** none.
- **Security:**
  - the class driver has no device capability and no physical
    addresses;
  - its control requests are limited to class requests to its
    interface;
  - every buffer window is bounds-checked;
  - status wrappers are validated.
- **Testing:**
  - host tests: CBW/CSW (tag, signature, residue, phase error), SCSI
    command encoding (10- and 16-byte forms), INQUIRY, capacity and
    sense parsing, class requests, claim records, bulk-interface
    discovery with companions, endpoint TRBs, burst encoding.
  - smoke test: QEMU's USB 3 stick (port 3, 5 Gb/s) is claimed and
    identified. `disk` reads the host-written sector 0 and writes
    sector 1, which the host finds in the image. The stick is then
    unplugged: the driver notices, and `disk info` fails cleanly. The
    second boot reads sector 1 back.
- **Failure behaviour:**
  - transport errors trigger BOT reset recovery;
  - unplugging ends the claim, and requests fail with an I/O error
    until another stick appears;
  - a stick that never becomes ready, or has no medium, is logged and
    left unclaimed.
