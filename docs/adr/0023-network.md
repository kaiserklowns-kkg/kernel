# ADR-0023: Networking: virtio-net, the IPv4 stack and sockets

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0013 (IPC), ADR-0021 (device capabilities)
- Adds: ABI v8 (syscalls 35–37: bound notifications, timers, clock)

## Context

A network stack must do two things at once:

- react to the network: frames arriving, retransmission timers;
- serve the programs using it.

Oceans processes have one thread (ADR-0014), and the only blocking points
were "wait for a call" and "wait for a notification", never both. A
network driver has the same problem with its device interrupt. Polling
would waste the CPU and add latency, so the kernel needs a small, general
addition first.

## Decision

### Kernel: events and time (ABI v8)

- **`ENDPOINT_BIND(server, notification)`**, the seL4 bound-notification
  pattern. A notification bound to an endpoint also wakes that endpoint's
  receiver. `IPC_RECEIVE_MSG` then returns `EVENT_NOTIFICATION` with the
  bits.
  - It needs `RECEIVE` on the endpoint and `WAIT` on the notification.
  - A notification binds to at most one endpoint; a second binding gives
    `Busy`.
  - Calls are still delivered first, and the old `IPC_RECEIVE` never
    consumes notification bits.
  - Lock order: endpoint, then notification. Signalling releases the
    notification lock before waking the endpoint, so the two orders never
    meet.
- **`TIMER_SET(notification, bits, ms)`**: a one-shot timer that signals
  the notification. Setting it again replaces the pending timer; 0 cancels
  it. Timers hold the notification weakly, so they never keep kernel
  memory alive past the capabilities. Expiry runs in the tick interrupt;
  an atomic "next deadline" makes idle ticks cost one comparison.
- **`CLOCK()`**: milliseconds since boot, with the tick's 10 ms resolution.
  It needs no capability: monotonic time grants no authority over
  anything.

### Shared virtio code (`user/virtio`)

- The PCI transport, feature negotiation and split virtqueues are one
  crate, `oceans-virtio`. Free descriptor lists are bounds-checked against
  what the device reports.
- virtio-blk (ADR-0021) now uses it too, and drops its own copy of the
  transport.

### virtio-net (`user/virtio-net`): frames only

- A service with one device capability (`grant = device:1af4:1041`) that
  serves `netdev`.
- Receive: 32 DMA receive buffers stay posted. Completed ones wait until
  the stack asks, then are re-posted. A slow stack therefore makes the
  device drop frames; the driver never queues without bound.
- Transmit: 32 DMA transmit buffers, reclaimed on completion.
- MSI-X goes to a notification bound to its own endpoint.
- **`netdev` protocol:** one session at a time, a shared 64 KiB buffer
  split into a receive area and a transmit area, `SEND` / `RECV` calls,
  and a "frames waiting" signal. The driver signals the stack and never
  calls it, so neither can block the other.

### The stack (`libs/net`, `oceans-net`): a pure state machine

- Covers Ethernet II, ARP, IPv4, ICMP echo, UDP and a DHCP client.
- Frames and time go in, frames and socket events come out. There is no
  I/O inside, so all of it is tested on the host.
- **Hardening:**
  - every header is length-checked before use;
  - IP, ICMP and UDP checksums are verified;
  - fragments, IP options and unknown protocols are dropped;
  - queues are bounded: 64 frames out, 32 datagrams per socket, 16 ARP
    entries, 16 packets waiting on ARP.
- **DHCP:** DISCOVER → OFFER → REQUEST → ACK, retransmitting with backoff
  (2, 4, 8, 16 s), renewing at half the lease, and starting over on NAK
  or expiry.
- **ARP:** requests retried 3 times, 1 s apart, after which the waiting
  packets are dropped. Entries live 60 s.
- **Errors:** UDP to a closed port gets ICMP port unreachable, except for
  broadcasts.

### The `net` service and sockets

- `use = netdev`, `provide = net`. One notification, bound to its
  endpoint, carries both "frames waiting" and the stack's timer: the
  timer is set to the stack's next deadline after every event.
- **Sockets are capabilities.**
  - `UDP_OPEN` (port 0 means ephemeral) and `PING_OPEN` return a badged
    handle.
  - The program passes a notification when opening, and the service
    signals it when the socket becomes readable. Programs combine that
    with their own timers (`ping` times out this way).
  - `SEND_TO` / `RECV` carry at most 240 bytes inline. Larger receives
    come back marked as truncated.
- **Programs:**
  - `ifconfig` and `ping ADDRESS [COUNT]`. Their manifests ask for
    `use:net`, which is not low-risk, so they are run explicitly:
    `run ping out use:net -- 10.0.2.2`.
  - `udp-echo` (RFC 862, port 7) runs as a service in the smoke manifest
    for end-to-end testing.

### Limits raised

- Boot modules kept by the kernel: 16 → 27. That is all that init can
  receive (32 initial handles, 5 fixed). Adding programs had silently
  pushed `ipc-test` past the old limit; the smoke test caught it.
- Grants per service in init: 12 → 16.

## Consequences

- Single-threaded services can now wait on calls, interrupts and time
  together. Future drivers and services get this for free.
- **No TCP yet.** That is the next step, and it needs bulk data (shared
  buffers per connection) rather than inline IPC. No DNS client, no IPv6
  and no routing beyond one gateway either.
- Any holder of `net` may bind any UDP port, including ports below 1024.
  Finer authority (per-port grants) can be minted from the socket
  capability model when services need it.
- Without an IOMMU the network driver is trusted, like the disk driver
  (ADR-0021).
- One boot module per program does not scale past 27. An archive (initrd)
  module is the planned replacement.

## Alternatives considered

- **Threads in services.** That means a much larger kernel change (TLS,
  synchronization in every service), for a problem a bound notification
  solves.
- **Polling with sleeps.** Simpler, but it adds latency and burns CPU.
- **The stack in the driver, or in the kernel.** Mixing them would make
  every NIC driver contain a stack, and would put untrusted packet parsing
  in the kernel.
- **smoltcp.** A capable stack, but an external dependency with its own
  device and socket model. A small, host-tested stack sized to what
  Oceans uses now keeps the code reviewable; TCP will show whether that
  still holds.

## Testing

- **`cargo test -p oceans-net` (12 tests):**
  - the RFC 1071 checksum vector;
  - the full DHCP exchange, including foreign and wrong-xid replies
    ignored, renewal and NAK;
  - DHCP retransmission backoff;
  - ARP replies only for our address;
  - UDP waiting on ARP, then sent to the resolved MAC with valid
    checksums;
  - gateway routing and `NoRoute`;
  - ARP give-up;
  - UDP delivery and port unreachable (not for broadcasts);
  - echo replies, and ping sockets matched by identifier;
  - malformed frames dropped, plus every single-byte mutation of real
    traffic handled without panic;
  - port and socket limits, and queue bounds;
  - address parsing.
- **Kernel self-tests** (smoke boots):
  - a bound notification wakes the receiver;
  - calls and signals arrive in order (`412`);
  - a second binding is `Busy`;
  - a timer fires after its delay, not before;
  - re-setting a timer replaces it, and cancelling works.
- **Smoke test** (QEMU user network, a virtio-net device with a fixed MAC,
  UDP port forwarding):
  - the driver reports MSI-X;
  - DHCP configures `10.0.2.15/24` with gateway and DNS;
  - bare `ifconfig` is refused (it asks for `use:net`);
  - `ifconfig` shows the configuration and MAC;
  - `ping 10.0.2.2 2` gets two replies;
  - `ping 10.0.2.99 1` times out;
  - **from the host, xtask sends UDP datagrams into the guest's echo
    service and requires the echo back**, in both boots. Packets therefore
    cross the host, QEMU, the driver, the stack and a socket in both
    directions.
- The virtio-blk refactor is covered by the existing disk smoke steps.
- Passes in debug, release and with `-cpu max`.

## Checklist (master spec §48)

- **Purpose:** networking.
- **API:** ABI v8; the `netdev` and socket protocols (`oceans-net-proto`).
- **Dependencies:** none external.
- **Security:**
  - packets are parsed in userspace;
  - the driver holds one device and the stack holds no device;
  - sockets are capabilities, and network use is an explicit grant;
  - all queues are bounded.
- **Failure behaviour:**
  - no NIC: the `net` service reports `NoDevice`;
  - no DHCP: retries forever with backoff;
  - unresolvable hosts: packets are dropped after the ARP retries;
  - driver queues full: frames are dropped, as on hardware.
