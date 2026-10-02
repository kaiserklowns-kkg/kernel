# Kernel memory management

Implemented incrementally (master spec §15). State per step:

| # | Step | State |
|---|---|---|
| 1 | Physical memory discovery | **Done** — `kernel/src/memory`, `libs/memory-map` |
| 2 | Physical frame allocator | **Done** — ADR-0008, `kernel/src/memory/frames.rs`, `libs/frame-allocator` |
| 3 | Kernel-owned page tables | Phase 2 (next, ADR-0009) |
| 4 | Kernel heap | Phase 2 |
| 5 | User address spaces | Phase 2 |
| 6 | Memory protection (NX, W^X, SMEP/SMAP) | Phase 2 |
| 7 | Shared memory (capability-mediated) | Phase 2, with IPC |
| 8 | Memory mapping | Phase 3 |

## Discovery (Phase 1)

The boot layer converts the bootloader memory map into `Region`s
(`libs/memory-map`), keeping up to 256 entries. `memory::discover` then:

- rejects maps whose regions overlap or are unsorted (kernel panic — running
  on a wrong map corrupts memory);
- counts only whole 4 KiB pages inside usable regions;
- reports reclaimable (bootloader, ACPI) and reserved bytes separately;
- requires a physical-memory direct map from the bootloader.

All physical memory is reachable at `direct_map_offset + phys` until the
kernel installs its own page tables.

## Current address-space layout

| Range | Contents |
|---|---|
| `0xffffffff80000000`.. | kernel image (text RX, rodata R, data/bss RW) |
| direct-map offset (bootloader chosen) | all physical memory |

The full layout, including user space and the kernel heap, is decided in the
Phase 2 memory ADR.

## Frame allocator (Phase 2)

Buddy allocator, orders 0–10 (4 KiB–4 MiB), decided in
[ADR-0008](../adr/0008-physical-frame-allocator.md).

- Metadata: 12 bytes per frame, placed at the start of the largest usable
  region and reached through the direct map (664 KiB for the 256 MiB QEMU
  machine).
- Never allocated: below 1 MiB, the metadata, anything not `Usable`.
- Kernel API (`memory::frames`): `allocate_frames(order) -> Result<Frame, AllocError>`,
  `free_frames(frame) -> Result<(), FreeError>`. Frame contents are undefined.
- Boot self-test runs on every boot and in the CI smoke test.
