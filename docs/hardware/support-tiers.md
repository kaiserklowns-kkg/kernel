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
| Interrupts | local APIC + I/O APIC (ACPI MADT); MSI-X for devices (ADR-0021) | x2APIC, MSI |
| CPU | x86-64 (TCG/KVM) | x86-64-v2+ Intel/AMD; selected AArch64 |
| Console | 16550 UART (COM1); UEFI GOP framebuffer text console + PS/2 keyboard (ADR-0029) | USB HID keyboards (xHCI) |
| Buses | PCI Express via ACPI MCFG/ECAM (ADR-0021) | — |
| Storage | virtio-blk (modern, `1af4:1042`), userspace driver (ADR-0021) | NVMe |
| USB | — | xHCI |
| Network | virtio-net (modern, `1af4:1041`), userspace driver (ADR-0023) | Intel and Realtek Ethernet |

Tier 3 explicitly: legacy BIOS boot, 32-bit x86, ISA, IDE/PATA, floppy.
