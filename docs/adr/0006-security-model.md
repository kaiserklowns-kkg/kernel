# ADR-0006: Capability-based security model

- Status: Accepted — mechanism specified in [ADR-0011](0011-capabilities.md); permission broker and consent in [ADR-0047](0047-permissions-and-consent.md), signed apps in [ADR-0046](0046-packages.md)
- Date: 2026-10-03

## Context

Oceans requires least privilege, explicit permissions, secure IPC and audit
logging from the beginning (master spec §22–23, §50). Retrofitting these onto
an ambient-authority model (Unix UIDs, global namespaces) does not work well.

## Decision (direction)

- **Capabilities are the only authority.** A process can act on a kernel
  object (memory, thread, IPC endpoint, IRQ, MMIO range) only through an
  unforgeable handle in its capability table.
- Capabilities can be **delegated** (optionally with reduced rights) over IPC
  and **revoked** by their grantor.
- **No global namespace in the kernel.** Names (files, services, devices) are
  resolved by userspace services that hand out capabilities.
- **User-facing permissions** (Files, Network, Camera, Microphone, Location,
  GPU, AI, Notifications, Devices) are policy in the userspace *permission
  broker*, which grants the underlying capabilities after user consent.
- **Audit:** the broker logs every grant, denial and revocation as structured
  events; the kernel exposes counters and denial events.
- Apps are signed; the package manager verifies signatures before install
  (Phase 5).

## To decide in the full ADR

Capability representation and table layout, revocation mechanism, rights
bits, how consent is persisted and how it expires.
