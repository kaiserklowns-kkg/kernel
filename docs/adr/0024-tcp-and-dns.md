# ADR-0024: TCP and DNS

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0023 (network stack, sockets)

## Context

ADR-0023 gave Oceans UDP, ICMP and DHCP. Nearly everything a networked
system does (package downloads, model fetching, remote shells, HTTP) runs
over TCP, and finds hosts by name. Both live in the same layers as before:
the host-tested stack and the socket protocol.

## Decision

### TCP in the stack (`libs/net/src/tcp.rs`)

- **Connections.**
  - The full RFC 9293 state machine: active and passive open (listeners
    with an 8-connection backlog), and graceful close in both directions.
  - TIME_WAIT lasts 10 s (a short MSL).
  - MSS option: 1460 bytes offered, the peer's respected.
  - Initial sequence numbers are keyed per connection (RFC 6528): a hash
    of a secret and the ports, plus the clock. The `net` service seeds the
    secret from the TSC.
- **Data.**
  - 16 KiB send and receive buffers per connection. The advertised window
    is the receive buffer's free space, with window updates once reading
    opens it significantly.
  - Zero-window probing keeps a stalled connection alive.
- **Reliability.**
  - RFC 6298 retransmission timeouts: smoothed RTT, Karn's rule,
    exponential backoff, 200 ms to 60 s.
  - Go-back-N from the oldest unacknowledged byte.
  - After 8 retries (5 for a SYN) the connection fails with `TimedOut`.
- **Robustness.**
  - Checksums are verified and data offsets and options bounds-checked.
  - Out-of-window segments get an ACK.
  - RST and SYN inside the window but not exactly at `rcv_nxt` only get a
    challenge ACK (RFC 5961, against blind resets).
  - Segments for unknown connections get a RST, never in reply to a RST.
- **Lifetime.**
  - Releasing a socket closes it gracefully in the background. A FIN goes
    after any queued data; the orphan is freed after TIME_WAIT, or after
    60 s stuck in FIN_WAIT_2.
  - Unread received data makes the release a reset.
  - Releasing a listener resets connections nobody accepted.
- **Deliberately not yet:**
  - reordering: out-of-order segments are dropped and the sender
    retransmits;
  - window scaling, SACK, congestion control beyond backoff, urgent data.

### Sockets

- **Operations:** `TCP_CONNECT`, `TCP_LISTEN`, `TCP_ACCEPT`, `TCP_SEND`
  (returns the bytes accepted, 0 when full), `TCP_RECV` (data, `Empty` or
  `Eof`), `TCP_SHUTDOWN` and `TCP_STATUS`.
- **New statuses:** `Refused`, `Reset`, `TimedOut`, `NotConnected`,
  `Closed`, `Eof`.
- **Events:** as before, a connection's notification is signalled on any
  change: connected, data, buffer space, end of stream or error. The
  program then asks.
- **Client API (`oceans-net-proto`):**
  - `TcpStream` (`connect`, `wait_connected`, `send` / `send_all`, `read`
    / `read_wait`, `shutdown`) and `TcpListener` (`listen`, `accept`).
  - The `*_on` constructors let one thread drive many sockets from a
    shared notification.
- **Limit:** stream data is inline, at most 248 bytes per call. That is
  fine for protocols and text; bulk transfers will get shared buffers.

### DNS

- **The codec (`libs/dns`, `oceans-dns`):** A-record queries, plus
  responses parsed without allocation (so any program can resolve).
  - A response must match the query's id and question.
  - Labels, lengths and compression pointers are bounds-checked.
    Pointers must point backwards, and chains are limited.
  - CNAME chains are followed to the A record.
  - NXDOMAIN, no-address, truncation and server errors are distinguished.
- **The resolver (`oceans_net_proto::resolve`) runs in the program.**
  - It uses a UDP socket to the DHCP-provided server and retries 3 times,
    1.5 s apart.
  - A resolver inside the `net` service would need it to hold calls open
    across network round trips, which its single pending reply cannot do.
    This way only the caller waits.
- No cache yet; a caching resolver service is the natural next step.

### Programs

| Program | What it does |
|---|---|
| `host NAME [SERVER[:PORT]]` | resolves a name |
| `nc HOST PORT [TEXT...]` | connects (resolving HOST), sends TEXT and closes its side, then prints the reply |
| `net-echo` | replaces `udp-echo`: RFC 862 echo on UDP **and** TCP port 7; one thread for every socket through a shared notification |

## Consequences

- Programs can talk to real servers by name. The remaining blocker for
  package and model downloads is HTTP and, later, TLS: both are userspace
  libraries on top of this.
- Dropping out-of-order data is simple and correct but slow on lossy
  paths; reassembly is a contained change in `tcp.rs`.
- ISNs are only as unpredictable as the TSC seed until the kernel has an
  entropy source.

## Alternatives considered

- **smoltcp.** It is capable, but it brings its own socket model and
  allocation patterns. Keeping the stack small and fully host-tested
  matched how the rest of Oceans is built.
- **DNS in the `net` service.** One cache for everyone, but it needs
  asynchronous replies (see above). This becomes possible with a caching
  resolver service.

## Testing

- **TCP, `cargo test -p oceans-net`** (23 tests, 11 for TCP). Two stacks
  are joined by a simulated wire that can drop frames or be unplugged:
  - connect, accept, 100 KB A→B, a reply B→A, close in both directions,
    TIME_WAIT expiry, every socket freed, segments ≤ MTU;
  - a closed port is `Refused`;
  - lost data segments are retransmitted;
  - a silent peer times out (data and SYN);
  - a receiver that stops reading closes the window, probes keep the
    connection up, and reading resumes the transfer completely;
  - releasing with unread data resets the peer;
  - a graceful release finishes in the background and frees the orphan;
  - the backlog bound, and a listener release resetting what it dropped;
  - port exclusivity and per-connection ISNs;
  - every single-byte mutation of a recorded exchange, fed to fresh
    stacks, never panics.
- **DNS, `cargo test -p oceans-dns`** (5 tests):
  - standard query bytes and name validation;
  - answers, NXDOMAIN, wrong id or question, truncation, server errors;
  - CNAME chains;
  - pointer loops, forward pointers and bad lengths, plus every mutation
    and truncation of a response without panic.
- **Smoke test.** xtask runs a DNS server and a TCP greeter on the host
  (reached from the guest as 10.0.2.2) and forwards host ports to guest
  port 7. It checks:
  - `host oceans.test` → `10.1.2.3`, and `missing.test` → not found;
  - `nc` to the greeter → `hello from the host: hello over tcp`;
  - from the host, a TCP connection to the guest's echo service (passive
    open through QEMU) and a UDP datagram must both come back.
  - Passes in debug, release and with `-cpu max`.
- **Found on the way.** CI failed in the second smoke boot, inside the
  boot loader, before the kernel ran. QEMU's `vvfat` (the ESP is a host
  directory, mounted read-write because IDE refuses read-only) writes the
  guest's changes back to that directory. The firmware saved its
  variables there (`NvVars`), and the write-back rewrote files including
  `BOOTX64.EFI`. xtask now rebuilds the ESP before every boot.
- **Not in the smoke test:** connection refused against the host. QEMU's
  user network on Windows does not pass a host RST on to the guest, so
  that step's outcome depends on the platform; the unit test covers
  `Refused`.

## Checklist (master spec §48)

- **Purpose:** connections and names.
- **API:** socket ops 6–12, new statuses, `TcpStream`, `TcpListener`,
  `resolve`.
- **Dependencies:** none external.
- **Security:**
  - all segments and DNS messages are untrusted and validated;
  - RFC 5961 checks and keyed ISNs;
  - bounded buffers, backlog and socket table.
- **Failure behaviour:**
  - refusal, reset and timeouts are distinct errors;
  - an unresolvable or unanswered name is an error, never a guess.
