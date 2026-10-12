# ADR-0125: Writing PCI configuration space, the safe fields only (ABI 19)

- Status: Accepted
- Date: 2026-10-12
- Depends on: ADR-0021 (device capabilities), ADR-0123 (Ethernet for real
  PCs), ADR-0124 (MSI)
- Part of Phase 4 (Hardware) and Phase 10 (Alpha).

## Context

Drivers could read configuration space (`DEVICE_CONFIG_READ`) but not
write it. ADR-0123 had to leave PCIe active-state power management (ASPM)
as the firmware set it. BSD's re(4) turns it off on the Realtek 8168,
whose transmitter stalls on many boards with it on. Other drivers will
want the same field, or the maximum read request size (larger DMA reads,
for jumbo frames).

Configuration space is also where a device is pointed at memory and
interrupts:
- the BARs;
- the command register (bus mastering, decoding);
- MSI and MSI-X;
- a bridge's windows.

A driver that could write those could aim a device's DMA at any memory,
which the device capability (ADR-0021) exists to prevent. The kernel
programs those registers itself.

## Decision

- **`DEVICE_CONFIG_WRITE`** (49, ABI 19):
  - The call is `(device, offset, width, value) -> 0` and needs `MANAGE`
    on the device.
  - The kernel writes only the bits `oceans_pci::writable` allows, keeping
    the register's other bits (a read-modify-write under the function's
    lock).
  - Anything else is refused with `MissingRights`.
- **What may be written** (PCI Express capability, 16-bit accesses):
  - **Link Control:** ASPM L0s and L1 (bits 1:0), clock power management
    (bit 8).
  - **Device Control:** relaxed ordering (bit 4), no snoop (bit 11), the
    maximum read request size (bits 14:12).
- **Nothing else:** not the BARs, the command register, MSI, MSI-X,
  power-state control, bridges, or vendor-specific space. The list grows
  only by an ADR, field by field.
- `oceans-nic` gains `pcie_capability` (the capability list walked
  through configuration reads) and `disable_aspm`. rtl8168 turns ASPM and
  clock power management off at start, as BSD's driver does.

## Consequences

- The kernel's PCI self-test, run on every smoke boot, checks the call
  against QEMU's NVMe controller (a PCI Express endpoint):
  - the power fields change and the rest of Link Control does not;
  - a 32-bit write is refused;
  - so are a BAR and the command register.
- The 8168's ASPM stall (ADR-0123) is dealt with like BSD's driver does.
  The transmit re-kick stays as a second guard. Neither is tested on
  hardware yet.
- ASPM off costs a little idle power on the 8168's link. On a laptop
  with an 8168 that is the trade BSD makes too.
