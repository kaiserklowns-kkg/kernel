# ADR-0003: Boot protocol — Limine behind a boot abstraction

- Status: Accepted
- Date: 2026-10-03

## Context

Phase 1 needs a reliable path from UEFI firmware to a 64-bit higher-half
kernel with a memory map. Writing a bootloader is not part of Oceans' identity
(master spec §2.2).

## Decision

- Use the **Limine boot protocol** (base revision 3) via the `limine` crate
  (MIT/Apache-2.0), with the Limine UEFI binary (BSD-2-Clause) on the EFI
  system partition.
- **UEFI only** on x86_64. Legacy BIOS boot is Tier 3 (ADR-0005).
- Only `kernel/src/boot/` knows Limine. It produces a protocol-neutral
  `BootInfo` (memory regions, direct-map offset, command line). The memory
  map model lives in `libs/memory-map` and is unit tested on the host.
- The kernel is a static ELF linked at `0xffffffff80000000` with explicit
  segment permissions (requests RW, text RX, rodata R, data RW).

## Consequences

- An Oceans-owned bootloader or a direct UEFI stub can replace Limine later by
  adding a sibling of `boot/limine.rs`.
- `limine` 0.6 requires nightly Rust; we stay on 0.5 until a stable-compatible
  release exists (ADR-0004).
- AArch64 will reuse the same protocol (Limine supports it).

## Alternatives considered

- `bootloader` crate (rust-osdev): couples the build to its image tooling.
- Multiboot2/GRUB: BIOS-era assumptions, 32-bit entry.
- Own UEFI loader now: real work with no Phase 1 payoff.
