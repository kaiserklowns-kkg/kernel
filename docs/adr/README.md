# Architecture Decision Records

Every decision that is hard to reverse or shapes the system's identity gets an
ADR before implementation (master spec §48, §53). AI coding agents must follow
accepted ADRs and may not change them without a new ADR.

Statuses: **Proposed** → **Accepted** → (**Superseded by ADR-NNNN**).

| ADR | Title | Status |
|---|---|---|
| [0001](0001-monorepo-and-language-boundaries.md) | Monorepo and language boundaries | Accepted |
| [0002](0002-kernel-architecture.md) | Kernel architecture: microkernel-leaning hybrid | Accepted |
| [0003](0003-boot-protocol.md) | Boot protocol: Limine behind a boot abstraction | Accepted |
| [0004](0004-rust-toolchain.md) | Stable Rust only; minimal, audited dependencies | Accepted |
| [0005](0005-hardware-targets.md) | Hardware tiers and initial targets | Accepted |
| [0006](0006-security-model.md) | Capability-based security model | Accepted (mechanism: ADR-0011) |
| [0007](0007-ai-permission-mediation.md) | AI agents as unprivileged, mediated principals | Proposed |
| [0008](0008-physical-frame-allocator.md) | Physical frame allocator: buddy system with per-frame metadata | Accepted |
| [0009](0009-kernel-address-space.md) | Kernel address space: own page tables, W^X, guarded stacks | Accepted |
| [0010](0010-kernel-heap.md) | Kernel heap: slab caches + buddy page blocks via the direct map | Accepted |
| [0011](0011-capabilities.md) | Kernel objects and capabilities | Accepted |
| [0012](0012-threads-and-scheduling.md) | Kernel threads and preemptive round-robin scheduling | Accepted |
| [0013](0013-ipc.md) | IPC: endpoints (call/reply) and notifications | Accepted |

New ADRs copy [template.md](template.md) and take the next number.
