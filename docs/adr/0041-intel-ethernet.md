# ADR-0041: Intel Ethernet (82574L, e1000e)

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0016 (init), ADR-0021 (device capabilities),
  ADR-0023 (network, the `netdev` protocol)

## Context

The only network card Oceans could drive was virtio-net (ADR-0023): fine
in a virtual machine, absent from real machines. Phase 4 needs real
Ethernet hardware. The Intel 82574L is the obvious first one:

- a documented, widely deployed gigabit controller on PCI Express;
- QEMU emulates it faithfully (`-device e1000e`, PCI `8086:10d3`), so it
  can be tested on every change like the rest of the system;
- its programming model (legacy descriptor rings, `RCTL`/`TCTL`, NVM via
  `EERD`, MSI-X through `IVAR`) is shared, with small differences, by the
  rest of Intel's e1000 family, so later parts reuse most of it.

The network stack (`net`) speaks to its driver only through `netdev`:
`INFO`, `OPEN` (a shared frame buffer and a notification), `SEND`, `RECV`.
A second driver serving that protocol needs no change to the stack. What
is new is choosing between two drivers when either card may be present.

## Decision

### The controller logic (`libs/e1000e`, `oceans-e1000e`)

- Pure, `no_std`, no allocation, tested on the host:
  - **registers and bits** the driver uses (82574 datasheet §10), and the
    values it writes: `receive_control`, `transmit_control`,
    `TRANSMIT_IPG`, `interrupt_mask`, `ivar`;
  - **legacy descriptors**, `RxDescriptor` and `TxDescriptor`, encoded
    and decoded;
  - **receive outcomes:** `RxDescriptor::outcome` judges a written-back
    descriptor against its buffer size. Lengths and flags come from the
    device and are checked: runts, oversized frames, lengths beyond the
    buffer and frames flagged with CRC, symbol, sequence, carrier or data
    errors are dropped. `RxAssembler` drops a frame spanning several
    buffers through its last descriptor (impossible as configured, so a
    device doing it is misbehaving);
  - **ring bookkeeping:** `RxRing` and `TxRing` (below);
  - **NVM:** `EERD` requests and results, the checksum (words 0–0x3F sum
    to `0xBABA`), the MAC address in words 0–2;
  - **PHY access** through `MDIC` (the internal PHY at address 1);
  - **link status:** `Link::from_status` (speed, duplex).

### Rings

- **Receive:** 64 descriptors, each with its own 2048-byte DMA buffer.
  - All but one are the device's; `RDT` points at the one that is not.
  - The driver consumes completed descriptors in ring order. Each one
    consumed is re-armed (its own buffer address, status cleared) and
    becomes the new tail, which hands the previous tail back.
  - As in virtio-net, a completed descriptor keeps its frame until the
    stack asks (`RECV`). A stack that does not keep up makes the device
    drop frames (counted in `MPC`) rather than the driver queue them.
- **Transmit:** 32 descriptors and buffers.
  - A frame is copied from the session buffer into the next free buffer,
    and its descriptor (end of packet, insert FCS, report status) handed
    over by advancing `TDT`.
  - One descriptor always stays empty, so a full ring is never mistaken
    for an empty one.
  - Descriptors are reclaimed, oldest first, once the device sets
    `DONE`. A full ring answers `NoBuffers`: the frame is dropped, as a
    NIC drops it.

### The driver (`user/e1000e`)

- A service with exactly one device capability (`grant =
  device:8086:10d3`), the endpoint it serves (`provide = netdev`) and a
  log. It serves `netdev` exactly as virtio-net does. The stack, and every
  program above it, is unchanged.
- **Bring-up:**
  - **reset:**
    - interrupts are masked and receive and transmit stopped;
    - then PCIe master disable, waiting for requests in flight;
    - then a global reset (`CTRL.RST`), waiting for it to finish and for
      the NVM auto-read (`EECD.AUTO_RD`).
    - The firmware may have left the device running (OVMF loads a UEFI
      network driver for it), so nothing it set is trusted.
  - **82574 settings:** the bits Intel's initialisation sets after every
    reset (reserved `TXDCTL`, `TARC0`, `CTRL` and `CTRL_EXT` bits, and the
    `GCR`/`GCR2` PCIe completion erratum workaround).
  - **MAC address:**
    - the NVM's checksum must hold, or the driver refuses the device: a
      bad checksum means its configuration cannot be trusted;
    - the MAC is read from NVM words 0–2 (falling back to the address the
      device loaded into receive address 0) and programmed there.
    - The multicast table is cleared.
  - **link:** `CTRL.SLU`, with speed and duplex following
    autonegotiation (nothing forced). If the link is down, the PHY
    restarts autonegotiation through `MDIC`.
  - **rings:**
    - receive with broadcast accepted, CRC stripped, 2048-byte buffers,
      no long frames, no checksum offload (the stack checks its own);
    - transmit with short frames padded and the recommended collision
      settings and inter-packet gap.
- **Interrupts:**
  - MSI-X vector 0 is bound to a notification on the driver's endpoint, as
    in virtio-net and xhci: one thread serves the stack and the device.
  - `IVAR` routes receive queue 0, transmit queue 0 and "other" causes
    (link changes, overruns) to it.
  - Causes are cleared by writing them back to `ICR`, then the rings are
    read, so no frame is missed between the two.
  - Without MSI-X the driver polls every 10 ms.
- **Errors are never silent:**
  - bad frames and transmit failures are counted;
  - every 10 s at most, the counts and the device's own statistics
    (missed frames, CRC and length errors) are logged when they changed;
  - link changes are logged (`e1000e: link up, 1000 Mb/s full duplex`);
  - bring-up failures are logged and end the driver;
  - a device that stops answering (registers read all ones) ends it too,
    with an error, for init's restart policy;
  - no normal path panics: the remaining assertions guard register and
    DMA offsets the driver computes itself.

### Choosing the driver: device grant first

- Both drivers are configured in `services.conf`, virtio-net first, and
  both `provide = netdev`.
- **virtio-net (`netdev`)** keeps its order: `provide` before the device
  grant. The endpoint therefore exists even when no virtio NIC does (the
  stack then reports the device unavailable), as before.
- **e1000e** lists its device grant before `provide`:
  - **82574L absent:** init cannot open the device, so it registers
    nothing and logs `init: cannot start e1000e: NotFound`. `netdev`
    stays virtio-net's.
  - **82574L present:** its endpoint replaces virtio-net's in init's
    registry (as a restarted provider's would), so `net`, started after
    both, uses the Intel NIC.
- In short: whichever card is present serves the stack. When both are,
  the Intel NIC does: real hardware is preferred over the paravirtual
  device. virtio-net then runs without a client.
- **No new mechanism:** this is ordering in the manifest, using init's
  existing rules (grants in order; a failed grant starts nothing; a
  provider replaces an endpoint of the same name). It is visible in the
  log and documented in the manifest.

## Consequences

- Oceans drives a real gigabit Ethernet controller. In QEMU, a 1 MiB HTTP
  download over the 82574L takes 0.25–0.7 s depending on host load,
  comparable to virtio-net (0.3 s in the same release smoke run).
- The second smoke boot runs on the Intel NIC: DHCP, ping, DNS, TCP,
  HTTP, and the host's UDP and TCP echo probes into the guest, all through
  e1000e. So every change is tested on both drivers.
- `cargo xtask run` takes `OCEANS_NIC=e1000e` to boot with the Intel
  card.
- **Limitations:**
  - **One NIC is used.** With both cards present, virtio-net idles. The
    choice is made when `net` starts: a later restart of virtio-net would
    re-register `netdev`, and a `net` restarted after that would use
    virtio-net. Several interfaces need a stack that can hold several
    devices.
  - **Only the 82574L** (`8086:10d3`). Other e1000e-family parts (82583,
    I217/I218/I219) differ in PHY access, NVM layout and errata, and need
    their IDs and those differences added.
  - **Not yet implemented:**
    - interrupt moderation (`EITR`) and checksum or segmentation offload:
      the stack is not fast enough for them to matter yet;
    - a transmit-hang watchdog. A stalled ring fills and then drops
      frames, which the stack's retransmissions survive; it is not reset
      automatically.
  - **Untested on real hardware.** Tested only on QEMU's model. Real
    hardware is still the next step: the bring-up follows Intel's
    documented sequence, including the parts QEMU ignores (master
    disable, errata bits, PHY autonegotiation).

## Alternatives considered

- **A class grant (`device-class:020000`):** takes the first Ethernet
  controller of any make, which only works if one driver handles them
  all. Wrong for vendor-specific programming models.
- **One `netdev` service whose image depends on the hardware (or a
  probing "netdev manager"):**
  - it would need init to choose images by device, or a privileged
    service holding the device list and spawning drivers;
  - that is more mechanism, and more authority in one place, than an
    ordering rule.
  - Worth revisiting when hot-plugged or multiple NICs arrive.
- **Making the stack take several `netdev`s:** the right end state for
  multiple interfaces. It is a stack change (routing, per-interface
  configuration) larger than this driver, and not needed to support
  either card.
- **The driver exits when its device is absent:** it never gets that
  far, since init cannot open the device. Grant order gives the same
  result without starting a process.
- **Extended or advanced descriptors, multiple queues:** offloads and
  RSS that the stack cannot use yet. Legacy descriptors are the simplest
  correct format, and the same across the e1000 family.

## Checklist (master spec §48)

- **Purpose:** an Intel 82574L Gigabit Ethernet driver serving the
  existing `netdev` protocol, and a rule for which NIC serves the stack.
- **Architecture:**
  - the `oceans-e1000e` crate: registers, descriptors, rings, NVM, PHY,
    link;
  - the `e1000e` service, a userspace driver;
  - manifest ordering selects the NIC.
- **API:**
  - `netdev` unchanged (ADR-0023);
  - `oceans_e1000e::{RxRing, TxRing, RxDescriptor, TxDescriptor,
    RxAssembler, RxOutcome, Link, nvm, mdic, reg, receive_address,
    mac_from_receive_address, ivar, interrupt_mask, receive_control,
    transmit_control}`;
  - `OCEANS_NIC` for `cargo xtask run`.
- **Dependencies:**
  - `oceans-rt`, `oceans-net-proto`;
  - `oceans-virtio` for its DMA memory helper (`Dma`), as xhci uses it.
- **Security:**
  - the driver holds one device capability, one endpoint and a log;
  - it cannot reach other devices, the console or the filesystem;
  - descriptor contents written back by the device are bounds-checked
    before use, and re-armed descriptors always carry the driver's own
    buffer addresses;
  - frames from the stack are copied only from the session buffer's
    transmit area (checked).
  - No IOMMU yet (ADR-0021): as with every driver, the device itself can
    reach all memory.
- **Testing:**
  - host unit tests for:
    - control register values and interrupt routing;
    - receive address and NVM encoding, NVM checksum;
    - PHY commands and link decoding;
    - descriptor encoding and judging, multi-buffer frame dropping;
    - both rings (a device simulation checks the receive ring never
      posts the unposted descriptor; transmit wrap-around and full ring).
  - the smoke test's second boot swaps virtio-net for `-device e1000e`,
    and checks:
    - init cannot start virtio-net and starts e1000e;
    - MSI-X and link-up lines; `lspci` shows the driver attached;
    - DHCP configuration and `ifconfig`;
    - three pings answered; a DNS lookup;
    - a TCP exchange; HTTP fetches, including a 1 MiB download that the
      host then compares byte for byte;
    - the host's UDP and TCP echo probes into the guest.
- **Failure behaviour:**
  - device absent: init logs `cannot start e1000e` and virtio-net (if
    present) serves;
  - bring-up failure (reset timeout, NVM checksum, no valid MAC, no DMA
    memory): logged, the device left quiet, the driver exits with an error
    (init's restart policy applies);
  - device gone at runtime: logged, exit with an error;
  - bad or multi-buffer frames: dropped, counted and logged;
  - a full transmit ring: `NoBuffers` (the frame is dropped);
  - a slow stack: the device drops frames (counted, logged);
  - link loss: logged; frames wait in the transmit ring or are dropped
    when it is full.
