# Kernel memory management

Implemented incrementally (master spec §15). State per step:

| # | Step | State |
|---|---|---|
| 1 | Physical memory discovery | **Done** — `kernel/src/memory`, `libs/memory-map` |
| 2 | Physical frame allocator | **Done** — ADR-0008, `kernel/src/memory/frames.rs`, `libs/frame-allocator` |
| 3 | Kernel-owned page tables | **Done** — ADR-0009, `kernel/src/memory/paging.rs`, `kernel/src/arch/x86_64/paging.rs` |
| 4 | Kernel heap | Phase 2 (next) |
| 5 | User address spaces | Phase 2 |
| 6 | Memory protection (NX, W^X, SMEP/SMAP/UMIP) | **Done** for the kernel — ADR-0009; user side with processes |
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

## Address-space layout

Decided in [ADR-0009](../adr/0009-kernel-address-space.md); source of truth
`kernel/src/memory/layout.rs`.

| Range | Contents |
|---|---|
| `0x0000_0000_0000_1000`.. | user space (128 TiB) |
| `0xffff_8000_0000_0000` | direct map of RAM (RW, NX; no MMIO, no kernel image) |
| `0xffff_c000_0000_0000` | kernel heap |
| `0xffff_c080_0000_0000` | kernel stacks (64 KiB + unmapped guard page each) |
| `0xffff_c100_0000_0000` | MMIO |
| `0xffff_ffff_8000_0000` | kernel image: text R-X, rodata R--, data RW- |

## Frame allocator (Phase 2)

Buddy allocator, orders 0–10 (4 KiB–4 MiB), decided in
[ADR-0008](../adr/0008-physical-frame-allocator.md).

- Metadata: 12 bytes per frame, placed at the start of the largest usable
  region and reached through the direct map (664 KiB for the 256 MiB QEMU
  machine).
- Never allocated: below 1 MiB, the metadata, anything not `Usable`.
- Kernel API (`memory::frames`): `allocate_frames(order) -> Result<Frame, AllocError>`,
  `free_frames(frame) -> Result<(), FreeError>`. Frame contents are undefined.
- Bootloader-reclaimable memory is added after the switch to the kernel's own
  stack and page tables (ADR-0009).
- Self-test runs only in smoke-test boots (`oceans.test=smoke`, CI).
