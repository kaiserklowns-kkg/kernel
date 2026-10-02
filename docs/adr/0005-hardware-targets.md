# ADR-0005: Hardware tiers and initial targets

- Status: Accepted
- Date: 2026-10-03

## Context

Oceans deliberately does not chase Linux's device coverage (master spec §12).
Engineering effort must go to hardware that is common, documented and
strategic.

## Decision

Tiers are defined in [docs/hardware/support-tiers.md](../hardware/support-tiers.md).
Initial commitments:

- **Tier 0 (development reference):** QEMU `q35`, x86_64, UEFI (OVMF), 256 MiB+.
  Every change must pass the QEMU smoke test.
- **Tier 1 architecture:** x86_64 first. AArch64 second, after Phase 2 proves
  the arch boundary; specific boards chosen by research (separate ADR).
- **Tier 1 baseline CPU:** x86-64-v2 or newer with UEFI 2.x, ACPI, x2APIC/APIC,
  NX. No 32-bit x86 support.
- **Device classes for Phase 4:** NVMe, xHCI USB (HID, mass storage), virtio
  (net, blk, gpu) for VMs, one Intel and one Realtek Ethernet family.
  Wi-Fi and GPU chipsets are chosen later by research, not assumption.
- **Tier 3 (not supported):** legacy BIOS boot, ISA devices, IDE/PATA, floppy,
  pre-UEFI systems, 32-bit CPUs.

## Consequences

- The kernel may assume UEFI + ACPI + APIC on x86_64.
- The legacy 8259 PIC is only remapped and masked, never used.
