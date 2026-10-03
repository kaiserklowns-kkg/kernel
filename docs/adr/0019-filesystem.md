# ADR-0019: Filesystem service: node handles as capabilities

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0013 (IPC), ADR-0016 (init), ADR-0018 (shell)
- Adds: ABI v5 (badged endpoints, `MEMORY_SIZE`), a userspace heap

## Context

Phase 3 needs a filesystem. Storage drivers come in Phase 4, so the first
filesystem lives in memory. Its **protocol** must survive that change,
because disk-backed filesystems will implement the same one. In Oceans,
access to files must follow the capability model: no global namespace, no
ambient "root can read everything".

## Decision

### Node handles are capabilities

- Every open file or directory is a **badged client end** of the fs
  service's endpoint. Holding a directory handle grants that directory and
  its subtree, nothing more:
  - there is no `..`;
  - names `.`, `..`, and names containing `/` or NUL are refused;
  - there is no global path.
- **Paths are resolved by the client**, one component at a time, from a
  directory it holds (`Node::walk`).
- **Access per handle:** read-only or read-write, decided at `OPEN` and
  never widened.
  - Write access needs a writable parent handle and a writable node, and is
    refused rather than silently downgraded.
  - Passing someone a read-only handle to a subtree is safe delegation.

### Kernel: badged endpoints (ABI v5)

- `ENDPOINT_MINT(server, badge)`, which needs `MANAGE` on the server end
  (now among its default rights), creates another client end carrying a
  non-zero badge.
- `IPC_RECEIVE_MSG` returns `(kind, badge)`:
  - `EVENT_CALL` with the badge of the end the call came through;
  - `EVENT_CLOSED`, when the last capability to a badged end is closed
    anywhere (closed by a client, dropped at process exit, …).
- Servers therefore never leak per-handle state. Unbadged ends produce no
  close events, so servers that never mint are unaffected (ABI v1/v2
  behaviour is unchanged).
- An endpoint now counts its client ends; `PeerClosed` reaches the server
  when the last one goes.
- `MEMORY_SIZE(memory)` returns an object's size (needed to copy granted
  program images).

### Protocol (`user/fs-proto`, shared by the service and clients)

Requests are calls on a node handle. The label selects the operation and
the reply label is a `Status`. Data is inline, at most 248 bytes.

| Operation | Request data | Reply |
|---|---|---|
| `OPEN` | flags (`CREATE_FILE`, `CREATE_DIRECTORY`, `WRITE`) + name | new node handle + kind |
| `READ` | offset, length | bytes |
| `WRITE` | offset + bytes | count written |
| `STAT` | — | kind, size, writable |
| `LIST` | index | kind + name, or `NotFound` past the end |
| `REMOVE` | name | — (directories must be empty) |
| `TRUNCATE` | size | — |

Statuses are `NotFound`, `Exists`, `NotADirectory`, `IsADirectory`,
`NotEmpty`, `PermissionDenied`, `InvalidName`, `NoSpace` and `BadRequest`.
The client API (`Node`) hides the encoding. Bulk transfer through memory
objects is a future protocol addition.

### Service (`user/fs`): in-memory filesystem

- **State:**
  - nodes in an arena;
  - an open-handle table keyed by badge, holding the node and its access;
  - unbadged ends (from `use = fs`) are read-write handles to the root.
- **Lifetime:** a node is freed when it is unlinked and no handle refers to
  it, so open files stay readable after removal.
- **Quotas:** 4096 nodes, 16 MiB per file, 64 MiB in total.
- **`/bin`:** program images granted with `grant = module:NAME` are copied
  into read-only files under `/bin` (read-only directory, read-only
  files).
- **Heap:** the service uses `Vec` and `BTreeMap` via the new `alloc`
  feature of `oceans-rt`. That is the **kernel's own `oceans-heap`
  allocator** (ADR-0010) with memory objects as its page source, mapped at
  block-aligned addresses.

### Shell

- New commands: `ls`, `cat`, `write`, `mkdir`, `rm`.
- `run NAME` now loads `/bin/NAME` (or a path) from the filesystem into a
  memory object and spawns it. Boot-module grants still work.
- The shell gets `use = fs` instead of module grants.

### Fixes found by this work

- **Serial driver:** it dropped an output byte when the transmitter stayed
  busy past a spin limit. QEMU's Windows pipe backend stalls under load, so
  the `help` text vanished. A UART is now detected at boot through the
  scratch register; if present, output waits for the transmitter and is
  never dropped.
- **Shell:** output longer than the 512-byte formatting buffer was lost.
  `Buffer` now keeps what fits (truncating at a character boundary), and
  `help` writes its static text directly.
- **Shell:** parent directories for `write`, `mkdir` and `rm` were opened
  read-only. The service correctly refused, and they are now opened with
  write access.

## Consequences

- Delegating part of the filesystem means passing a handle; revoking it
  means closing it. No access-control lists are needed for that.
- The protocol is message-sized (248 bytes per call). Large files need
  many calls until memory-object transfers are added.
- Single-threaded processes (ADR-0014) mean one request at a time; fine
  for now.
- A restarted fs service loses its contents (it is in-memory) and existing
  handles see `PeerClosed`.

## Testing

- Kernel smoke: a badged call reports badge 7; closing that end produces a
  close event for 7; closing the unbadged end produces none.
- Shell smoke script (typed by `cargo xtask smoke`), each output checked:
  1. `ls /bin` lists `crasher` and `hello-client`;
  2. `mkdir /docs`, `write /docs/note.txt hello filesystem`, `cat` prints
     it back, `ls /docs` shows it;
  3. `rm` removes it, and `cat` then reports `not found`;
  4. `write /bin/evil` and `rm /bin/crasher` are `permission denied`;
  5. `run hello-client log use:echo` loads from `/bin` and exits 0;
  6. `run /bin/crasher log` is reported as killed by exception 14;
  7. `run nosuch` reports `not found`.
- Passes in debug, release and with `-cpu max`. On a normal boot, `fs`
  publishes `/bin/hello-client` and the shell can use it.
