# ADR-0123: Ethernet for real PCs: Intel I219, I210, I225/I226 and Realtek RTL8111/8168

- Status: Accepted
- Date: 2026-10-12
- Depends on: ADR-0041 (Intel 82574L, e1000e), ADR-0023 (the `netdev`
  protocol), ADR-0021 (device capabilities), ADR-0068 (hardware
  validation)
- Part of Phase 4 (Hardware) and Phase 10 (Alpha).

## Context

The only Ethernet on real hardware was the Intel 82574L (ADR-0041), an
add-in card. The alpha's release checklist asks for a real Tier 1
machine to boot and pass validation, and almost none has an 82574L. What
PCs have, by far most often:

- **Intel I219** (and the older I217/I218): the MAC inside Intel's
  platform controller hub, on nearly every Intel board since 2013.
- **Intel I225-V/I226-V** (2.5 Gb/s): on recent desktop and mini-PC
  boards; and the **I210/I211** on servers and older boards.
- **Realtek RTL8111/8168**: on most boards whose Ethernet is not Intel's.

None of them is emulated by QEMU, except an ancestor of the I210/I225:
QEMU's `igb` is an 82576.

Specs were written from the BSD drivers (FreeBSD's `e1000_ich8lan.c`,
`if_em.c`, `if_re.c`; OpenBSD's `igc`, `re`), whose licences allow it,
and QEMU's igb model; facts only, the code here is new.

## Decision

### What the drivers share: `oceans-nic`

The netdev side of a driver (one session, `INFO`/`OPEN`/`SEND`/`RECV`,
copying frames between the stack's buffer and the driver's own DMA
buffers, the interrupt or polling notification on the endpoint) moves out
of e1000e into a crate, `oceans-nic`, with the register-BAR access and
waiting. A driver implements `Nic`: `transmit`, `receive` into `Frames`,
`service` for its interrupt causes, `frames_waiting`. e1000e is the first
user, unchanged in behaviour; igc and rtl8168 are the others.

### One driver for a family: `grant = device:A,B,...`

A device grant may list several IDs: init opens the first one present
(`NotFound` lets it try the next; any other refusal stops it). Without
any of them the service does not start, as before. No ABI change.

### Intel I217/I218/I219: in e1000e

The rings and descriptors are the 82574's; what differs is the bring-up
(`user/e1000e/src/pch.rs`, the facts in `oceans_e1000e::pch`, host-tested):

- the PHY's configuration gated; ultra low power left through the ME
  (`H2ME`) or, without one, the PHY's own registers; SMBus mode unforced;
  the PHY power-cycled (`LANPHYPC`) if it does not answer;
- every PHY access under the software flag the ME shares, with the PCH's
  paging (page 0 at address 2, pages from 768 at address 1);
- MAC and PHY reset together, the 20 ms that must pass without a register
  read, the configuration-space read around it; on Sunrise Point and later
  the descriptor rings drained first when the PCH asks, through a ring of
  the driver's own;
- the address from receive address 0 (the PCH loads it from its flash);
- the settings Intel's drivers apply after reset, per generation (Sunrise
  Point's `IOSFPC` and `TARC0`, Tiger Lake's DMA clock, Meteor Lake's K1
  timeout), and those that follow the link (`TIPG`, the PLL clock gate).

63 device IDs, from Lynx Point to Nova Lake.

### Intel 82576, I210/I211, I225/I226: `igc`

A new driver (`user/igc`, the logic in `oceans-igc`). The three families
share the queue registers, advanced descriptors and MSI-X routing, and
differ where `Family` says: the reset bit (`DEV_RST` on the I225), the
interrupt cause registers (moved on the I225), the PHY's MDIO address,
2.5 Gb/s. On the I225 the PHY's own link-down saving and EEE are turned
off, as Intel's driver does. Interrupts: MSI-X vector 0 for both queues
and the link. Blank-NVM parts get a locally administered address.

### Realtek RTL8111/8168: `rtl8168`

A new driver (`user/rtl8168`, the logic in `oceans-rtl8168`). The
revision (8168B to 8411B) comes from `TxConfig`; unknown ones are
refused. Reset, the address from the ID registers, C+ mode, the rings,
the 8168G-on receive gate opened, the PHY reset and autonegotiating. No
microcode: BSD's drivers show the 8168 works without the blobs Linux
loads. PCIe parts can miss a transmit kick, so the driver kicks again
while frames are queued. MSI-X where the part has it, else polling.

## Consequences

- **What is tested where:**
  - igc runs in CI on QEMU's `igb` (the smoke test's sixth boot: DHCP, a
    fetch of 1 MiB, the host's UDP and TCP probes).
  - e1000e's 82574L path runs in CI as before, now through `oceans-nic`.
  - **The I219 path and rtl8168 are not emulated anywhere.** Their pure
    logic (IDs, generations, revisions, PHY addressing, descriptors, link
    tuning) is host-tested; the drivers need real machines, and reports
    (`sysreport`, compatibility.md).
- **Interrupts:** the kernel binds MSI-X only (ADR-0021). The I219 has
  MSI and no MSI-X, so e1000e polls it every 10 ms, as the 82574L without
  MSI-X; so do 8168s without MSI-X. MSI is later work.
- **Not done:**
  - RTL8125 (2.5G Realtek): its bring-up needs MAC and PHY microcode
    tables, a decision of its own.
  - PCIe ASPM: BSD's re(4) turns it off (it stalls some 8168s). Drivers
    cannot write PCI configuration space, so it stays as the firmware left
    it; the transmit re-kick is the mitigation.
  - The I219's NVM extended PHY settings (in the PCH's flash) are not
    replayed; the driver says so when the NVM has them.
  - The 8168B (2006), whose address is only in its serial EEPROM, starts
    only if the firmware left it in the ID registers.
  - Fibre and SerDes variants of the 82576 and I210; the 810x
    fast-Ethernet parts; the I225's PHY semaphore with firmware (consumer
    boards have no manageability firmware on it).
- `sysreport` and the compatibility matrix name the new drivers;
  `grant = device:` lists keep each manifest's driver the first match.
