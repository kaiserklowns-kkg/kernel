# ADR-0004: Stable Rust only; minimal, audited dependencies

- Status: Accepted
- Date: 2026-10-03

## Context

Kernels often depend on nightly features (`x86-interrupt` ABI,
`custom_test_frameworks`, `build-std`). Nightly breaks unpredictably and
undermines reproducible builds.

## Decision

- Build with **stable Rust**, pinned in `rust-toolchain.toml`, targeting
  `x86_64-unknown-none` (soft-float, no red zone, panic=abort).
- Exception entry uses `global_asm!` stubs and `#[unsafe(naked)]` functions
  (stable since 1.88) instead of the unstable `x86-interrupt` ABI.
- Kernel tests: host unit tests for portable crates (`libs/*`) plus QEMU smoke
  tests driven by `cargo xtask smoke`.
- Kernel dependencies must be `no_std`, permissively licensed
  (MIT/Apache/BSD/Zlib), and confined to one module where possible:

  | Crate | Use | Confined to |
  |---|---|---|
  | `limine` 0.5 | boot protocol structures | `kernel/src/boot` |
  | `x86_64` 0.15 | GDT/TSS types, port I/O, `lidt` | `kernel/src/arch/x86_64` |
  | `spin` 0.12 | `Mutex`, `Once` before a scheduler exists | kernel-wide |

- `unsafe` blocks carry a `SAFETY:` comment stating the invariant.

## Consequences

- Some ergonomics are lost (hand-written trap stubs).
- Adding a kernel dependency requires updating this table in review.

## Alternatives considered

- Nightly with `build-std`: rejected for stability.
