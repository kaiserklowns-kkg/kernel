# ADR-0026: Kernel entropy and the `RANDOM` system call

- Status: Accepted
- Date: 2026-10-03
- Adds: ABI v9 (syscall 38)

## Context

TCP initial sequence numbers (ADR-0024), DNS query ids, and soon TLS keys
and package nonces all need unpredictable numbers. A process has no good
source of its own: it sees only a clock and a TSC. The kernel sees more,
including interrupt timing and the CPU's random-number instructions, so
randomness belongs there.

## Decision

- **Primitives (`libs/random`, `oceans-random`, host-tested):**
  - BLAKE2s-256 (RFC 7693) as the mixing function;
  - a ChaCha20 (RFC 8439) generator with **fast key erasure**: each
    request also produces the next key, so captured state never reveals
    past output.
- **Kernel sources, all hashed together:**
  - RDSEED, else RDRAND, when the CPU has them;
  - timing jitter of a memory-dependent loop at boot (1024 samples);
  - the TSC at every timer tick, device interrupt (MSI-X) and console
    byte, kept in a 64-entry ring that interrupts update with one atomic
    store.
- **Each request reseeds** from the ring, the TSC and, if available, the
  hardware, then generates. Hashing means one good source suffices, and a
  bad or hostile one (e.g. a broken RDRAND) cannot cancel the others.
- The boot log says which sources were found. QEMU's default CPU has no
  RDRAND, and the kernel says so instead of pretending.
- **`RANDOM(ptr, len)`, up to 256 bytes per call, needs no capability.**
  Randomness grants no authority, like `CLOCK`. `oceans_rt::random` and
  `random_u64` wrap it.
- **Users now:** the `net` service keys TCP ISNs with it, and the DNS
  resolver draws query ids from it.

## Consequences

- Programs get secure randomness with one call; TLS can build on it.
- Jitter under emulation is weaker than on hardware. On Tier 1 hardware
  RDSEED/RDRAND is present, and its absence is logged.
- No blocking "not yet seeded" state: the boot seed comes from jitter
  before any user process exists.

## Testing

- `cargo test -p oceans-random`:
  - BLAKE2s reference vectors (`""`, `"abc"`) and block-boundary splits;
  - ChaCha20 RFC 8439 quarter-round and block vectors;
  - the generator is deterministic for a seed but depends on it, never
    repeats across requests, erases its key, and keeps old state on
    reseed;
  - a byte-frequency uniformity check.
- **Kernel self-test** (smoke): two requests differ and are not trivially
  patterned.
- **Smoke test, both ways:**
  - QEMU's default CPU logs the jitter-only warning;
  - `-cpu max` logs CPU random numbers.
  - The network steps (TCP, DNS) run with the new ISN secret and query
    ids.
