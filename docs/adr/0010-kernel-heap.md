# ADR-0010: Kernel heap — slab caches + buddy page blocks via the direct map

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0008 (frames), ADR-0009 (direct map)

## Context

Processes, threads, capability tables and IPC endpoints need dynamically
sized kernel objects: Rust's `alloc` (`Box`, `Vec`, `BTreeMap`, `Arc`).
Requirements: no fixed size cap (Redox's 1 MiB heap is a lesson),
predictable cost, low fragmentation for many small objects, memory returned
to the system when no longer needed, and host-testable logic.

## Decision

`#[global_allocator]` in `kernel/src/memory/heap.rs`, over the
host-tested crate `libs/heap` (`oceans-heap`). Two tiers:

| Request (max of size, align) | Served by | Granularity |
|---|---|---|
| ≤ 2 KiB | slab caches, 9 classes: 8, 16, … 2048 B | power of two |
| > 2 KiB, ≤ 4 MiB | one buddy block of `4 KiB << order` | power-of-two pages |
| > 4 MiB | fails (`alloc` error → panic) | — |

- **Slabs:** a buddy block (4 KiB, or 8/16 KiB for the 1 KiB/2 KiB classes,
  so each slab holds ≥ 8 objects) with a 32-byte header in the first slot(s)
  and objects aligned to their class size. Free objects form an intrusive
  list. Slabs are aligned to their size, so `free` finds the header by
  masking: no lookup structures.
- **Returning memory:** each class keeps at most one empty slab; further
  empty slabs go straight back to the frame allocator.
- **Addressing:** all heap memory lives in the direct map (like Linux
  `kmalloc`). Allocating needs no page-table changes and no TLB work. The
  `KERNEL_HEAP` virtual region (ADR-0009) stays reserved for future
  virtually-contiguous allocations (a `vmalloc` equivalent) above 4 MiB.
- **Integrity:** `free` asserts that the pointer is an object slot of the
  layout's class; a mismatched layout or wild pointer panics as heap
  corruption instead of corrupting free lists.
- **Locking:** one spinlock, taken with interrupts disabled. Lock order is
  heap → frames, and the frame allocator never allocates from the heap.
- **Bring-up:** allocations before `memory::init` completes return null
  (and panic through the default allocation error handler).

## Consequences

- Power-of-two rounding wastes up to 50% on unlucky sizes (e.g. 2049 B →
  4 KiB). Acceptable for kernel objects; measure once real workloads exist.
- Large allocations need physically contiguous memory and can fail under
  fragmentation before memory is exhausted; the vmalloc-style tier fixes that
  when needed.
- No per-CPU caches yet; they arrive with SMP.
- No double-free detection for small objects beyond slot alignment (a
  poisoning debug mode can be added).

## Testing

- `cargo test -p oceans-heap` (7 tests): class and order selection,
  alignment, distinct addresses, return of empty slabs, the large tier,
  exhaustion and oversize, detection of misaligned frees, and a
  30 000-step randomised mix verifying every allocation keeps its contents.
- Smoke boot (`oceans.test=smoke`): `Box`, `Vec`, `BTreeMap<u32, String>` and
  a 1 MiB `Vec` in the real kernel, then checks that heap usage returns to
  baseline.

## Alternatives considered

- **`linked_list_allocator` over a fixed region:** O(n) allocation, external
  fragmentation, fixed size.
- **Heap in its own virtual region, mapping pages on demand:** every growth
  costs page-table work and later unmapping needs TLB shootdowns. The direct
  map makes this unnecessary for objects up to 4 MiB.
- **Third-party allocator crates (e.g. `talc`):** good, but the heap's
  interaction with frames and later per-CPU caches is core kernel design; the
  slab tier is ~300 lines and fully tested.
