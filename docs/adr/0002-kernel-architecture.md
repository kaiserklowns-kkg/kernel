# ADR-0002: Kernel architecture — microkernel-leaning hybrid

- Status: Accepted
- Date: 2026-10-03

## Context

Oceans prioritises correctness and security (driver faults must not take the
system down; AI and apps must be mediated) but also hardware practicality and
performance. Pure microkernels (seL4, Redox's direction) maximise isolation;
monolithic kernels (Linux) minimise IPC cost and are faster to bring up.

## Decision

The Oceans kernel contains only:

- physical/virtual memory management and address spaces
- threads, processes and scheduling
- IPC (synchronous message passing, shared-memory channels, notifications)
- the capability system (ADR-0006)
- interrupt routing and timers
- boot-critical device support: early console, interrupt controller, timer

Everything else — storage, filesystems, network stack, USB, GPU, input —
runs as **userspace services** that hold capabilities to the device resources
(MMIO ranges, IRQs, DMA-able memory) they need.

An in-kernel driver beyond the list above requires its own ADR with
measurements showing the userspace version is inadequate.

The syscall interface is small, Oceans-specific and versioned. Applications
target the Oceans System API (libraries + services), not raw syscalls.

## Consequences

- IPC performance is a first-class concern; it is benchmarked from Phase 2.
- DMA isolation needs an IOMMU on Tier 1 hardware for full driver isolation;
  without it, driver services are trusted for DMA (documented limitation).
- Bring-up is slower than a monolithic design; acceptable per priorities.

## Alternatives considered

- Monolithic: fastest to boot to a shell, but driver isolation and the
  capability model become retrofits.
- Fork Redox: contrary to the master spec; Oceans needs its own API and
  security model.
