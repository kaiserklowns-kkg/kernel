# Hardware support tiers

Decision record: [ADR-0005](../adr/0005-hardware-targets.md).

| Tier | Meaning | Engineering commitment |
|---|---|---|
| 0 — Reference | QEMU q35 + OVMF (x86_64) | Every change is tested here in CI |
| 1 — Supported | Validated hardware in the compatibility matrix | Bugs are release blockers |
| 2 — Experimental | May work; community-maintained | Best effort, no guarantee |
| 3 — Legacy | Not supported | No engineering time |

## Criteria for Tier 1

A device or platform qualifies when it has significant current usage, public
documentation or a stable specification, reasonable driver cost, and strategic
value. Exact chipsets are added only after research and a test device.

## Current matrix

| Class | Tier 0 (QEMU) | Tier 1 candidates (research pending) |
|---|---|---|
| Firmware | OVMF UEFI | UEFI 2.x with ACPI |
| Interrupts | local APIC + I/O APIC (ACPI MADT) | x2APIC, MSI (Phase 4) |
| CPU | x86-64 (TCG/KVM) | x86-64-v2+ Intel/AMD; selected AArch64 |
| Console | 16550 UART (COM1) | — (framebuffer console in Phase 4) |
| Storage | — | NVMe, virtio-blk |
| USB | — | xHCI |
| Network | — | virtio-net, Intel and Realtek Ethernet |

Tier 3 explicitly: legacy BIOS boot, 32-bit x86, ISA, IDE/PATA, floppy.
