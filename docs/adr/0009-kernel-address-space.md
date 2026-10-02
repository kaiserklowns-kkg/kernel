# ADR-0009: Kernel address space — own page tables, W^X, guarded stacks

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0003 (boot), ADR-0005 (NX baseline), ADR-0008 (frames)

## Context

Until now the kernel ran in the bootloader's page tables: every page RWX
or close to it, the kernel image aliased writable, on a bootloader stack
with no guard, with 47 MiB of bootloader memory unusable. Processes, the
heap and userspace (Phase 2–3) need a kernel-owned, documented layout that
every future address space can share.

## Decision

### Layout (x86_64, 4-level paging)

| Range | Size | PML4 | Contents |
|---|---|---|---|
| `0x0000_0000_0000_1000`–`0x0000_7fff_ffff_ffff` | 128 TiB | 0–255 | user space; page 0 never mapped |
| `0xffff_8000_0000_0000` | 64 TiB | 256–383 | direct map of RAM |
| `0xffff_c000_0000_0000` | 512 GiB | 384 | kernel heap (next step) |
| `0xffff_c080_0000_0000` | 512 GiB | 385 | kernel stacks |
| `0xffff_c100_0000_0000` | 512 GiB | 386 | MMIO mappings |
| `0xffff_ffff_8000_0000` | 2 GiB | 511 | kernel image |

Source of truth: `kernel/src/memory/layout.rs`.

### Mappings

- **Kernel image:** one mapping per segment, using linker-script symbols:
  text R-X, rodata (+GOT) R--, data/bss RW-, Limine requests RW-. No page is
  both writable and executable (W^X). The loader's virtual base must equal
  the link address (no KASLR yet).
- **Direct map:** RAM only (usable, bootloader-reclaimable, ACPI), RW-, at
  the bootloader's offset (which must lie in the direct-map slot and be
  2 MiB aligned), using 1 GiB pages where the CPU supports them, else 2 MiB.
  **Not** in the direct map: MMIO and reserved ranges (mapped on demand
  with the right cache type) and the kernel image itself, so kernel code has
  no writable alias.
- **Kernel stacks:** 64 KiB each in a 1 MiB slot of the stack region. The
  page below each stack is never mapped, so an overflow faults; the double
  fault then runs on its IST stack and reports.
- All kernel mappings are global. Top-level entries for the heap, stack and
  MMIO regions are created at boot, so later address spaces share the kernel
  half by copying PML4 entries 256–511 once.

### CPU protections

Enabled at boot, before the new tables are loaded: EFER.NXE (required;
boot fails without NX), CR0.WP, CR4.PGE, and SMEP, SMAP and UMIP when CPUID
reports them.

### Boot hand-over

1. `BootInfo` (memory map, command line up to 512 bytes, kernel load
   address) is copied into a kernel static.
2. The kernel builds its tables with frames from ADR-0008 and loads CR3.
3. It allocates a guarded kernel stack and switches to it; the bootloader
   stack is abandoned.
4. Bootloader-reclaimable memory is handed to the frame allocator (checked:
   adding a frame twice is an error).

### Self-tests

Normal boots run no tests. With `oceans.test=smoke` (CI), the kernel checks
the permissions of every kernel segment, that page 0 and stack guard pages
are unmapped, and runs the frame allocator self-test.

## Consequences

- Frame metadata now spans bootloader-reclaimable memory too (768 KiB on the
  256 MiB QEMU machine, up from 664 KiB).
- Page-table code is ours (`kernel/src/arch/x86_64/paging.rs`): map,
  translate, activate. Unmapping (with TLB shootdown) arrives with user
  address spaces and SMP.
- `Cache::Uncached` uses the power-on PAT. Write-combining needs a PAT setup
  (framebuffer, Phase 4).
- No KASLR. It is possible later because the kernel half is already
  separated by region.

## Failure behaviour

Missing NX, a missing or misplaced direct map, a mismatched load address, or
running out of frames while building the tables are boot panics with a
diagnostic: the kernel cannot run safely without any of these. After boot,
`map` returns `MapError` (misaligned, already mapped, huge-page conflict,
out of memory).

## Alternatives considered

- **Keep the bootloader's tables:** no W^X and no guard pages, and bootloader
  memory could never be reclaimed.
- **Our own direct-map offset:** the frame metadata and every direct-map
  pointer would need relocating mid-boot. Keeping the bootloader's offset,
  constrained to our region, is simpler and just as safe.
- **Direct map including MMIO, as Linux does:** wrong cache type for devices,
  and it hands every kernel bug access to device registers.
