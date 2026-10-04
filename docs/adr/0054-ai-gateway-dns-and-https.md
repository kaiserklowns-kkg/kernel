# ADR-0054: The AI model gateway: DNS and https

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0024 (TCP and DNS), ADR-0031 (TLS), ADR-0043 (IPv6),
  ADR-0050 (Go on Oceans), ADR-0051 (the AI runtime)

## Context

ADR-0051's model gateway reaches a model server only at
`http://A.B.C.D:PORT/path`:
- **no names:** the Go side had no resolver;
- **no TLS:** hosted providers, and most servers outside the local
  network, are https-only.

That limits the AI to a model server on the local network, typed by
address. The gateway must reach real model servers. It is also the first
Go program on Oceans that needs:
- **UDP:** the Go binding had TCP only (`go/oceans/tcp`);
- **a trust store:** on `wasip1` there are no system roots, and Go's
  `crypto/x509` finds none;
- **a CA the user adds** (a private model server), although the AI
  service has no filesystem, by design (ADR-0051).

## Decision

### DNS in Go (`go/oceans/udp`, `go/oceans/dns`)

**`go/oceans/udp`** is a UDP socket over the net-proto socket protocol,
with the same notification pattern as `go/oceans/tcp`:
- `UDP_OPEN` with a notification the stack signals when a datagram waits;
- `SEND_TO` (IPv4) and `SEND_TO6` (IPv6);
- `RECV6` for both families (IPv4 senders come back IPv4-mapped and are
  unmapped);
- `Receive(timeout)` sleeps on the notification with a timer.

**`go/oceans/netproto`** holds what the Go clients share:
- operations and statuses;
- `INFO`/`INFO6` decoding;
- the wait loop.

`tcp` now uses it, and gains:
- **`DialAddr`:** IPv6 connects (`TCP_CONNECT6`), so AAAA answers are
  usable;
- **deadlines** (`SetDeadline`, `SetReadDeadline`, `SetWriteDeadline`),
  which `crypto/tls` needs of a connection.

**`go/oceans/dns`** mirrors `libs/dns` and the resolver of
`user/net-proto`:
- **Messages:** recursive A and AAAA queries; strict response parsing:
  - the id, the question (name, type, class), QR and the opcode must
    match;
  - every length, label and pointer is bounds-checked;
  - compression pointers must point backwards, at most 16 per name;
  - names are at most 253 characters;
  - a label holding a dot is malformed;
  - TC is an error (it would need TCP);
  - CNAME chains are followed from the asked name, one step per record,
    so a loop ends.
- **The resolver:**
  - asks the server the user named, or the one the network service
    reports: DHCP's (`INFO`), else a router's RDNSS (`INFO6`);
  - a random id per query (`crypto/rand`, kernel entropy);
  - 3 tries of 1.5 s per query;
  - datagrams from another address or port, or that do not parse as the
    answer, are ignored, not trusted;
  - an answer cut to the socket protocol's 236 bytes fails at once
    (`the DNS answer is too large`), rather than waiting for a timeout;
  - asks for A and/or AAAA as the stack can use them, and orders the
    addresses as `Addresses::ordered` does (IPv6 first only with a global
    IPv6 address).

### The endpoint (`go/ai/httpc.ParseEndpoint`)

`http[s]://HOST[:PORT][/PATH]`, where HOST is:
- a DNS name;
- an IPv4 address (anything made only of digits and dots must be one, so
  `1.2.3` is refused, not looked up);
- a bracketed IPv6 address.

Default ports are 80 and 443. Refused, since a model server's URL has no
use for them and they could hide another destination:
- user information (`@`), queries, fragments;
- zones, IPv4-mapped IPv6 literals, percent-escapes;
- any path character outside RFC 3986's plain `pchar` set;
- ports that are not 1–65535 in plain digits.

### https (`go/ai/gateway`)

TLS is Go's own `crypto/tls`, over the Oceans TCP connection wrapped as a
`net.Conn`:
- TLS 1.2 or 1.3;
- the certificate is verified for the URL's host (a name, or an IP
  address SAN for a literal), at the system's real-time clock;
- the handshake has 30 s, and each later read or write waits at most
  120 s for the server;
- addresses are tried in order until one connects.

Each handshake is logged with its version, suite, key exchange and time
(`ai: model server: TLS 1.3 with … in N ms (…)`).

Using the Rust TLS stack (ADR-0031) from Go would need either a TLS
service, or host functions that move bytes through the Go host's TLS.
`crypto/tls` is already in the standard library, maintained upstream, and
runs inside the WebAssembly sandbox; its cost is measured below.

### The trust store: a boot module

The roots are the system's trust store, given like any other authority:
- **The bundle:** `cargo xtask` writes **`ca-roots.pem`** into the boot
  archive: Mozilla's roots, the same set the Rust TLS stack trusts
  (`webpki-roots`).
  - Go needs whole certificates, not trust anchors, so the PEM comes
    from `webpki-root-certs`, the companion crate of the same release.
  - The build checks that the two agree (same count; every anchor's key
    in a certificate) and refuses to build otherwise.
- **The grant:** init grants it to the `ai` service as
  `grant = module:ca-roots.pem`, after the program (the Go host runs the
  first module).
- **Reading it:** the Go host gains two host functions, **`memory_size`**
  and **`memory_read`**:
  - they read a memory object the process holds (`READ` and `MAP`
    rights), mapped read-only for the copy only;
  - in Go they are `oceans.MemorySize`, `oceans.MemoryRead` and
    `oceans.ReadMemory`.
- **When:** it is parsed when https is first configured and cached, so
  a system without an https model pays nothing.

### Adding a CA: `ai model … --ca PATH`

`ai model URL MODEL [--dns SERVER[:PORT]] [--ca PATH]`:
- **`--ca`:** the AI service has no filesystem. The shell, which has
  one, reads the file into a memory object. It sends the service a copy
  with **`READ | MAP | TRANSFER` only**, attached to `CONFIGURE`.
  - The service trusts those certificates **for this server only**,
    besides the system's roots: the pool is a copy.
  - It closes the object after reading it.
  - The user names the file in the command, so the authority comes from
    the user, at the moment it is needed, and the service never holds a
    filesystem capability.
- **`--dns`:** a DNS server for this gateway, for a split-horizon or
  local resolver. Without it, names go to the network's DNS server.

`CONFIGURE` is now `URL MODEL [--dns SERVER]`, with handles = `[CA]`
optional. Everything is checked before anything changes:
- the URL, and the model name (printable ASCII, at most 128 bytes);
- the DNS server;
- that a CA comes with `https://` only;
- that every PEM block is a certificate that parses, and nothing is cut
  off.

### Testing

**Go unit tests (host):**
- **DNS messages:** queries; A, AAAA, CNAME chains and loops; NXDOMAIN,
  SERVFAIL and empty answers; 17 kinds of malformed or foreign response;
  pointer chain bounds; a fuzz target.
- **The resolver** (fake socket): retries, timeouts, forged answers from
  other senders or with other ids, truncation, family ordering, literals.
- **INFO/INFO6 and RECV6 decoding.**
- **`ParseEndpoint`:** 8 good URLs and 30 bad ones.
- **The settings and PEM bundles.**
- **The transport, against a `crypto/tls` server on loopback:**
  - a successful POST over TLS 1.3, with its trace;
  - an unknown issuer, another name, no roots, a server that is not TLS,
    and an expired certificate, all refused;
  - unresolved names, unreachable addresses.

**xtask unit tests:** base64, and that the roots bundle holds every root.

**Smoke:**
- The host's DNS server answers **`models.oceans.test` → 10.0.2.2**.
- An **https model server** runs on the host:
  - the HTTP test server's answers (the scripted model) behind the
    in-tree TLS stack's server side (`oceans-tls`, its `server` feature,
    as its own tests use it);
  - a certificate for `models.oceans.test` from a test CA
    (`tools/xtask/testdata`, regenerated by its `generate.sh`; public
    test keys);
  - `openssl s_server -WWW` serves files only, and the model needs POST;
    an in-process server needs nothing installed;
  - over https, the scripted model says "Memory over https", so the
    smoke test can tell the answers apart.
- **The guest:**
  1. fetches the test CA;
  2. configures `https://models.oceans.test:PORT/v1 --dns 10.0.2.2:PORT`
     **without** the CA: the question fails with `x509: certificate
     signed by unknown authority`;
  3. configures it again with `--ca /keep/models-ca.pem`: the question
     is answered over TLS 1.3, by name.
- ADR-0051's plain-http steps are unchanged.

### Measurements (§51)

The handshake runs in interpreted Go (wasmi, soft float). Measured in the
smoke test (QEMU TCG on a loaded host; recorded in
docs/development/benchmarks.md):

| What | Release | Debug kernel | `-cpu max` |
|---|---|---|---|
| TLS 1.3 handshake (ECDSA P-256 certificate, X25519; the client also offers X25519MLKEM768) | 1 140–1 240 ms | 1 180–1 260 ms | 1 150–1 240 ms |
| Reading and parsing the 121 roots (once) | — | 2 480 ms | 1 860 ms |

The handshake is Go code interpreted, so the kernel's profile barely
matters. About 1.2 s per request is small next to a model's answer, but
it is paid on every step of the agent loop. Connection reuse or session
resumption is the first thing to do if it matters (see below).

### What `crypto/tls` needed of the Go host and the runtime

- **Heap blocks without a table.** `ai.wasm` grew from 3.6 MB to 8.4 MB
  (`crypto/tls`, `net`), 4 689 functions, 594 of them over 2 KiB.
  - wasmi keeps each function body until it is first called, and the
    process heap gave every allocation over 2 KiB (and every slab) a
    page block of its own.
  - Live blocks were tracked in a table of 1 024, each holding a memory
    object handle. The table ran out, and the Go host panicked at start.
  - `oceans-rt`'s heap now closes each block's handle as soon as it is
    mapped: the mapping keeps the object (the kernel drops it when
    unmapped), so freeing is unmapping.
  - Live blocks are now bounded by memory alone, not by a table or the
    process's handle limit. Addresses are never reused; the 16 TiB
    region outlasts any process.
- **WASI imports.** Go's `os` package, pulled in by `net`, imports
  `fd_readdir`, `path_filestat_get` and `path_readlink`. The Go host
  answers them like the other file calls (`EBADF`, `ENOTSUP`): there are
  still no files through WASI.

## Consequences

- **The AI reaches real model servers:** a hosted provider over
  `https://` by name, a local server by name or address, over IPv4 or
  IPv6, with a private CA when needed.
- **Go programs on Oceans have UDP, DNS and TLS**, all inside the
  WebAssembly sandbox, through the same capabilities as Rust programs.
- **The trust store is system data:** updating Mozilla's roots is a
  dependency update (`webpki-roots` and `webpki-root-certs` together),
  checked at build time.
- **Cost:**
  - `ai.wasm` grows by `crypto/tls`;
  - the first https configuration parses the roots;
  - every request pays a full handshake: one connection per request, as
    in ADR-0051.
- **Not yet:**
  - **Connection reuse or TLS session resumption:** each step of the
    agent loop pays a full handshake.
  - **DNS over TCP:** answers over 236 bytes fail with a clear error.
  - **API keys** for hosted providers: an `Authorization` header needs a
    secret store first, and keys must never pass through the model or
    the activity log.
  - **Persisting the CA** with the rest of the model setting: the CA
    comes with each `ai model`.

## Alternatives considered

- **TLS through the Rust stack** (a TLS service, or host functions): one
  TLS implementation instead of two, but a new service or privileged host
  code on the data path, for a sandboxed program that has `crypto/tls`
  already. If measurements make interpreted handshakes too slow, a host
  function for the expensive primitives is the next step, not a second
  protocol.
- **Embedding the roots in `ai.wasm`** (`go:embed`): every root update
  would rebuild every Go program that uses TLS, and the trust store would
  not be visible system data.
- **Synthesising certificates from the trust anchors** (`webpki-roots`
  only): Go would accept them, since roots' signatures are not checked,
  but they would be invented certificates. The companion crate carries
  the real ones.
- **Giving the AI service read access to a CA directory:** standing
  filesystem authority for one file. `--ca` hands over exactly the file
  the user names, read-only.
- **A test CA built into smoke images only:** it would not help a user
  with a private model server, and it would make test images trust what
  real ones do not.
- **The network service's DNS server only:** in QEMU's user network
  that is QEMU's forwarder to the host's resolver, which cannot know
  test names. `--dns` is also what a split-horizon setup needs.

## Checklist (master spec §48)

- **Purpose:** the model gateway reaches model servers by name and over
  https.
- **Architecture:**
  - `go/oceans/udp`, `go/oceans/netproto`, `go/oceans/dns`;
  - IPv6 and deadlines in `go/oceans/tcp`;
  - `go/ai/gateway` (`crypto/tls`, trust, transport);
  - `memory_size`/`memory_read` and three more WASI stubs in the Go
    host;
  - table-free heap blocks in `oceans-rt`;
  - `ca-roots.pem` in the boot archive.
- **API:**
  - `CONFIGURE` `URL MODEL [--dns SERVER]` + optional CA handle;
  - the shell's `ai model … [--dns SERVER] [--ca PATH]`;
  - the Go packages above.
- **Dependencies:**
  - Go's standard library (`crypto/tls`, `crypto/x509`, `net/netip`);
  - for xtask: `webpki-root-certs` (CDLA-Permissive-2.0, the same data
    as `webpki-roots`), and the in-tree TLS stack's `server` feature with
    rustls's std I/O for the test server.
- **Security:**
  - responses to DNS queries are untrusted input, parsed strictly, from
    the server asked only, with random ids;
  - certificates are verified against Mozilla's roots, plus a CA the user
    names for one server;
  - the service never gets filesystem access: the CA comes read-only
    from the requester;
  - URLs that could hide another destination are refused.
- **Testing:** Go unit tests (DNS, resolver, endpoint, settings, PEM,
  TLS transport); xtask unit tests; smoke (name resolution, an untrusted
  certificate refused, https with an added CA).
- **Failure behaviour:**
  - each failure names its cause: DNS (not found, no address, no
    answer, too large, no server), TLS (unknown authority, wrong name,
    expired), or connecting (each address tried, and why it failed);
  - a refused `CONFIGURE` leaves the previous model setting in place.
