# ADR-0008: Physical frame allocator — buddy system with per-frame metadata

- Status: Accepted
- Date: 2026-10-03
- Inputs: [reference-systems.md](../architecture/reference-systems.md) §1

## Context

Phase 2 needs physical memory for page tables, the kernel heap, thread
stacks, user memory and DMA buffers. Requirements:

- single frames (page tables, user pages) **and** contiguous, aligned blocks
  (DMA buffers, large pages later);
- bounded, predictable cost (no scanning proportional to RAM per call);
- detection of double frees and frees of foreign addresses (§43: never
  silently ignore errors);
- metadata usable later for reference counts (shared and copy-on-write
  memory, capability-mediated mappings);
- no heap: this allocator is what the heap is built on;
- host-testable (§44).

## Decision

**Buddy allocator**, crate `libs/frame-allocator` (`no_std`, no memory
access, host-tested), wrapped by `kernel/src/memory/frames.rs`.

- Orders `0..=10`: blocks of 4 KiB to 4 MiB, naturally aligned in physical
  memory. Allocation and free are O(MAX_ORDER).
- **One `FrameInfo` per frame** (12 bytes ≈ 0.3% of RAM): state, order,
  intrusive doubly linked free-list links (u32 frame indices), and 16
  reserved bits for a future reference count.
- Frame states: `Unavailable`, `Body`, `FreeHead`, `AllocatedHead`. `free`
  takes only the frame and reads the order from metadata, so a free with the
  wrong size is impossible, and freeing a non-head frame (double free, a
  pointer into the middle of a block, reserved memory) returns
  `FreeError::NotAllocated` instead of corrupting the lists.
- The metadata array spans the lowest to highest usable page, with its base
  rounded down to a 4 MiB boundary so index buddy arithmetic equals physical
  alignment. Ranges are added as maximal aligned blocks and coalesce with
  neighbours, so adjacent firmware regions merge.
- **Placement:** the kernel puts the metadata at the start of the largest
  usable region outside the exclusions and reaches it through the bootloader's
  direct map. It is excluded from allocation.
- **Excluded:** physical memory below 1 MiB (firmware data and the future SMP
  trampoline), the metadata itself, and everything not `Usable`.
  Bootloader-reclaimable memory is added once boot data has been consumed
  (Phase 2, after the kernel owns its page tables).
- **Concurrency:** one global allocator behind a spinlock, taken with
  interrupts disabled. Per-CPU frame caches come with SMP.
- **Contents are undefined** on allocation; whoever exposes frames to
  userspace must zero them (to be enforced at the mapping layer, ADR-0009).

## Consequences

- Holes in the physical map still cost metadata: 12 bytes per 4 KiB of hole
  between the lowest and highest usable page. Typical PCs have a 1–2 GiB hole
  below 4 GiB (≈ 3–6 MiB of metadata). If that matters on Tier 1 machines,
  split the array into sections per large region (as Redox does).
- No NUMA awareness. Tier 1 targets are single-socket; revisit with data.
- Indices are u32: up to 16 TiB of physical span.

## Testing

- 12 host unit tests (`cargo test -p oceans-frame-allocator`): layout,
  alignment, splitting and coalescing, adjacent-region merging, exclusions
  and trimming of unaligned edges, every frame handed out exactly once,
  bad and double frees, order limits, exhaustion, and a 20 000-step
  randomised run checked against a shadow model with a full coalescing check
  at the end.
- Boot self-test in the kernel: allocates a frame and a 64 KiB block, writes
  and verifies them through the direct map, frees them, checks that a double
  free is rejected and that stats return to their initial values. Runs on
  every `cargo xtask smoke`.

## Failure behaviour

Allocation returns `AllocError::OutOfMemory` / `OrderTooLarge`; free returns
`FreeError`. Initialisation failure (no usable RAM, metadata does not fit,
invalid map) panics at boot with a diagnostic: the kernel cannot run without
it.

## Alternatives considered

- **Bitmap allocator:** simplest, but contiguous allocations need a linear
  scan and it has no room for per-frame state.
- **Free-list stack of single frames:** O(1) but cannot provide contiguous
  blocks.
- **Linux-style `struct page` (64 B/frame):** far richer, 5× the memory;
  not needed yet.
