# ADR-0030: Shared buffers for bulk data

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0022 (files), ADR-0024 (TCP), ADR-0028 (fetch)

## Context

Every file and TCP request carried its data inline in an IPC message,
which holds at most 248 bytes. A 1 MiB download took over 8,000 round
trips to the network service and 4,000 more to the filesystem: 2.6 s in
a release build and 9.5 s in debug. Loading a program from disk was
slow for the same reason. Messages stay small on purpose (ADR-0013: at most 256 bytes), so
bulk data needs another path.

## Decision

- **A memory object per handle.** A client creates a memory object of
  4 KiB to 1 MiB and maps it. It then sends a duplicate (`READ`,
  `WRITE`, `MAP`, `TRANSFER`) with an attach request on one open
  handle: `TCP_ATTACH` (13) on a TCP connection, `ATTACH` (9) on a file
  handle. The service maps the object read-write and closes the
  capability. The mapping keeps the object alive. A second attach
  replaces the first buffer.
- **Bulk operations name a window of that buffer:**
  - TCP: `TCP_SEND_BUF` (14) takes `[offset u32][len u32]` and returns
    the bytes accepted; `TCP_RECV_BUF` (15) takes
    `[offset u32][capacity u32]` and returns the bytes received, `Empty`
    or `Eof`.
  - Files: `WRITE_BUF` (10) and `READ_BUF` (11) take
    `[file offset u64][at u32][len u32]` and return the bytes moved.
  - The service checks that the window lies inside the buffer it mapped
    (overflow-checked). A file handle also needs `WRITE` to use
    `WRITE_BUF`, the same rule as `WRITE`.
- **The buffer is used only during a call.** The client writes into it
  before the call and reads from it after the reply. The service
  touches it only while the call is in progress. Each side therefore
  sees a stable buffer, without locks. A client that changes the
  buffer during a call only spoils its own data.
- **Lifetime:** the service unmaps the buffer when the handle is closed.
  The client unmaps its own mapping when it drops `TcpStream` or
  `oceans_fs_proto::Shared`.
- **Clients:**
  - `TcpStream` attaches a 64 KiB buffer at creation.
  - `fetch` gives its output file a 16 KiB buffer.
  - The shell uses 64 KiB to load a program from disk and 4 KiB for
    `cat`.
  - Inline operations keep working, and every client falls back to them
    if attaching fails.

## Consequences

A 1 MiB `fetch` to disk now takes 260 ms in release (it was 2,640 ms)
and 1.2 s in debug (it was 9.5 s). The data is still copied once on
each side, between a private buffer and the shared one. Removing that
copy would need protocols that hand out windows of the buffer, which is
possible later without changing the ABI. A service now holds one extra
mapping per attached handle, at most 1 MiB each. This memory counts
against the memory object the client created.

## Alternatives considered

- **Bigger IPC messages:** these grow every message and the kernel's
  copy path. ADR-0013 keeps messages small.
- **Grants for each request** (pass a memory capability with every
  call): every call would pay for a map and an unmap.
- **Rings shared with notifications** (io_uring-style): faster for many
  small operations in flight, but much more protocol for clients that
  do one operation at a time. It can come later on top of the same
  memory objects.

## Checklist (master spec §48)

- **Purpose:** move bulk data without one IPC round trip per 248 bytes.
- **Architecture:** a memory object per handle, attached once; IPC
  messages carry only windows of it.
- **API:** net ops 13–15, fs ops 9–11, `TcpStream` (automatic),
  `Node::attach`, `write_shared`, `read_shared`.
- **Dependencies:** none new.
- **Security:**
  - every window is bounds-checked;
  - the buffer is private to one handle, so other clients cannot see
    it;
  - write access is checked as before.
- **Testing:**
  - the smoke test downloads 1 MiB to disk and verifies it on the host;
  - the shell loads programs and runs `cat` through the bulk read path.
- **Failure behaviour:** if attaching fails, clients fall back to inline
  operations. A bad window gets `BadRequest`.
