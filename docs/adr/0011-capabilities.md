# ADR-0011: Kernel objects and capabilities

- Status: Accepted
- Date: 2026-10-03
- Makes concrete: [ADR-0006](0006-security-model.md) (capability-based security)
- Inputs: [reference-systems.md](../architecture/reference-systems.md) §1 (Redox's
  hybrid `uid == 0` checks are what this avoids)

## Context

ADR-0006 fixed the direction: capabilities are the only authority, there is
no ambient root, and capabilities can be delegated and revoked. Processes,
IPC and userspace drivers (next ADRs) need the concrete mechanism first,
because every syscall that touches an object goes through it.

## Decision

### Model

- Every resource is a **kernel object** (`kernel/src/object`). A
  **capability** = reference to an object + **rights**. A process holds
  capabilities in its own **capability table** and names them by **handle**.
- **No ambient authority.** No uid/gid checks and no "root" in the kernel.
  The first process receives initial capabilities from the kernel; all other
  authority flows from it by derivation and transfer.
- **No global namespace in the kernel.** Paths and service names are
  resolved by userspace services, which hand out capabilities.

### Handles

64-bit: slot index (low 32 bits) + generation (high 32 bits, starting at 1;
0 is never a valid handle). Closing a slot bumps its generation, so a stale
handle fails with `InvalidHandle`, never aliases a reused slot. Handles mean
nothing outside their table and cannot be forged into authority.

### Rights

`READ WRITE EXECUTE MAP SEND RECEIVE SIGNAL WAIT MANAGE DUPLICATE TRANSFER`.
Each object type defines which rights it honours. Raw rights from userspace
with unknown bits are rejected, not masked.

- **Derive** (`DUPLICATE` required): a new capability in the same table with
  a *subset* of the source's rights. Requesting more is `RightsEscalation`.
- **Transfer** (`TRANSFER` required): moves a capability between tables;
  exactly one table holds it afterwards. On any error neither table changes.
  IPC (ADR-0012) uses this to pass capabilities in messages.
- **Close:** removes the handle; the object is destroyed when its last
  capability (or kernel reference) goes.

### Revocation

`derive_revocable` creates a capability attached to a new **revocation
node**, whose parent is the source's node, and a **Revoker**, itself a kernel
object held by capability (`MANAGE`). Revoking marks the node. A capability is
revoked if any node on its chain is marked, so revoking cuts off that
capability and **everything later derived or transferred from it, in every
table**, with no global tracking. The check is O(chain depth) per use.
Revoked entries fail with `Revoked` and are purged on access or by `sweep`.
Revoking a parent's revoker never affects capabilities derived from the
parent *before* the revocable derivation (the grantor's own authority).

### Object types

| Type | Status | Rights used |
|---|---|---|
| `Memory` | **implemented**: zero-filled pages, freed when the last capability goes; byte read/write | READ, WRITE, MAP, MANAGE, DUPLICATE, TRANSFER |
| `Revoker` | **implemented** | MANAGE, TRANSFER |
| `AddressSpace`, `Thread` | with processes | MAP, MANAGE, … |
| `Endpoint`, `Notification` | ADR-0012 (IPC) | SEND, RECEIVE, SIGNAL, WAIT |
| `Irq`, `MmioRange`, `IoPortRange`, `DmaBuffer` | with userspace drivers | READ, WRITE, MAP, WAIT |

### Limits and locking

Each table has a limit (default 4096 entries; a per-process resource limit,
§16). Tables are not internally synchronised; each process's table will sit
behind its own lock. Revocation flags are atomics, so revoking needs no
lock on any holder's table.

### Code

- `libs/capability` (`oceans-capability`): `Rights`, `Handle`,
  `Capability<O>`, `CapTable<O>`, `Revoker`, `transfer`. Generic over the
  object type and host-tested.
- `kernel/src/object`: `KernelObject` enum, `MemoryObject`, typed lookups
  (`object::memory(table, handle, rights)`), `revoke`.

## Consequences

- A revoked capability keeps its object alive until the holder touches or
  closes it, or the table is swept. Memory objects will also need their
  mappings torn down on revocation; that hook comes with mapping (MAP).
- Revocation chains grow with nested revocable derivations; depth is
  bounded in practice by how many times authority is re-lent.
- There is no "list all holders of an object". This is deliberate: the
  kernel does not track capability copies.

## Testing

- `cargo test -p oceans-capability` (9 tests): rights algebra and parsing,
  stale and forged handles, rights enforcement, derive-only-drops-rights,
  revocation across tables (including nested and transferred
  descendants), revoked capabilities that cannot be derived or transferred,
  atomic transfer, table limits and sweep, and object lifetime following
  capabilities.
- Smoke boot: a server table creates a 3-page memory object and writes
  across a page boundary, lends a read-only revocable view to a client
  table by transfer, and the client reads it. A client write is denied,
  revocation cuts the client off, and closing all handles destroys the
  object (checked with a weak reference; `Drop` returns its frames).

## Alternatives considered

- **seL4-style CNodes and capability derivation tree:** precise, eager
  revocation, but much more complex (CNode addressing, untyped retype); a
  per-process handle table fits Oceans' service model better.
- **Zircon-style handles without revocation:** simpler, but ADR-0006 needs
  revocation for lending authority (e.g. a driver's DMA buffer, an AI
  agent's temporary grant, ADR-0007).
- **Unix permissions + capabilities (Redox):** two authority systems where
  the ambient one undermines the other.
