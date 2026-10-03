# ADR-0015: System call ABI v2: capabilities over IPC, memory, processes

- Status: Accepted
- Date: 2026-10-03
- Extends: ADR-0014 (ABI v1). First step of Phase 3.

## Context

To start and supervise services, init (Phase 3) needs to:
- create memory and map it;
- create endpoints and hand them out;
- move capabilities in messages;
- start processes from images it holds;
- wait for them to exit.

ABI v1 had none of this. The ABI must stay stable (§19): only additions.

## Decision

`ABI_VERSION` = 2. Syscalls 0–7 are unchanged; 8–17 are added
(`libs/abi`).

| # | Call | Arguments → result | Authority |
|---|---|---|---|
| 8 | `HANDLE_DUPLICATE` | handle, rights → handle | `DUPLICATE`; rights ⊆ source (unknown bits rejected) |
| 9 | `ENDPOINT_CREATE` | → server, client | none (creates new objects) |
| 10 | `IPC_CALL_MSG` | client, request desc, reply desc | `SEND`; `TRANSFER` on each sent handle |
| 11 | `IPC_RECEIVE_MSG` | server, desc | `RECEIVE` |
| 12 | `IPC_REPLY_MSG` | desc | the pending call; `TRANSFER` on each sent handle |
| 13 | `MEMORY_CREATE` | size → handle | none (resource quotas: later) |
| 14 | `MEMORY_MAP` | handle, addr (0 = kernel picks), prot → addr | `MAP` + `READ`, plus `WRITE`/`EXECUTE` per prot |
| 15 | `MEMORY_UNMAP` | addr | own mapping |
| 16 | `PROCESS_SPAWN` | image, len (0 = whole), handles, count, arg → process | `READ` on the image; `TRANSFER` on each handle |
| 17 | `PROCESS_WAIT` | process → (0, exit code) | `WAIT` |

New errors: `InvalidArgument`, `AddressInUse`, `InvalidImage`.

### Capabilities in messages

- A `MessageDesc { label, data, data_len, handles, handles_len }` in user
  memory describes what to send, or the buffers to receive into. On
  receive, the kernel writes the label and the actual lengths back.
- **Sending is all-or-nothing.** Every handle is checked for `TRANSFER`
  (and duplicates are rejected) before any is removed, and removal is the
  last step, so nothing can fail after handles leave the table.
- **Receiving:**
  - If the data or handles exceed the receiver's buffers, the call fails
    with `TooLarge` and the carried capabilities are **closed, never
    leaked**.
  - If inserting into the table or writing the handles back fails, the
    inserted handles are closed again.
  - Protocols size their buffers; the ABI limits are 256 bytes and 4
    handles.

### Memory

- `MEMORY_CREATE` gives a zero-filled object. `MEMORY_MAP` maps the whole
  object, at a caller address (page-aligned, inside user space, no overlap
  → `AddressInUse`) or a kernel-chosen one. Kernel-chosen addresses come
  from `0x1000_0000_0000`–`0x7000_0000_0000`, with an unmapped page between
  mappings. Partial mappings are rolled back.
- **W^X across mappings.** `WRITE|EXECUTE` in one request is refused. Each
  memory object also remembers whether it has ever been mapped writable or
  executable and refuses to become both. So two mappings of one object
  cannot be combined into writable code. The loader's segments obey the
  same rule.
- `MEMORY_UNMAP` removes a whole mapping and flushes the TLB entries. The
  object's frames are freed when its last mapping and capability go.

### Processes

- `PROCESS_SPAWN` reads the image from a memory object (≤ 16 MiB) and
  **validates it before any handle leaves the caller**. It then moves the
  listed handles to the child as its initial capabilities and returns a
  **process capability** (`WAIT`, `MANAGE`, `DUPLICATE`, `TRANSFER`). The
  child is named `<parent>.child`.
- `PROCESS_WAIT` blocks until exit (a wait queue on the process) and
  returns the code in `rdx`, so negative codes cannot be confused with
  errors.
- **Resource lifetime:**
  - at exit, capabilities are closed (ADR-0014);
  - when the last thread is reaped, the **address space is freed**, even
    while a parent still holds the process capability;
  - the process record keeps only the exit code until the last handle goes.

### Not yet

- Per-process resource quotas (memory, handles, processes) for
  `MEMORY_CREATE`/`PROCESS_SPAWN`.
- Unmapping on revocation of a memory capability.
- Kill (`MANAGE`), multi-threaded processes, and spawn by name through a
  service rather than by image.

## Testing

Smoke boot, in addition to the ADR-0014 processes: a **parent** process
receives its own image as a read-only memory object and checks, in order:
1. ABI version ≥ 2;
2. create and map memory read-write, then fill it;
3. a duplicated read-only handle maps the same bytes;
4. a read-only handle cannot map writable (`MissingRights`);
5. `WRITE|EXECUTE` is refused, and a writable-mapped object cannot be
   mapped executable (`InvalidArgument`);
6. an overlapping address is refused (`AddressInUse`);
7. rights cannot be escalated by duplicating;
8. unmap works, and a second unmap of the same mapping is refused;
9. it creates an endpoint and spawns a **child** with a log handle and the
   server end; the moved handles are gone from the parent;
10. it sends the read-only memory handle in a message. The child maps it,
    sums it, and replies with a new memory object of its own, which the
    parent maps and reads;
11. it closes its end; the child sees `PeerClosed` and exits; `PROCESS_WAIT`
    returns 0.

All processes are then destroyed (checked with weak references). This
passes in debug, release, and `-cpu max` (SMEP/SMAP/UMIP). Each failing
step returns its own exit code (`20 + step`) for diagnosis.
